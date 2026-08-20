//! What a command is going to do before it is sent.
//!
//! Postgres tells you: a statement either returns rows or reports a count, and
//! the server enforces `default_transaction_read_only` whatever the client
//! thinks. Redis has no read-only session, so a connection marked read-only in
//! tupli is only read-only if this file is right about which commands write.
//!
//! Two consequences shape the table below. Anything unrecognised counts as a
//! write, because the cost of that being wrong is a refused read, whereas the
//! cost of the opposite is a `FLUSHALL` on production. And the read list is
//! written out by hand rather than derived from `COMMAND INFO`, because the
//! answer has to exist before the connection does — the connection sheet asks
//! whether a saved connection is safe long before anyone dials it.

/// What a command does, as far as the safety rules care.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Reads, and the harmless bookkeeping ones: `PING`, `SELECT`, `INFO`.
    Read,
    /// Changes data. Allowed unless the connection is read-only; worth a
    /// confirmation on a connection marked as needing one.
    Write,
    /// Changes data wholesale, blocks the server, or reconfigures it —
    /// `FLUSHALL`, `KEYS`, `SHUTDOWN`, `CONFIG SET`. Always worth stopping to
    /// ask about, whatever the connection's safety level.
    Dangerous,
    /// Waits for something that may never happen. Refused unconditionally:
    /// these hold the connection open with no reply, and this crate's
    /// connection is multiplexed, so one `BLPOP` would wedge every pane
    /// sharing it. A pub/sub or monitoring pane would need its own connection.
    Blocking,
}

impl Kind {
    /// Whether a read-only connection may send this.
    pub fn is_read(self) -> bool {
        self == Self::Read
    }

    /// Whether the user should be asked first, on a connection that asks.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::Write | Self::Dangerous)
    }
}

/// Commands that only read. Everything not here is assumed to write.
const READS: &[&str] = &[
    // Keyspace.
    "DBSIZE", "DUMP", "EXISTS", "EXPIRETIME", "KEYCOUNT", "OBJECT", "PEXPIRETIME", "PTTL",
    "RANDOMKEY", "SCAN", "TOUCH", "TTL", "TYPE",
    // Strings.
    "BITCOUNT", "BITPOS", "GET", "GETBIT", "GETRANGE", "LCS", "MGET", "STRLEN", "SUBSTR",
    // Hashes.
    "HEXISTS", "HGET", "HGETALL", "HGETEX", "HKEYS", "HLEN", "HMGET", "HRANDFIELD", "HSCAN",
    "HSTRLEN", "HTTL", "HVALS",
    // Lists.
    "LINDEX", "LLEN", "LPOS", "LRANGE", "SORT_RO",
    // Sets.
    "SCARD", "SDIFF", "SINTER", "SINTERCARD", "SISMEMBER", "SMEMBERS", "SMISMEMBER",
    "SRANDMEMBER", "SSCAN", "SUNION",
    // Sorted sets.
    "ZCARD", "ZCOUNT", "ZDIFF", "ZINTER", "ZINTERCARD", "ZLEXCOUNT", "ZMSCORE", "ZRANDMEMBER",
    "ZRANGE", "ZRANGEBYLEX", "ZRANGEBYSCORE", "ZRANK", "ZREVRANGE", "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE", "ZREVRANK", "ZSCAN", "ZSCORE", "ZUNION",
    // Streams.
    "XINFO", "XLEN", "XPENDING", "XRANGE", "XREAD", "XREVRANGE",
    // Geo, HyperLogLog. `PFCOUNT` is missing on purpose: it can rewrite the
    // key it is counting.
    "GEODIST", "GEOHASH", "GEOPOS", "GEOSEARCH",
    // The session and the server.
    "COMMAND", "DBSIZE", "ECHO", "EXPLAIN", "HELLO", "INFO", "LASTSAVE", "LOLWUT", "PING",
    "SELECT", "TIME", "WAITAOF",
];

/// Commands worth stopping for, whatever the connection's safety level says.
const DANGEROUS: &[&str] = &[
    // Deletes everything, with no undo and no confirmation of its own.
    "FLUSHALL", "FLUSHDB", "SWAPDB",
    // Walks the entire keyspace with the server single-threaded meanwhile.
    // The key browser never sends it; a person typing it into the console
    // should be told what it costs.
    "KEYS",
    // Stops, reconfigures, or re-points the server.
    "SHUTDOWN", "REPLICAOF", "SLAVEOF", "FAILOVER", "MIGRATE", "RESET", "DEBUG",
    // Rewrites what will be there after a restart.
    "BGREWRITEAOF", "BGSAVE", "SAVE",
];

/// Commands that wait, and so must never go down a multiplexed connection.
const BLOCKING: &[&str] = &[
    "BLMOVE", "BLMPOP", "BLPOP", "BRPOP", "BRPOPLPUSH", "BZMPOP", "BZPOPMAX", "BZPOPMIN",
    "MONITOR", "PSUBSCRIBE", "PSYNC", "SSUBSCRIBE", "SUBSCRIBE", "SYNC", "WAIT",
];

/// Container commands whose subcommand decides the answer, because `CONFIG
/// GET` and `CONFIG SET` are not the same kind of thing at all.
const CONTAINERS: &[(&str, &[&str], Kind)] = &[
    ("ACL", &["CAT", "GETUSER", "LIST", "USERS", "WHOAMI"], Kind::Read),
    ("ACL", &["DELUSER", "SETUSER", "LOAD", "SAVE"], Kind::Dangerous),
    ("CLIENT", &["ID", "INFO", "GETNAME", "LIST", "NO-EVICT", "NO-TOUCH"], Kind::Read),
    ("CLIENT", &["KILL", "PAUSE", "UNPAUSE"], Kind::Dangerous),
    ("CLUSTER", &["COUNTKEYSINSLOT", "INFO", "MYID", "NODES", "SHARDS", "SLOTS"], Kind::Read),
    ("CLUSTER", &["FORGET", "FLUSHSLOTS", "RESET", "FAILOVER"], Kind::Dangerous),
    ("CONFIG", &["GET"], Kind::Read),
    ("CONFIG", &["SET", "RESETSTAT", "REWRITE"], Kind::Dangerous),
    ("FUNCTION", &["DUMP", "LIST", "STATS"], Kind::Read),
    ("FUNCTION", &["FLUSH"], Kind::Dangerous),
    ("LATENCY", &["DOCTOR", "HISTORY", "LATEST"], Kind::Read),
    ("MEMORY", &["DOCTOR", "STATS", "USAGE"], Kind::Read),
    ("MEMORY", &["PURGE"], Kind::Dangerous),
    ("SCRIPT", &["EXISTS"], Kind::Read),
    ("SCRIPT", &["FLUSH"], Kind::Dangerous),
    ("SLOWLOG", &["GET", "LEN", "HELP"], Kind::Read),
];

/// What sending this command line would do.
///
/// `args` is the whole command, name first, as [`crate::split_args`] produced
/// it. The subcommand matters for a handful of names, so this takes the line
/// rather than the name.
pub fn classify(args: &[Vec<u8>]) -> Kind {
    let Some(name) = name_of(args) else {
        return Kind::Read;
    };
    let subcommand = args.get(1).and_then(|arg| word(arg));

    // A blocking form of an otherwise ordinary command: `XREAD BLOCK 0 …`
    // waits exactly as long as `BLPOP` does.
    if matches!(name.as_str(), "XREAD" | "XREADGROUP")
        && args.iter().skip(1).any(|arg| word(arg).as_deref() == Some("BLOCK"))
    {
        return Kind::Blocking;
    }
    if BLOCKING.contains(&name.as_str()) {
        return Kind::Blocking;
    }
    if DANGEROUS.contains(&name.as_str()) {
        return Kind::Dangerous;
    }
    if let Some(subcommand) = &subcommand {
        for (container, subcommands, kind) in CONTAINERS {
            if *container == name && subcommands.contains(&subcommand.as_str()) {
                return *kind;
            }
        }
        // A container command this table does not list a subcommand for. It
        // is not a read, and the unknown-writes rule applies.
        if CONTAINERS.iter().any(|(container, ..)| *container == name) {
            return Kind::Write;
        }
    }
    if READS.contains(&name.as_str()) {
        return Kind::Read;
    }
    Kind::Write
}

/// The command's name, upper-cased. `None` for an empty line.
pub fn name_of(args: &[Vec<u8>]) -> Option<String> {
    args.first().and_then(|arg| word(arg))
}

/// One argument as an upper-case word, if it is text at all. A binary
/// argument is certainly not a command name.
fn word(arg: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(arg).ok()?;
    (!text.is_empty()).then(|| text.to_ascii_uppercase())
}

/// The whole command line, for an error message or a log line.
///
/// Only the name and — where the table cares about it — the subcommand. The
/// arguments are the user's data: a key name, a value, and on `AUTH` a
/// password. None of that belongs anywhere this string might end up.
pub fn describe(args: &[Vec<u8>]) -> String {
    let Some(name) = name_of(args) else {
        return String::new();
    };
    let container = CONTAINERS.iter().any(|(container, ..)| *container == name);
    match container.then(|| args.get(1).and_then(|arg| word(arg))).flatten() {
        Some(subcommand) => format!("{name} {subcommand}"),
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(line: &str) -> Kind {
        classify(&crate::split_args(line).unwrap())
    }

    #[test]
    fn reads_are_reads_whatever_their_case() {
        assert_eq!(kind("get some:key"), Kind::Read);
        assert_eq!(kind("HGETALL h"), Kind::Read);
        assert_eq!(kind("scan 0 match a:* count 100"), Kind::Read);
    }

    #[test]
    fn writes_are_writes() {
        assert_eq!(kind("SET k v"), Kind::Write);
        assert_eq!(kind("DEL k"), Kind::Write);
        assert_eq!(kind("EXPIRE k 60"), Kind::Write);
    }

    #[test]
    fn an_unknown_command_is_treated_as_a_write() {
        // A module command, a typo, a command added after this was written.
        // Refusing a read on a read-only connection is a nuisance; allowing a
        // write on one is a bug with consequences.
        assert_eq!(kind("JSON.SET k $ 1"), Kind::Write);
        assert_eq!(kind("SOMETHINGENTIRELYNEW a b"), Kind::Write);
    }

    #[test]
    fn the_ones_that_ruin_an_afternoon_are_called_out() {
        assert_eq!(kind("FLUSHALL"), Kind::Dangerous);
        assert_eq!(kind("keys *"), Kind::Dangerous);
        assert_eq!(kind("SHUTDOWN NOSAVE"), Kind::Dangerous);
        assert!(Kind::Dangerous.needs_confirmation());
        assert!(!Kind::Dangerous.is_read());
    }

    #[test]
    fn a_subcommand_can_change_the_answer() {
        assert_eq!(kind("CONFIG GET maxmemory"), Kind::Read);
        assert_eq!(kind("config set maxmemory 0"), Kind::Dangerous);
        assert_eq!(kind("CLIENT LIST"), Kind::Read);
        assert_eq!(kind("CLIENT KILL ID 4"), Kind::Dangerous);
        assert_eq!(kind("SLOWLOG GET 10"), Kind::Read);
        assert_eq!(kind("SLOWLOG RESET"), Kind::Write);
    }

    #[test]
    fn anything_that_waits_is_refused_outright() {
        assert_eq!(kind("BLPOP q 0"), Kind::Blocking);
        assert_eq!(kind("SUBSCRIBE channel"), Kind::Blocking);
        // The same command is fine or fatal depending on one argument.
        assert_eq!(kind("XREAD COUNT 10 STREAMS s 0"), Kind::Read);
        assert_eq!(kind("XREAD BLOCK 0 STREAMS s $"), Kind::Blocking);
    }

    #[test]
    fn a_described_command_carries_no_arguments() {
        // `AUTH` is the reason this is not just the line the user typed.
        let args = crate::split_args("AUTH default hunter2").unwrap();
        assert_eq!(describe(&args), "AUTH");
        assert_eq!(describe(&crate::split_args("config set a b").unwrap()), "CONFIG SET");
        assert_eq!(describe(&crate::split_args("get k").unwrap()), "GET");
        assert!(describe(&[]).is_empty());
    }
}
