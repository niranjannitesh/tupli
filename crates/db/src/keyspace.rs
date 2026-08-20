//! The vocabulary of a keyspace: what a key is, and how one is listed.
//!
//! Here rather than in the Redis driver because the app has to be able to draw
//! a key browser without depending on the crate that fills it, which is the
//! whole point of [`crate::Driver`]. Postgres puts every object it has into a
//! [`crate::SchemaSnapshot`] once and the tree reads it; a keyspace cannot be
//! snapshotted — ten million keys is a listing nobody wants and a command that
//! would stop the server — so it is read a page at a time instead, and these
//! are the types that page.

use std::sync::Arc;

use crate::ResultSet;

/// What kind of value a key holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyType {
    String,
    List,
    Set,
    SortedSet,
    Hash,
    Stream,
    /// A type from a module — `ReJSON-RL`, `TSDB-TYPE`. Named rather than
    /// swallowed, so a pane can say what it cannot read instead of showing an
    /// empty table.
    Other(String),
}

impl KeyType {
    /// The word a `TYPE` reply carries. `none` — the key is gone — is `None`
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

    /// The word the server uses, for a badge in the tree and for a typed scan.
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

    /// The word a badge shows, which is not always the word the wire uses:
    /// `zset` is what the protocol calls it and `sorted set` is what it is.
    pub fn label(&self) -> &str {
        match self {
            Self::SortedSet => "sorted set",
            other => other.as_str(),
        }
    }

    /// Whether a driver can read this one into rows.
    pub fn is_readable(&self) -> bool {
        !matches!(self, Self::Other(_))
    }

    /// The six built-in types, in the order a filter should offer them.
    pub const BUILT_IN: [Self; 6] = [
        Self::String,
        Self::List,
        Self::Hash,
        Self::Set,
        Self::SortedSet,
        Self::Stream,
    ];
}

/// One key, as the browser lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// The key's bytes. Not a `String`: a Redis key is binary, and a browser
    /// that could not show a key it cannot decode would be hiding data rather
    /// than displaying it.
    pub key: Arc<[u8]>,
    pub kind: KeyType,
    /// Seconds until this expires. `None` means it does not.
    pub ttl: Option<i64>,
    /// Bytes, as the server estimates them, when it will say.
    pub memory: Option<u64>,
}

/// What the header of a value pane shows: what the key is, how big it is, how
/// long it has left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyFacts {
    pub kind: KeyType,
    /// Seconds until expiry. `None` means no expiry set.
    pub ttl: Option<i64>,
    /// How the server is storing it — `listpack`, `hashtable`, `intset`.
    /// Worth showing: it is the difference between a hash that costs a hundred
    /// bytes and one that costs a hundred kilobytes.
    pub encoding: Option<String>,
    pub memory: Option<u64>,
    /// Elements, or bytes for a string.
    pub length: Option<u64>,
}

/// Where the next page starts, in whichever terms the thing being paged can be
/// paged by.
///
/// Three variants rather than one universal offset, because the server offers
/// two paging methods and they are not interchangeable: ordered collections
/// can be asked for a range, unordered ones can only be walked with a cursor
/// in an order nobody promises to keep, and a stream is keyed by id. Pretending
/// to one offset would mean three different things under one name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cursor {
    /// An opaque walk position, only meaningful to the server that issued it.
    Walk(u64),
    /// An element index.
    Index(u64),
    /// An id, already written so that the next page excludes it.
    Id(String),
}

/// What to list.
#[derive(Clone, Debug, Default)]
pub struct KeyQuery {
    /// A glob the server matches — `user:*`, `session:??`. Empty lists
    /// everything.
    pub pattern: String,
    /// Only keys of one type, when the browser is filtered to one.
    pub kind: Option<KeyType>,
    /// Where to resume. `None` starts the walk.
    pub from: Option<Cursor>,
    /// How much work to ask for. A hint about effort, not a row count: a page
    /// can come back empty with the walk unfinished.
    pub limit: usize,
    /// Whether to ask what each key costs. It samples the value to estimate a
    /// size, so on a keyspace of large values it is the expensive part of
    /// listing, and a browser that is only drawing names should say no.
    pub memory: bool,
}

/// One page of a key listing.
#[derive(Clone, Debug, Default)]
pub struct KeyListing {
    pub keys: Vec<KeyInfo>,
    /// Where to resume, or `None` when the walk has finished. An empty page
    /// with a cursor is normal and does not mean the keyspace ended.
    pub more: Option<Cursor>,
}

/// One page of a key's contents.
pub struct KeyPage {
    pub rows: ResultSet,
    /// How many elements the key holds in total, when the server can say
    /// cheaply. A cursor-paged type reports its total but cannot promise the
    /// pages add up to it — the keyspace can change underneath a walk.
    pub total: Option<u64>,
    /// Where to resume, or `None` when this was the last page.
    pub more: Option<Cursor>,
}

/// How the databases on a server divide up, which is as much of a catalog as a
/// keyspace has.
#[derive(Clone, Debug, Default)]
pub struct Keyspace {
    pub databases: Vec<KeyspaceDatabase>,
    /// The database this connection is on.
    pub current: u8,
}

#[derive(Clone, Debug)]
pub struct KeyspaceDatabase {
    pub index: u8,
    pub keys: u64,
    /// How many of those keys have a TTL.
    pub expires: u64,
}

/// `14d 5h`, `3m 20s`, `Forever` — a TTL as a header shows it.
///
/// Two units and never three: the point of the line is whether the key is
/// about to go, and `14d 5h 22m 8s` answers that no better than `14d 5h` while
/// being four times as much to read past.
pub fn format_ttl(ttl: Option<i64>) -> String {
    let Some(seconds) = ttl.filter(|ttl| *ttl >= 0) else {
        return "Forever".into();
    };
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hours, rest) = (rest / 3_600, rest % 3_600);
    let (minutes, seconds) = (rest / 60, rest % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{seconds}s"),
        (0, 0, _) => format!("{minutes}m {seconds}s"),
        (0, _, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// A key's bytes as something to put on a row.
///
/// Redis keys are usually UTF-8 and occasionally not. The lossy conversion is
/// deliberate: a key that arrives as bytes nobody can decode still has to be
/// visible and clickable, and a replacement character where the bad byte was
/// says more than hiding the row would.
pub fn key_text(key: &[u8]) -> String {
    String::from_utf8_lossy(key).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ttl_is_shown_in_the_two_units_that_matter() {
        assert_eq!(format_ttl(None), "Forever");
        // A key with no expiry reports -1, which is not a duration.
        assert_eq!(format_ttl(Some(-1)), "Forever");
        assert_eq!(format_ttl(Some(45)), "45s");
        assert_eq!(format_ttl(Some(200)), "3m 20s");
        assert_eq!(format_ttl(Some(7_325)), "2h 2m");
        assert_eq!(format_ttl(Some(1_226_100)), "14d 4h");
    }

    #[test]
    fn a_sorted_set_is_called_what_it_is_and_scanned_by_what_the_wire_calls_it() {
        assert_eq!(KeyType::SortedSet.label(), "sorted set");
        assert_eq!(KeyType::SortedSet.as_str(), "zset");
    }

    #[test]
    fn a_key_that_is_not_utf8_is_still_a_row() {
        assert_eq!(key_text(&[0xff, b'a']), "\u{fffd}a");
    }
}
