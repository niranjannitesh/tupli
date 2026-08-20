//! Query history.
//!
//! Every statement the app sends, whether it worked or not — especially if it
//! did not. The failures are the ones people come back for: "what was that
//! migration I ran before everything broke".
//!
//! History is append-only from the app's side and pruned by age, never edited.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::Store;

/// One statement, as it was sent and as it turned out.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub connection: Option<Uuid>,
    pub sql: String,
    /// Unix milliseconds. The caller stamps this, because the store has no
    /// business deciding what "now" means and tests need it to be a constant.
    pub started_at: i64,
    pub duration_ms: Option<i64>,
    pub row_count: Option<i64>,
    /// `Some` if the server refused it. The message as the server phrased it.
    pub error: Option<String>,
}

impl HistoryEntry {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }

    /// The statement on one line, for a list row that has no room for more.
    pub fn one_line(&self) -> String {
        let mut out = String::with_capacity(self.sql.len());
        let mut space = false;
        for ch in self.sql.chars() {
            if ch.is_whitespace() {
                space = true;
                continue;
            }
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        }
        out
    }
}

impl Store {
    /// Record a statement as it is sent, before the outcome is known, and
    /// return its id so the outcome can be attached when it arrives. A query
    /// that never comes back is still worth having in the list.
    pub fn record_query(
        &self,
        connection: Option<Uuid>,
        sql: &str,
        started_at: i64,
    ) -> Result<i64> {
        self.db().execute(
            "insert into history (connection_id, sql, started_at) values (?1, ?2, ?3)",
            params![connection.map(|id| id.to_string()), sql, started_at],
        )?;
        Ok(self.db().last_insert_rowid())
    }

    /// Attach the outcome to a statement recorded by [`Store::record_query`].
    pub fn finish_query(
        &self,
        id: i64,
        duration_ms: i64,
        row_count: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        self.db().execute(
            "update history set duration_ms = ?1, row_count = ?2, error = ?3 where id = ?4",
            params![duration_ms, row_count, error, id],
        )?;
        Ok(())
    }

    /// The most recent statements, newest first.
    pub fn recent_queries(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.query_history(
            "select id, connection_id, sql, started_at, duration_ms, row_count, error
             from history order by started_at desc, id desc limit ?1",
            params![limit as i64],
        )
    }

    /// The most recent statements sent on one connection.
    pub fn recent_queries_for(&self, connection: Uuid, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.query_history(
            "select id, connection_id, sql, started_at, duration_ms, row_count, error
             from history where connection_id = ?1
             order by started_at desc, id desc limit ?2",
            params![connection.to_string(), limit as i64],
        )
    }

    /// Substring search over the statements themselves — the way anyone
    /// actually looks for something they ran last week.
    pub fn search_history(&self, needle: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let pattern = format!("%{}%", escape_like(needle));
        self.query_history(
            "select id, connection_id, sql, started_at, duration_ms, row_count, error
             from history where sql like ?1 escape '\\'
             order by started_at desc, id desc limit ?2",
            params![pattern, limit as i64],
        )
    }

    /// Drop everything older than `cutoff` (unix milliseconds), and report how
    /// many rows went. Called at boot: an unbounded history is a slow leak that
    /// only shows up on the machines that have been running the app longest.
    pub fn prune_history(&self, cutoff: i64) -> Result<usize> {
        Ok(self
            .db()
            .execute("delete from history where started_at < ?1", params![cutoff])?)
    }

    pub fn history_entry(&self, id: i64) -> Result<Option<HistoryEntry>> {
        Ok(self
            .db()
            .query_row(
                "select id, connection_id, sql, started_at, duration_ms, row_count, error
                 from history where id = ?1",
                params![id],
                read_entry,
            )
            .optional()?)
    }

    fn query_history(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<HistoryEntry>> {
        let mut statement = self.db().prepare(sql)?;
        let rows = statement.query_map(params, read_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn read_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        connection: row
            .get::<_, Option<String>>(1)?
            .and_then(|text| Uuid::parse_str(&text).ok()),
        sql: row.get(2)?,
        started_at: row.get(3)?,
        duration_ms: row.get(4)?,
        row_count: row.get(5)?,
        error: row.get(6)?,
    })
}

/// `%` and `_` are wildcards in `like`, and a user searching for `user_id`
/// means the underscore literally.
fn escape_like(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len());
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_is_recorded_before_its_outcome_is_known() {
        let store = Store::in_memory().unwrap();
        let id = store.record_query(None, "select 1", 1_000).unwrap();

        let pending = store.history_entry(id).unwrap().unwrap();
        assert!(pending.succeeded());
        assert_eq!(pending.duration_ms, None);

        store.finish_query(id, 12, Some(1), None).unwrap();
        let done = store.history_entry(id).unwrap().unwrap();
        assert_eq!(done.duration_ms, Some(12));
        assert_eq!(done.row_count, Some(1));
    }

    #[test]
    fn a_failure_keeps_the_servers_own_words() {
        let store = Store::in_memory().unwrap();
        let id = store.record_query(None, "select oops", 1_000).unwrap();
        store
            .finish_query(id, 3, None, Some("column \"oops\" does not exist"))
            .unwrap();

        let entry = store.history_entry(id).unwrap().unwrap();
        assert!(!entry.succeeded());
        assert_eq!(
            entry.error.as_deref(),
            Some("column \"oops\" does not exist")
        );
    }

    #[test]
    fn history_comes_back_newest_first() {
        let store = Store::in_memory().unwrap();
        store.record_query(None, "first", 1_000).unwrap();
        store.record_query(None, "second", 2_000).unwrap();
        store.record_query(None, "third", 3_000).unwrap();

        let sqls: Vec<_> = store
            .recent_queries(10)
            .unwrap()
            .into_iter()
            .map(|e| e.sql)
            .collect();
        assert_eq!(sqls, ["third", "second", "first"]);
        assert_eq!(store.recent_queries(2).unwrap().len(), 2);
    }

    #[test]
    fn searching_treats_underscores_as_text() {
        let store = Store::in_memory().unwrap();
        store
            .record_query(None, "select user_id from users", 1_000)
            .unwrap();
        store
            .record_query(None, "select userXid from other", 2_000)
            .unwrap();

        let hits = store.search_history("user_id", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].sql.contains("user_id"));
    }

    #[test]
    fn pruning_drops_only_what_is_older_than_the_cutoff() {
        let store = Store::in_memory().unwrap();
        store.record_query(None, "old", 1_000).unwrap();
        store.record_query(None, "new", 5_000).unwrap();

        assert_eq!(store.prune_history(2_000).unwrap(), 1);
        let left = store.recent_queries(10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].sql, "new");
    }

    #[test]
    fn a_multiline_statement_collapses_for_a_list_row() {
        let entry = HistoryEntry {
            id: 1,
            connection: None,
            sql: "select *\n  from users\n where id = 1".into(),
            started_at: 0,
            duration_ms: None,
            row_count: None,
            error: None,
        };
        assert_eq!(entry.one_line(), "select * from users where id = 1");
    }
}
