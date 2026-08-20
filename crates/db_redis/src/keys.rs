//! Reading one key, whatever kind of key it turns out to be.
//!
//! Six data types, six shapes, one [`ResultSet`] — a list becomes index and
//! value, a hash becomes field and value, a stream becomes an id and a column
//! per field name found in the page. That is the trade this crate is here to
//! test: if the grid can draw all six without knowing anything about Redis,
//! then the driver boundary is in the right place.
//!
//! Everything is paged, because a list can hold a million elements and a value
//! pane can show a few hundred. Redis offers two ways to page and they are not
//! interchangeable: the ordered types (list, sorted set, stream) can be asked
//! for a range, and the unordered ones (hash, set) can only be walked with a
//! cursor, in an order the server picks and does not promise to keep. So
//! [`Position`] has a variant per method rather than pretending to one
//! universal offset that would quietly mean three different things.

use db::{ColumnMeta, DbResult, ResultSet, ValueKind};

use crate::client::{argv, RedisConnection};
use crate::resp::RespValue;
use crate::rows;

/// Elements read in one page.
pub const DEFAULT_PAGE: usize = 500;

/// A field and its value, which is what half the reply shapes here are.
pub(crate) type Pair = (Option<Vec<u8>>, Option<Vec<u8>>);

/// One stream entry's fields, in the order the server sent them.
type Fields = Vec<(Vec<u8>, Option<Vec<u8>>)>;

/// The most of a string value to fetch at once.
///
/// A Redis string can hold 512 MB. Nothing renders that, and asking for it
/// would stall the connection every other pane shares, so a long value arrives
/// as its first megabyte and says so.
pub const MAX_STRING: u64 = 1024 * 1024;

/// What kind of value a key holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyType {
    String,
    List,
    Set,
    SortedSet,
    Hash,
    Stream,
    /// A type from a module — `ReJSON-RL`, `TSDB-TYPE`. Named rather than
    /// swallowed, so the pane can say what it cannot read instead of showing
    /// an empty table.
    Other(String),
}

impl KeyType {
    /// The word `TYPE` answers with. `none` — the key is gone — is `None`
    /// here, because a key that does not exist has no type rather than a type
    /// called "none".
    pub fn parse(reply: &str) -> Option<Self> {
        Some(match reply {
            "none" | "" => return None,
            "string" => Self::String,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::SortedSet,
            "hash" => Self::Hash,
            "stream" => Self::Stream,
            other => Self::Other(other.to_string()),
        })
    }

    /// The word Redis uses, for a badge in the tree and for `SCAN … TYPE`.
    pub fn as_str(&self) -> &str {
        match self {
            Self::String => "string",
            Self::List => "list",
            Self::Set => "set",
            Self::SortedSet => "zset",
            Self::Hash => "hash",
            Self::Stream => "stream",
            Self::Other(name) => name,
        }
    }

    /// Whether this crate can read it.
    pub fn is_readable(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

/// Where the next page starts, in whichever terms this type can be paged by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Position {
    /// A `SCAN` cursor — hashes and sets. Opaque, and only meaningful to the
    /// server that issued it.
    Cursor(u64),
    /// An element index — lists and sorted sets.
    Index(u64),
    /// A stream id, already written exclusively (`(1712-0`).
    Id(String),
}

/// One page of a key's contents.
pub struct KeyPage {
    pub rows: ResultSet,
    /// How many elements the key holds in total, when the server can say
    /// cheaply. A cursor-paged type reports its total but cannot promise the
    /// page count adds up to it — the keyspace can change underneath a scan.
    pub total: Option<u64>,
    /// Where to resume, or `None` when this was the last page.
    pub more: Option<Position>,
}

/// What the header of the value pane shows: what the key is, how big, how
/// long it has left.
#[derive(Debug)]
pub struct KeyFacts {
    pub kind: KeyType,
    /// Seconds until expiry. `None` means no expiry set.
    pub ttl: Option<i64>,
    /// `OBJECT ENCODING` — `listpack`, `hashtable`, `intset`. Worth showing:
    /// it is the difference between a hash that costs 100 bytes and one that
    /// costs 100 kilobytes.
    pub encoding: Option<String>,
    /// `MEMORY USAGE`, when the server has it.
    pub memory: Option<u64>,
    /// Elements, or bytes for a string.
    pub length: Option<u64>,
}

/// What kind of value this key holds, or `None` if it does not exist.
pub async fn type_of(conn: &RedisConnection, key: &[u8]) -> DbResult<Option<KeyType>> {
    let reply = conn.text(&argv([b"TYPE", key])).await?;
    Ok(KeyType::parse(&reply))
}

/// The facts about a key, in one round trip.
pub async fn describe(conn: &RedisConnection, key: &[u8]) -> DbResult<Option<KeyFacts>> {
    let Some(kind) = type_of(conn, key).await? else {
        return Ok(None);
    };
    let length = length_command(&kind, key);
    let mut batch = vec![
        argv([b"TTL", key]),
        argv([b"OBJECT", b"ENCODING", key]),
        argv([b"MEMORY", b"USAGE", key]),
    ];
    if let Some(length) = length.clone() {
        batch.push(length);
    }
    // `MEMORY USAGE` is the one command here that a server might not have —
    // it arrived in 4.0, and some hosted Redises disable it. One failure
    // fails the pipeline, so the retry drops it and the pane goes without.
    let replies = match conn.pipeline(&batch).await {
        Ok(replies) => replies,
        Err(_) => {
            batch.remove(2);
            let mut replies = conn.pipeline(&batch).await?;
            replies.insert(2, RespValue::Nil);
            replies
        }
    };

    Ok(Some(KeyFacts {
        kind,
        ttl: replies.first().and_then(RespValue::as_i64).filter(|ttl| *ttl >= 0),
        encoding: replies.get(1).and_then(|r| r.as_str()).map(str::to_owned),
        memory: replies
            .get(2)
            .and_then(RespValue::as_i64)
            .and_then(|n| u64::try_from(n).ok()),
        length: replies
            .get(3)
            .and_then(RespValue::as_i64)
            .and_then(|n| u64::try_from(n).ok()),
    }))
}

/// Read a page of a key's contents.
///
/// `from` is a [`Position`] this function handed back; `None` starts at the
/// beginning. A `from` of the wrong variant for the type is treated as the
/// beginning rather than as an error — it can only come from a key that
/// changed type under a pane that was already open, which is a refresh, not a
/// failure.
pub async fn read(
    conn: &RedisConnection,
    key: &[u8],
    kind: &KeyType,
    from: Option<&Position>,
    limit: usize,
) -> DbResult<KeyPage> {
    let limit = limit.max(1);
    match kind {
        KeyType::String => read_string(conn, key).await,
        KeyType::List => read_list(conn, key, index(from), limit).await,
        KeyType::SortedSet => read_sorted_set(conn, key, index(from), limit).await,
        KeyType::Hash => read_hash(conn, key, cursor(from), limit).await,
        KeyType::Set => read_set(conn, key, cursor(from), limit).await,
        KeyType::Stream => read_stream(conn, key, from, limit).await,
        KeyType::Other(_) => Ok(KeyPage {
            rows: crate::resp::empty("value"),
            total: None,
            more: None,
        }),
    }
}

async fn read_string(conn: &RedisConnection, key: &[u8]) -> DbResult<KeyPage> {
    let length = conn.number(&argv([b"STRLEN", key])).await? as u64;
    let reply = match length > MAX_STRING {
        true => {
            let end = (MAX_STRING - 1).to_string();
            conn.command(&argv([b"GETRANGE", key, b"0", end.as_bytes()]))
                .await?
        }
        false => conn.command(&argv([b"GET", key])).await?,
    };
    Ok(KeyPage {
        rows: rows::single("value", reply.as_bytes().as_deref()),
        total: Some(length),
        // A string has no second page: the rest of it is fetched by the
        // inspector when somebody asks for it, not by paging the table.
        more: None,
    })
}

async fn read_list(
    conn: &RedisConnection,
    key: &[u8],
    start: u64,
    limit: usize,
) -> DbResult<KeyPage> {
    let total = conn.number(&argv([b"LLEN", key])).await? as u64;
    let stop = start + limit as u64 - 1;
    let reply = conn
        .command(&argv([
            b"LRANGE",
            key,
            start.to_string().as_bytes(),
            stop.to_string().as_bytes(),
        ]))
        .await?;
    let values = scalars(&reply);
    let indices: Vec<_> = (0..values.len()).map(|n| Some(start as i64 + n as i64)).collect();
    let next = start + values.len() as u64;
    let more = (next < total).then_some(Position::Index(next));
    Ok(KeyPage {
        rows: ResultSet::new(vec![
            rows::int_column(ColumnMeta::new("index", ValueKind::Int, "integer"), indices),
            rows::nullable_value_column("value", &values),
        ]),
        total: Some(total),
        more,
    })
}

async fn read_sorted_set(
    conn: &RedisConnection,
    key: &[u8],
    start: u64,
    limit: usize,
) -> DbResult<KeyPage> {
    let total = conn.number(&argv([b"ZCARD", key])).await? as u64;
    let stop = start + limit as u64 - 1;
    let reply = conn
        .command(&argv([
            b"ZRANGE",
            key,
            start.to_string().as_bytes(),
            stop.to_string().as_bytes(),
            b"WITHSCORES",
        ]))
        .await?;
    let pairs = pairs(&reply);
    let members: Vec<_> = pairs.iter().map(|(member, _)| member.clone()).collect();
    let scores: Vec<_> = pairs
        .iter()
        .map(|(_, score)| {
            score
                .as_ref()
                .and_then(|score| std::str::from_utf8(score).ok())
                .and_then(|score| score.parse().ok())
        })
        .collect();
    let next = start + pairs.len() as u64;
    let more = (next < total).then_some(Position::Index(next));
    Ok(KeyPage {
        rows: ResultSet::new(vec![
            rows::nullable_value_column("member", &members),
            // A score is a double, and the grid right-aligns a numeric column
            // — which is what makes a sorted set read as sorted.
            rows::float_column(ColumnMeta::new("score", ValueKind::Float, "double"), scores),
        ]),
        total: Some(total),
        more,
    })
}

async fn read_hash(
    conn: &RedisConnection,
    key: &[u8],
    cursor: u64,
    limit: usize,
) -> DbResult<KeyPage> {
    let total = conn.number(&argv([b"HLEN", key])).await? as u64;
    // A hash small enough to show whole is fetched whole: `HGETALL` gives a
    // stable order for as long as the hash does not change, and a cursor walk
    // does not.
    let (reply, next) = match cursor == 0 && total <= limit as u64 {
        true => (conn.command(&argv([b"HGETALL", key])).await?, None),
        false => {
            let reply = conn
                .command(&argv([
                    b"HSCAN",
                    key,
                    cursor.to_string().as_bytes(),
                    b"COUNT",
                    limit.to_string().as_bytes(),
                ]))
                .await?;
            let (next, page) = scan_reply(&reply);
            (page, next)
        }
    };
    let pairs = pairs(&reply);
    let fields: Vec<_> = pairs.iter().map(|(field, _)| field.clone()).collect();
    let values: Vec<_> = pairs.iter().map(|(_, value)| value.clone()).collect();
    Ok(KeyPage {
        rows: ResultSet::new(vec![
            rows::nullable_value_column("field", &fields),
            rows::nullable_value_column("value", &values),
        ]),
        total: Some(total),
        more: next.map(Position::Cursor),
    })
}

async fn read_set(
    conn: &RedisConnection,
    key: &[u8],
    cursor: u64,
    limit: usize,
) -> DbResult<KeyPage> {
    let total = conn.number(&argv([b"SCARD", key])).await? as u64;
    let (reply, next) = match cursor == 0 && total <= limit as u64 {
        true => (conn.command(&argv([b"SMEMBERS", key])).await?, None),
        false => {
            let reply = conn
                .command(&argv([
                    b"SSCAN",
                    key,
                    cursor.to_string().as_bytes(),
                    b"COUNT",
                    limit.to_string().as_bytes(),
                ]))
                .await?;
            let (next, page) = scan_reply(&reply);
            (page, next)
        }
    };
    Ok(KeyPage {
        rows: ResultSet::new(vec![rows::nullable_value_column("member", &scalars(&reply))]),
        total: Some(total),
        more: next.map(Position::Cursor),
    })
}

/// A stream, as one column of ids and one column per field name in the page.
///
/// Entries in a stream are usually written by one producer to one shape, so
/// spreading the fields across columns is what makes a stream readable at all
/// — and an entry that is missing a field gets a null in it rather than a
/// shifted row.
async fn read_stream(
    conn: &RedisConnection,
    key: &[u8],
    from: Option<&Position>,
    limit: usize,
) -> DbResult<KeyPage> {
    let total = conn.number(&argv([b"XLEN", key])).await? as u64;
    let start = match from {
        Some(Position::Id(id)) => id.clone(),
        _ => "-".to_string(),
    };
    let reply = conn
        .command(&argv([
            b"XRANGE",
            key,
            start.as_bytes(),
            b"+",
            b"COUNT",
            limit.to_string().as_bytes(),
        ]))
        .await?;

    let mut ids: Vec<Option<Vec<u8>>> = Vec::new();
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut entries: Vec<Fields> = Vec::new();
    if let RespValue::Array(items) = &reply {
        for item in items {
            let RespValue::Array(entry) = item else { continue };
            let Some(id) = entry.first().and_then(RespValue::as_bytes) else {
                continue;
            };
            let fields: Vec<_> = entry
                .get(1)
                .map(pairs)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(name, value)| Some((name?, value)))
                .collect();
            for (name, _) in &fields {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            ids.push(Some(id));
            entries.push(fields);
        }
    }

    let more = match ids.len() == limit {
        // `(` makes the id exclusive, so the next page starts after this one
        // rather than repeating its last row.
        true => ids
            .last()
            .and_then(|id| id.as_deref())
            .and_then(|id| std::str::from_utf8(id).ok())
            .map(|id| Position::Id(format!("({id}"))),
        false => None,
    };

    let mut columns = vec![rows::nullable_value_column("id", &ids)];
    for name in &names {
        let cells: Vec<_> = entries
            .iter()
            .map(|fields| {
                fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .and_then(|(_, value)| value.clone())
            })
            .collect();
        columns.push(rows::nullable_value_column(
            &String::from_utf8_lossy(name),
            &cells,
        ));
    }
    Ok(KeyPage {
        rows: ResultSet::new(columns),
        total: Some(total),
        more,
    })
}

/// The command that reports how many elements a key holds, if there is one.
fn length_command(kind: &KeyType, key: &[u8]) -> Option<Vec<Vec<u8>>> {
    Some(match kind {
        KeyType::String => argv([b"STRLEN", key]),
        KeyType::List => argv([b"LLEN", key]),
        KeyType::Set => argv([b"SCARD", key]),
        KeyType::SortedSet => argv([b"ZCARD", key]),
        KeyType::Hash => argv([b"HLEN", key]),
        KeyType::Stream => argv([b"XLEN", key]),
        KeyType::Other(_) => return None,
    })
}

fn index(from: Option<&Position>) -> u64 {
    match from {
        Some(Position::Index(index)) => *index,
        _ => 0,
    }
}

fn cursor(from: Option<&Position>) -> u64 {
    match from {
        Some(Position::Cursor(cursor)) => *cursor,
        _ => 0,
    }
}

/// The two halves of a `SCAN`-family reply: the next cursor, and the page.
/// A zero cursor is the end, and comes back as `None`.
pub(crate) fn scan_reply(reply: &RespValue) -> (Option<u64>, RespValue) {
    let RespValue::Array(parts) = reply else {
        return (None, RespValue::Array(Vec::new()));
    };
    let cursor = parts
        .first()
        .and_then(|cursor| cursor.as_str().and_then(|c| c.parse::<u64>().ok()).or_else(|| cursor.as_i64().and_then(|c| u64::try_from(c).ok())))
        .unwrap_or(0);
    let page = parts.get(1).cloned().unwrap_or(RespValue::Array(Vec::new()));
    ((cursor != 0).then_some(cursor), page)
}

/// A flat reply as a column of values.
fn scalars(reply: &RespValue) -> Vec<Option<Vec<u8>>> {
    match reply {
        RespValue::Array(items) | RespValue::Set(items) => {
            items.iter().map(RespValue::as_bytes).collect()
        }
        _ => Vec::new(),
    }
}

/// A reply of pairs, however this server chose to spell it.
///
/// The same command answers three ways depending on protocol version and
/// command: RESP3 sends a real map, RESP2 sends one flat array of alternating
/// keys and values, and `ZRANGE … WITHSCORES` under RESP3 sends an array of
/// two-element arrays. Handling all three here is what lets the readers above
/// not care which server they are talking to.
pub(crate) fn pairs(reply: &RespValue) -> Vec<Pair> {
    match reply {
        RespValue::Map(entries) => entries
            .iter()
            .map(|(key, value)| (key.as_bytes(), value.as_bytes()))
            .collect(),
        RespValue::Array(items) | RespValue::Set(items) => {
            let nested = items.iter().all(|item| {
                matches!(item, RespValue::Array(pair) if pair.len() == 2)
            });
            if nested && !items.is_empty() {
                return items
                    .iter()
                    .map(|item| match item {
                        RespValue::Array(pair) => (pair[0].as_bytes(), pair[1].as_bytes()),
                        _ => (None, None),
                    })
                    .collect();
            }
            items
                .chunks(2)
                .filter(|chunk| chunk.len() == 2)
                .map(|chunk| (chunk[0].as_bytes(), chunk[1].as_bytes()))
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(text: &str) -> RespValue {
        RespValue::Bulk(text.as_bytes().to_vec())
    }

    #[test]
    fn a_type_word_becomes_a_type() {
        assert_eq!(KeyType::parse("zset"), Some(KeyType::SortedSet));
        assert_eq!(KeyType::parse("none"), None);
        assert_eq!(
            KeyType::parse("ReJSON-RL"),
            Some(KeyType::Other("ReJSON-RL".into()))
        );
        assert!(!KeyType::Other("ReJSON-RL".into()).is_readable());
        assert_eq!(KeyType::SortedSet.as_str(), "zset");
    }

    #[test]
    fn pairs_come_out_the_same_whichever_way_the_server_spelled_them() {
        let expected = vec![
            (Some(b"a".to_vec()), Some(b"1".to_vec())),
            (Some(b"b".to_vec()), Some(b"2".to_vec())),
        ];
        // RESP2: one flat array.
        let flat = RespValue::Array(vec![bulk("a"), bulk("1"), bulk("b"), bulk("2")]);
        assert_eq!(pairs(&flat), expected);
        // RESP3: a map.
        let map = RespValue::Map(vec![(bulk("a"), bulk("1")), (bulk("b"), bulk("2"))]);
        assert_eq!(pairs(&map), expected);
        // RESP3 `ZRANGE … WITHSCORES`: an array of two-element arrays.
        let nested = RespValue::Array(vec![
            RespValue::Array(vec![bulk("a"), bulk("1")]),
            RespValue::Array(vec![bulk("b"), bulk("2")]),
        ]);
        assert_eq!(pairs(&nested), expected);
    }

    #[test]
    fn an_odd_reply_does_not_invent_a_pair() {
        // A truncated reply loses its last element rather than pairing it with
        // nothing: a hash row with no value is a lie, an absent row is not.
        let odd = RespValue::Array(vec![bulk("a"), bulk("1"), bulk("b")]);
        assert_eq!(pairs(&odd).len(), 1);
        assert!(pairs(&RespValue::Nil).is_empty());
    }

    #[test]
    fn a_scan_reply_splits_into_a_cursor_and_a_page() {
        let reply = RespValue::Array(vec![bulk("17"), RespValue::Array(vec![bulk("k")])]);
        let (cursor, page) = scan_reply(&reply);
        assert_eq!(cursor, Some(17));
        assert_eq!(page, RespValue::Array(vec![bulk("k")]));

        // Cursor zero is the end of the walk, not a place to resume from.
        let last = RespValue::Array(vec![bulk("0"), RespValue::Array(vec![])]);
        assert_eq!(scan_reply(&last).0, None);
        assert_eq!(scan_reply(&RespValue::Nil).0, None);
    }
}
