//! [`db::Driver`] for SQLite.
//!
//! As thin as the ClickHouse one, and for the same reason: everything that is
//! SQLite-shaped is in the modules below, and what is left is the translation
//! into what the window draws.

use std::sync::Arc;

use db::{Capabilities, Catalog, DbResult, Driver, Engine, Outcome, Write};
use futures::future::BoxFuture;

use crate::client::SqliteConnection;
use crate::introspect;

impl Driver for SqliteConnection {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SQLITE
    }

    fn server_version(&self) -> Arc<str> {
        SqliteConnection::server_version(self).clone()
    }

    fn is_closed(&self) -> bool {
        SqliteConnection::is_closed(self)
    }

    fn catalog<'a>(&'a self) -> BoxFuture<'a, DbResult<Catalog>> {
        Box::pin(async move {
            let version = SqliteConnection::server_version(self).clone();
            let database = self.database().clone();
            // The whole catalog under one lock, on one thread. Nothing here
            // waits on anything, so splitting it up would only give another
            // connection a chance to change the schema halfway through.
            let snapshot = self
                .with_conn(move |conn| introspect::snapshot(conn, database, version))
                .await?;
            Ok(Catalog::Sql(snapshot))
        })
    }

    fn query<'a>(
        &'a self,
        statement: &'a str,
        max_rows: usize,
    ) -> BoxFuture<'a, DbResult<Outcome>> {
        Box::pin(SqliteConnection::query(self, statement, max_rows))
    }

    fn apply<'a>(&'a self, writes: &'a [Write<'a>]) -> BoxFuture<'a, DbResult<Vec<u64>>> {
        Box::pin(SqliteConnection::apply(self, writes))
    }

    fn cancel(&self) -> BoxFuture<'static, ()> {
        SqliteConnection::cancel(self)
    }
}
