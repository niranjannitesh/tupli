//! Walking the keyspace without stopping the server.
//!
//! `KEYS *` is the obvious way to list keys and it is an outage: Redis is
//! single-threaded, so a pattern match over ten million keys is ten million
//! keys' worth of time during which nothing else is served. Every listing in
//! this crate goes through `SCAN` instead, which walks the hash table a bucket
//! at a time and hands back a cursor.
//!
//! The price is that a scan is a *sample*, not an inventory. `COUNT` is a hint
//! about work done and not about rows returned, so a page can come back empty
//! with the walk unfinished; a key added during the walk may or may not be
//! seen; a key present throughout will be. Anything built on this has to say
//! how far it got rather than claim a total — which is why [`Scan::seen`]
//! exists and why nothing here reports a key count.
//!
//! Cancelling is not a command, it is stopping: each [`Scan::page`] does a
//! bounded amount of work and returns, so a caller that stops asking has
//! cancelled. That is the whole mechanism, and it is why paging is a method on
//! a struct rather than a stream this crate drives itself.

use db::{ColumnMeta, DbResult, KeyInfo, KeyType, ResultSet, ValueKind};

use crate::client::RedisConnection;
use crate::keys::scan_reply;
use crate::resp::RespValue;
use crate::rows;

/// Buckets of work per `SCAN` call.
///
/// Redis's own default is 10, which is far too small to fill a table and costs
/// a round trip for each handful. This is a page of a browser at a time.
pub const DEFAULT_COUNT: usize = 500;

/// A walk of the keyspace, resumable and stoppable.
pub struct Scan {
    cursor: u64,
    pattern: Option<Vec<u8>>,
    kind: Option<String>,
    count: usize,
    done: bool,
    seen: usize,
    /// Whether this walk wants sizes at all. Separate from whether the server
    /// has `MEMORY USAGE` — that is [`RedisConnection::has_memory_usage`], a
    /// fact about the server that outlives any one walk, while this is a
    /// caller saying it is only drawing names.
    memory: bool,
}

impl Default for Scan {
    fn default() -> Self {
        Self::new()
    }
}

impl Scan {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            pattern: None,
            kind: None,
            count: DEFAULT_COUNT,
            done: false,
            seen: 0,
            memory: true,
        }
    }

    /// Resume a walk at a cursor an earlier page handed back.
    ///
    /// A cursor is only meaningful to the server that issued it, and a stale
    /// one is not an error — the server restarts the walk somewhere valid. So
    /// nothing here validates it; there is nothing to validate it against.
    pub fn resume(mut self, cursor: u64) -> Self {
        self.cursor = cursor;
        self
    }

    /// Where the next page starts, or `None` when the walk has finished.
    pub fn position(&self) -> Option<u64> {
        (!self.done).then_some(self.cursor)
    }

    /// Only keys matching a glob — `user:*`, `session:??`.
    ///
    /// The pattern is matched by the server, which is the only place it is
    /// cheap: filtering here would still pay for every key crossing the wire.
    pub fn matching(mut self, pattern: impl AsRef<[u8]>) -> Self {
        let pattern = pattern.as_ref();
        self.pattern = (!pattern.is_empty() && pattern != b"*").then(|| pattern.to_vec());
        self
    }

    /// Only keys of one type.
    pub fn of_type(mut self, kind: &KeyType) -> Self {
        self.kind = Some(kind.as_str().to_string());
        self
    }

    /// How much work the server does per call. Larger means fewer round trips
    /// and a longer pause on the server for each.
    pub fn count(mut self, count: usize) -> Self {
        self.count = count.clamp(1, 10_000);
        self
    }

    /// Skip `MEMORY USAGE`. It samples the value to estimate a size, so on a
    /// keyspace of large values it is the expensive part of listing.
    pub fn without_memory(mut self) -> Self {
        self.memory = false;
        self
    }

    /// Whether the walk has finished. A page can be empty without this being
    /// true — that is `SCAN` doing bounded work, not the keyspace ending.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// How many keys the walk has produced so far. The honest denominator for
    /// anything the UI wants to say about coverage.
    pub fn seen(&self) -> usize {
        self.seen
    }

    /// One page: a `SCAN` call, then one pipeline for everything the browser
    /// shows about the keys it returned.
    pub async fn page(&mut self, conn: &RedisConnection) -> DbResult<Vec<KeyInfo>> {
        if self.done {
            return Ok(Vec::new());
        }
        let found = self.walk(conn).await?;
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let described = self.describe(conn, found).await?;
        self.seen += described.len();
        Ok(described)
    }

    /// Pages until there are `budget` keys or the walk ends.
    ///
    /// The budget is a floor rather than a ceiling: a page is not split, so
    /// this stops at the first page that reaches it. It exists because one
    /// page of a filtered scan is very often empty, and a browser that showed
    /// nothing until the user pressed "more" four times would look broken.
    pub async fn take(&mut self, conn: &RedisConnection, budget: usize) -> DbResult<Vec<KeyInfo>> {
        let mut collected = Vec::new();
        while !self.done && collected.len() < budget {
            collected.extend(self.page(conn).await?);
        }
        Ok(collected)
    }

    /// The `SCAN` call itself, returning the raw keys.
    async fn walk(&mut self, conn: &RedisConnection) -> DbResult<Vec<Vec<u8>>> {
        let cursor = self.cursor.to_string();
        let count = self.count.to_string();
        let mut args = vec![
            b"SCAN".to_vec(),
            cursor.into_bytes(),
            b"COUNT".to_vec(),
            count.into_bytes(),
        ];
        if let Some(pattern) = &self.pattern {
            args.push(b"MATCH".to_vec());
            args.push(pattern.clone());
        }
        if let (Some(kind), true) = (&self.kind, conn.has_scan_type()) {
            args.push(b"TYPE".to_vec());
            args.push(kind.clone().into_bytes());
        }

        let reply = match conn.command(&args).await {
            Ok(reply) => reply,
            // `SCAN … TYPE` is a 6.0 command. An older server calls it a
            // syntax error, and the filtering moves here — the keys still all
            // cross the wire, but the browser shows the right ones.
            Err(error) if conn.has_scan_type() && self.kind.is_some() => {
                conn.without_scan_type();
                log::debug!("SCAN TYPE refused ({:?}); filtering client-side", error.class);
                return Box::pin(self.walk(conn)).await;
            }
            Err(error) => return Err(error),
        };

        let (next, page) = scan_reply(&reply);
        self.cursor = next.unwrap_or(0);
        self.done = next.is_none();
        Ok(match page {
            RespValue::Array(keys) | RespValue::Set(keys) => {
                keys.iter().filter_map(RespValue::as_bytes).collect()
            }
            _ => Vec::new(),
        })
    }

    /// Type, expiry and size for a page of keys, in one round trip.
    async fn describe(
        &mut self,
        conn: &RedisConnection,
        found: Vec<Vec<u8>>,
    ) -> DbResult<Vec<KeyInfo>> {
        let want_memory = self.memory && conn.has_memory_usage();
        let per_key = if want_memory { 3 } else { 2 };
        let batch = |memory: bool| {
            let mut batch = Vec::with_capacity(found.len() * 3);
            for key in &found {
                batch.push(vec![b"TYPE".to_vec(), key.clone()]);
                batch.push(vec![b"TTL".to_vec(), key.clone()]);
                if memory {
                    batch.push(vec![b"MEMORY".to_vec(), b"USAGE".to_vec(), key.clone()]);
                }
            }
            batch
        };
        let (replies, per_key) = match conn.pipeline(&batch(want_memory)).await {
            Ok(replies) => (replies, per_key),
            // `MEMORY USAGE` is missing before 4.0 and disabled on some
            // hosted Redises. One refusal fails the whole pipeline, so it is
            // dropped for the rest of this walk rather than retried per page.
            Err(error) if want_memory => {
                conn.without_memory_usage();
                log::debug!("MEMORY USAGE refused ({:?}); listing without sizes", error.class);
                (conn.pipeline(&batch(false)).await?, 2)
            }
            Err(error) => return Err(error),
        };

        let mut keys = Vec::with_capacity(found.len());
        for (ix, key) in found.into_iter().enumerate() {
            let at = ix * per_key;
            // A key that expired between the walk and this pipeline has no
            // type any more. Listing it would show a row that is gone by the
            // time anyone clicks it.
            let Some(kind) = replies
                .get(at)
                .and_then(|reply| reply.as_str())
                .and_then(KeyType::parse)
            else {
                continue;
            };
            if !conn.has_scan_type() {
                if let Some(wanted) = &self.kind {
                    if kind.as_str() != wanted {
                        continue;
                    }
                }
            }
            keys.push(KeyInfo {
                key: key.into(),
                kind,
                // -1 is "no expiry" and -2 is "no key"; neither is a duration.
                ttl: replies
                    .get(at + 1)
                    .and_then(RespValue::as_i64)
                    .filter(|ttl| *ttl >= 0),
                memory: replies
                    .get(at + 2)
                    .filter(|_| per_key == 3)
                    .and_then(RespValue::as_i64)
                    .and_then(|size| u64::try_from(size).ok()),
            });
        }
        Ok(keys)
    }
}

/// A listing as the grid draws it.
pub fn to_result_set(keys: &[KeyInfo]) -> ResultSet {
    let names: Vec<_> = keys.iter().map(|key| Some(&*key.key)).collect();
    ResultSet::new(vec![
        rows::bytes_column("key", &names),
        rows::text_column(
            ColumnMeta::new("type", ValueKind::Text, "string"),
            keys.iter().map(|key| Some(key.kind.as_str())),
        ),
        rows::int_column(
            ColumnMeta::new("ttl", ValueKind::Int, "seconds"),
            keys.iter().map(|key| key.ttl),
        ),
        rows::int_column(
            ColumnMeta::new("size", ValueKind::Int, "bytes"),
            keys.iter().map(|key| key.memory.map(|size| size as i64)),
        ),
    ])
}

/// A glob that matches exactly one key, whatever is in its name.
///
/// Key names routinely contain the characters `MATCH` treats as syntax —
/// `cache:[user]:1` is a perfectly ordinary key — so a pattern built by
/// pasting a name into one would match the wrong things or nothing at all.
pub fn escape_glob(key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(key.len());
    for &byte in key {
        if matches!(byte, b'*' | b'?' | b'[' | b']' | b'\\' | b'^' | b'-') {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out
}

/// The glob for everything under a prefix — what the key tree sends when a
/// folder is opened.
pub fn under(prefix: &[u8]) -> Vec<u8> {
    let mut pattern = escape_glob(prefix);
    pattern.push(b'*');
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_starts_at_the_beginning_and_is_not_finished() {
        let scan = Scan::new();
        assert!(!scan.is_done());
        assert_eq!(scan.seen(), 0);
        assert_eq!(scan.cursor, 0);
    }

    #[test]
    fn a_pattern_that_matches_everything_is_not_sent() {
        // `MATCH *` costs the server a comparison per key and excludes
        // nothing, so an empty filter box means no `MATCH` at all.
        assert!(Scan::new().matching("*").pattern.is_none());
        assert!(Scan::new().matching("").pattern.is_none());
        assert_eq!(Scan::new().matching("user:*").pattern, Some(b"user:*".to_vec()));
    }

    #[test]
    fn the_count_stays_within_reason() {
        // Zero would never advance; a million would stall the server for as
        // long as it took.
        assert_eq!(Scan::new().count(0).count, 1);
        assert_eq!(Scan::new().count(1_000_000).count, 10_000);
    }

    #[test]
    fn a_key_name_is_escaped_before_it_becomes_a_pattern() {
        assert_eq!(escape_glob(b"cache:[user]:1"), b"cache:\\[user\\]:1".to_vec());
        assert_eq!(under(b"user:"), b"user:*".to_vec());
        assert_eq!(under(b"a*b"), b"a\\*b*".to_vec());
    }

    #[test]
    fn a_listing_has_a_column_per_thing_the_browser_shows() {
        let keys = vec![
            KeyInfo {
                key: b"user:1".to_vec().into(),
                kind: KeyType::Hash,
                ttl: Some(60),
                memory: Some(120),
            },
            KeyInfo {
                key: b"queue".to_vec().into(),
                kind: KeyType::List,
                ttl: None,
                memory: None,
            },
        ];
        let rows = to_result_set(&keys);
        assert_eq!(rows.column_count(), 4);
        assert_eq!(rows.row_count(), 2);
        let mut scratch = String::new();
        assert!(matches!(
            rows.columns[2].render(1, &mut scratch),
            db::CellText::Null
        ));
    }
}
