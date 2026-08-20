//! [`RedisConnection`] as the app sees it.
//!
//! The interesting half of the driver boundary, because Redis does not fit it
//! by accident the way Postgres does. A command line is not a statement, a
//! reply is not a result set, and there is no catalog — so each of those is a
//! decision made here rather than a forwarding call, and each is written down.

use std::sync::Arc;

use db::{
    Capabilities, Catalog, Cursor, DbError, DbResult, Driver, Engine, ErrorClass, KeyFacts,
    KeyListing, KeyPage, KeyQuery, KeyType, Keyspace, KeyspaceDatabase, Outcome, Write,
};
use futures::future::BoxFuture;

use crate::client::RedisConnection;
use crate::resp::{self, RespValue};
use crate::scan::Scan;
use crate::{info, keys, rows};

impl Driver for RedisConnection {
    fn engine(&self) -> Engine {
        Engine::Redis
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::REDIS
    }

    fn server_version(&self) -> Arc<str> {
        RedisConnection::server_version(self).clone()
    }

    fn is_closed(&self) -> bool {
        RedisConnection::is_closed(self)
    }

    /// The keyspace, which is as much of a catalog as Redis has.
    ///
    /// `INFO keyspace` rather than sixteen `DBSIZE`s, and nothing about the
    /// keys themselves: they are browsed a page at a time by [`crate::scan`],
    /// because the only command that would list them all is the one that takes
    /// the server down.
    fn catalog<'a>(&'a self) -> BoxFuture<'a, DbResult<Catalog>> {
        Box::pin(async move {
            let databases = info::databases(self).await?;
            Ok(Catalog::Keyspace(Keyspace {
                databases: databases
                    .iter()
                    .map(|database| KeyspaceDatabase {
                        index: database.index,
                        keys: database.keys,
                        expires: database.expires,
                    })
                    .collect(),
                current: self.config().db_index,
            }))
        })
    }

    /// One page of the keyspace.
    ///
    /// A `Scan` is rebuilt per call rather than kept between them, because the
    /// only thing a walk carries forward is its cursor and the caller already
    /// holds that. What must not be rebuilt is what the *server* turned out
    /// not to support, and that lives on the connection — see
    /// [`RedisConnection::has_memory_usage`].
    ///
    /// [`Scan::take`] and not [`Scan::page`]: one page of a filtered scan is
    /// very often empty, and a browser that showed nothing until somebody
    /// pressed "more" four times would look broken rather than busy.
    fn list_keys<'a>(&'a self, query: &'a KeyQuery) -> BoxFuture<'a, DbResult<KeyListing>> {
        Box::pin(async move {
            let mut scan = Scan::new().matching(&query.pattern).count(query.limit.max(1));
            if let Some(kind) = &query.kind {
                scan = scan.of_type(kind);
            }
            if !query.memory {
                scan = scan.without_memory();
            }
            if let Some(Cursor::Walk(cursor)) = query.from {
                scan = scan.resume(cursor);
            }
            let keys = scan.take(self, query.limit.max(1)).await?;
            Ok(KeyListing {
                keys,
                more: scan.position().map(Cursor::Walk),
            })
        })
    }

    fn describe_key<'a>(&'a self, key: &'a [u8]) -> BoxFuture<'a, DbResult<Option<KeyFacts>>> {
        Box::pin(keys::describe(self, key))
    }

    fn read_key<'a>(
        &'a self,
        key: &'a [u8],
        kind: &'a KeyType,
        from: Option<&'a Cursor>,
        limit: usize,
    ) -> BoxFuture<'a, DbResult<KeyPage>> {
        Box::pin(keys::read(self, key, kind, from, limit))
    }

    /// Run one command line.
    ///
    /// The console types `hgetall user:1`, not SQL, so the split is
    /// `redis-cli`'s and a bad quote is reported as the user's typo rather
    /// than sent to the server as a key.
    fn query<'a>(
        &'a self,
        statement: &'a str,
        max_rows: usize,
    ) -> BoxFuture<'a, DbResult<Outcome>> {
        Box::pin(async move {
            let args = resp::split_args(statement)
                .map_err(|problem| DbError::new(ErrorClass::Syntax, problem))?;
            if args.is_empty() {
                return Err(DbError::new(
                    ErrorClass::Syntax,
                    "There is no command on this line.",
                ));
            }
            let reply = self.command(&args).await?;
            Ok(outcome(reply, max_rows))
        })
    }

    /// Redis has no transaction the grid could stage edits into — `MULTI` runs
    /// a batch without rolling one back — so [`Capabilities::REDIS`] says
    /// `transactions: false` and `editable_rows: false`, and nothing calls
    /// this. It is an error rather than a silent success because a write that
    /// reported success without happening is the worst answer available.
    fn apply<'a>(&'a self, _writes: &'a [Write<'a>]) -> BoxFuture<'a, DbResult<Vec<u64>>> {
        Box::pin(async move {
            Err(DbError::internal(
                "a Redis connection has no transaction to apply SQL to",
            ))
        })
    }
}

/// A reply → what the results pane shows.
///
/// Everything is rows, including `OK` and including a count. The temptation is
/// to call an integer reply [`Outcome::Affected`], but `LLEN` and `DEL` both
/// answer with one, and the driver has no way to tell "three keys deleted"
/// from "three items in the list" without a table of every command's meaning.
/// A one-cell result says what the server said and claims nothing more.
fn outcome(reply: RespValue, max_rows: usize) -> Outcome {
    let (reply, truncated) = truncate(reply, max_rows);
    let rows = reply
        .to_result_set()
        .unwrap_or_else(|| rows::single("reply", reply.as_bytes().as_deref()));
    Outcome::Rows { rows, truncated }
}

/// Cut a container reply down to `max_rows` elements.
///
/// Cheaper than building the whole result set and slicing it, and it is the
/// same limit for the same reason: `LRANGE mylist 0 -1` on a list of ten
/// million is a mistake somebody should be able to recover from.
fn truncate(reply: RespValue, max_rows: usize) -> (RespValue, bool) {
    match reply {
        RespValue::Array(mut items) if items.len() > max_rows => {
            items.truncate(max_rows);
            (RespValue::Array(items), true)
        }
        RespValue::Set(mut items) if items.len() > max_rows => {
            items.truncate(max_rows);
            (RespValue::Set(items), true)
        }
        RespValue::Map(mut pairs) if pairs.len() > max_rows => {
            pairs.truncate(max_rows);
            (RespValue::Map(pairs), true)
        }
        other => (other, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scalar_reply_is_a_one_cell_table() {
        let Outcome::Rows { rows, truncated } = outcome(RespValue::Status("OK".into()), 100) else {
            panic!("a reply is always rows");
        };
        assert!(!truncated);
        assert_eq!(rows.row_count(), 1);
        assert_eq!(rows.column_count(), 1);
    }

    #[test]
    fn a_count_is_shown_and_not_claimed_as_an_affected_row_count() {
        // `DEL k` and `LLEN k` are the same reply; only one of them is a count
        // of anything that changed.
        let Outcome::Rows { rows, .. } = outcome(RespValue::Int(3), 100) else {
            panic!("a reply is always rows");
        };
        let mut scratch = String::new();
        assert!(matches!(
            rows.columns[0].render(0, &mut scratch),
            db::CellText::Borrowed("3")
        ));
    }

    #[test]
    fn a_long_reply_is_cut_and_says_so() {
        let long = RespValue::Array((0..10).map(RespValue::Int).collect());
        let Outcome::Rows { rows, truncated } = outcome(long, 4) else {
            panic!("a reply is always rows");
        };
        assert!(truncated);
        assert_eq!(rows.row_count(), 4);
    }

    #[test]
    fn a_reply_that_fits_is_not_called_truncated() {
        let short = RespValue::Array((0..4).map(RespValue::Int).collect());
        let (_, truncated) = truncate(short, 4);
        assert!(!truncated);
    }
}
