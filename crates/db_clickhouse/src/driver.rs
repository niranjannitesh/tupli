//! [`db::Driver`] for ClickHouse.
//!
//! Thin on purpose: everything that is ClickHouse-shaped is in the modules
//! below this one, and what is left here is the translation from "what the
//! server sent" to "what the window can draw".

use std::sync::Arc;

use db::{
    Capabilities, Catalog, DbError, DbResult, Driver, Engine, ErrorClass, Notice, Outcome, Write,
};
use futures::future::BoxFuture;

use crate::client::ClickHouseConnection;
use crate::introspect;

impl Driver for ClickHouseConnection {
    fn engine(&self) -> Engine {
        Engine::ClickHouse
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::CLICKHOUSE
    }

    fn server_version(&self) -> Arc<str> {
        // Spelled out because the inherent method and this one share a name;
        // `self.server_version()` reads like recursion even though it is not.
        ClickHouseConnection::server_version(self).clone()
    }

    fn is_closed(&self) -> bool {
        ClickHouseConnection::is_closed(self)
    }

    fn take_notices(&self) -> Vec<Notice> {
        ClickHouseConnection::take_notices(self)
    }

    fn catalog<'a>(&'a self) -> BoxFuture<'a, DbResult<Catalog>> {
        Box::pin(async move {
            let version = ClickHouseConnection::server_version(self).clone();
            Ok(Catalog::Sql(introspect::snapshot(self, version).await?))
        })
    }

    fn query<'a>(
        &'a self,
        statement: &'a str,
        max_rows: usize,
    ) -> BoxFuture<'a, DbResult<Outcome>> {
        Box::pin(async move {
            let fetched = self.fetch(statement, max_rows).await?;
            Ok(match fetched.rows {
                // Columns came back, so this was a read — even a read that
                // matched nothing, which still deserves a grid with headers
                // rather than "0 rows affected".
                Some(rows) => Outcome::Rows {
                    rows,
                    truncated: fetched.truncated,
                },
                // No columns: an `insert`, an `alter`, or DDL. The count is
                // whatever the server's own progress reported, which is zero
                // for everything that does not write rows.
                None => Outcome::Affected(fetched.written_rows),
            })
        })
    }

    /// Refused rather than half-implemented.
    ///
    /// [`Capabilities::CLICKHOUSE`] says `transactions: false` and
    /// `editable_rows: false`, so nothing in the app calls this — the grid does
    /// not offer the edit that would produce the writes. Running them one at a
    /// time anyway would be the dangerous version of support: the first three
    /// land, the fourth fails, and there is no way back.
    fn apply<'a>(&'a self, _writes: &'a [Write<'a>]) -> BoxFuture<'a, DbResult<Vec<u64>>> {
        Box::pin(async {
            Err(DbError::new(
                ErrorClass::Internal,
                "ClickHouse has no transactions, so tupli will not stage edits against it. \
                 Run the statements yourself in the console, where you can see each one.",
            ))
        })
    }

    fn cancel(&self) -> BoxFuture<'static, ()> {
        ClickHouseConnection::cancel(self)
    }
}
