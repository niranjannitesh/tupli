//! [`PgConnection`] as the app sees it.
//!
//! Nothing new happens here — every call forwards to the inherent method it
//! shadows. The file exists so that the trait's shape is visible in one place,
//! and so `client.rs` stays a Postgres file rather than a Postgres file with
//! an abstraction threaded through it.

use std::sync::Arc;

use db::{Capabilities, Catalog, DbResult, Driver, Engine, Notice, Outcome, Write};
use futures::future::BoxFuture;

use crate::client::PgConnection;
use crate::introspect;

impl Driver for PgConnection {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::POSTGRES
    }

    fn server_version(&self) -> Arc<str> {
        PgConnection::server_version(self).clone()
    }

    fn is_closed(&self) -> bool {
        PgConnection::is_closed(self)
    }

    fn take_notices(&self) -> Vec<Notice> {
        PgConnection::take_notices(self)
    }

    fn catalog<'a>(&'a self) -> BoxFuture<'a, DbResult<Catalog>> {
        Box::pin(async move { introspect::snapshot(self).await.map(Catalog::Sql) })
    }

    fn query<'a>(
        &'a self,
        statement: &'a str,
        max_rows: usize,
    ) -> BoxFuture<'a, DbResult<Outcome>> {
        Box::pin(PgConnection::query(self, statement, max_rows))
    }

    fn apply<'a>(&'a self, writes: &'a [Write<'a>]) -> BoxFuture<'a, DbResult<Vec<u64>>> {
        Box::pin(PgConnection::apply(self, writes))
    }

    /// The cancel is `'static` because the request is a *second* connection: it
    /// carries the backend key and nothing of this one, so it outlives the
    /// borrow and can be sent while the first socket is still waiting on rows.
    fn cancel(&self) -> BoxFuture<'static, ()> {
        let canceller = self.canceller();
        Box::pin(async move { canceller.cancel().await })
    }
}
