//! Query history.
//!
//! Every statement the app sends, whether it worked or not — especially if it
//! did not. The failures are the ones people come back for: "what was that
//! migration I ran before everything broke".
//!
//! And not only the statements. A commit, an import and an export are things
//! the app did to a database on somebody's behalf, and leaving them out of the
//! record meant the durable log was missing exactly the entries with
//! consequences. They are all [`Kind`]s of the same row.
//!
//! History is append-only from the app's side and pruned by age, never edited.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::Store;

/// What the app was doing. A statement unless it says otherwise, which is what
/// the column's default means for every row written before there were others.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Statement,
    /// Staged grid edits, or a structure change, sent as one transaction.
    Commit,
    Import,
    Export,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Statement => "statement",
            Self::Commit => "commit",
            Self::Import => "import",
            Self::Export => "export",
        }
    }

    /// An unknown word reads as a statement rather than as an error: a file
    /// written by a later build is still a log worth showing.
    fn parse(text: &str) -> Self {
        match text {
            "commit" => Self::Commit,
            "import" => Self::Import,
            "export" => Self::Export,
            _ => Self::Statement,
        }
    }
}

/// How it turned out.
///
/// `Canceled` is its own answer rather than a kind of failure: nothing is
/// wrong with a statement somebody decided they no longer wanted, and a log
/// that paints it red teaches people to ignore red.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Outcome {
    #[default]
    Ok,
    Failed,
    Canceled,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => Self::Ok,
        }
    }
}

/// How something turned out, as one argument.
///
/// A struct rather than six parameters, four of which are `Option<i64>` or
/// `Option<String>`: a call site that got two of those the wrong way round
/// would compile and then quietly log the wrong thing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finished {
    pub duration_ms: i64,
    pub row_count: Option<i64>,
    pub affected: Option<i64>,
    pub error: Option<String>,
    pub outcome: Outcome,
    pub notices: Vec<String>,
}

impl Finished {
    pub fn ok(duration_ms: i64) -> Self {
        Self {
            duration_ms,
            ..Self::default()
        }
    }

    pub fn failed(duration_ms: i64, error: impl Into<String>) -> Self {
        Self {
            duration_ms,
            error: Some(error.into()),
            outcome: Outcome::Failed,
            ..Self::default()
        }
    }
}

/// One thing the app did, as it was sent and as it turned out.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub connection: Option<Uuid>,
    /// The statement, or — for the other kinds — what was done, in the one
    /// line a list row has space for: `commit (2 inserts)`, `export → sales.csv`.
    pub sql: String,
    /// Unix milliseconds. The caller stamps this, because the store has no
    /// business deciding what "now" means and tests need it to be a constant.
    pub started_at: i64,
    pub duration_ms: Option<i64>,
    pub row_count: Option<i64>,
    /// How many rows a statement that returned none changed. Beside
    /// `row_count` rather than folded into it: three rows updated and three
    /// rows returned are different facts, and a log that spells both "3 rows"
    /// is one nobody can read backwards.
    pub affected: Option<i64>,
    /// `Some` if the server refused it. The message as the server phrased it.
    pub error: Option<String>,
    pub outcome: Outcome,
    pub kind: Kind,
    /// What the server said on the side while this ran: `RAISE NOTICE` from a
    /// function, the `WARNING` a `create ... if not exists` produces. Each
    /// already carries its own severity, because the server's word for it is
    /// worth more than a rank this app invented.
    pub notices: Vec<String>,
}

impl HistoryEntry {
    pub fn succeeded(&self) -> bool {
        self.outcome == Outcome::Ok
    }

    /// Still in flight, or never came back. A row is written when a statement
    /// is sent, so this is the difference between "no duration yet" and "took
    /// no time".
    pub fn pending(&self) -> bool {
        self.duration_ms.is_none()
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
    pub fn finish_query(&self, id: i64, outcome: &Finished) -> Result<()> {
        self.db().execute(
            "update history
                set duration_ms = ?1, row_count = ?2, affected = ?3,
                    error = ?4, outcome = ?5, notices = ?6
              where id = ?7",
            params![
                outcome.duration_ms,
                outcome.row_count,
                outcome.affected,
                outcome.error,
                outcome.outcome.as_str(),
                pack_notices(&outcome.notices),
                id
            ],
        )?;
        Ok(())
    }

    /// Record something that is already over: a commit, an import, an export.
    ///
    /// Two calls for a statement and one for these, because a statement is
    /// worth a row the moment it is sent — a query that never comes back is
    /// exactly the one somebody will go looking for — while a commit has
    /// nothing to say until it has said all of it.
    pub fn record_event(
        &self,
        connection: Option<Uuid>,
        kind: Kind,
        sql: &str,
        started_at: i64,
        outcome: &Finished,
    ) -> Result<i64> {
        self.db().execute(
            "insert into history
               (connection_id, sql, started_at, kind,
                duration_ms, row_count, affected, error, outcome, notices)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                connection.map(|id| id.to_string()),
                sql,
                started_at,
                kind.as_str(),
                outcome.duration_ms,
                outcome.row_count,
                outcome.affected,
                outcome.error,
                outcome.outcome.as_str(),
                pack_notices(&outcome.notices),
            ],
        )?;
        Ok(self.db().last_insert_rowid())
    }

    /// The most recent statements, newest first.
    pub fn recent_queries(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.query_history(
            "select id, connection_id, sql, started_at, duration_ms, row_count, error,
                    affected, outcome, kind, notices
             from history order by started_at desc, id desc limit ?1",
            params![limit as i64],
        )
    }

    /// The most recent statements sent on one connection.
    pub fn recent_queries_for(&self, connection: Uuid, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.query_history(
            "select id, connection_id, sql, started_at, duration_ms, row_count, error,
                    affected, outcome, kind, notices
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
            "select id, connection_id, sql, started_at, duration_ms, row_count, error,
                    affected, outcome, kind, notices
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
                "select id, connection_id, sql, started_at, duration_ms, row_count, error,
                        affected, outcome, kind, notices
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
        affected: row.get(7)?,
        outcome: Outcome::parse(&row.get::<_, String>(8)?),
        kind: Kind::parse(&row.get::<_, String>(9)?),
        notices: unpack_notices(row.get::<_, Option<String>>(10)?),
    })
}

/// One notice per line, `None` for none at all.
///
/// A notice is a paragraph — message, detail, hint — so the lines within one
/// are the notice's own and the separator between two has to be something
/// else. It is a form feed, which no server has ever put in a notice and no
/// list row would survive if it did.
fn pack_notices(notices: &[String]) -> Option<String> {
    match notices.is_empty() {
        true => None,
        false => Some(notices.join("\u{c}")),
    }
}

fn unpack_notices(packed: Option<String>) -> Vec<String> {
    packed
        .filter(|text| !text.is_empty())
        .map(|text| text.split('\u{c}').map(str::to_string).collect())
        .unwrap_or_default()
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

        assert!(pending.pending());

        store
            .finish_query(
                id,
                &Finished {
                    row_count: Some(1),
                    ..Finished::ok(12)
                },
            )
            .unwrap();
        let done = store.history_entry(id).unwrap().unwrap();
        assert_eq!(done.duration_ms, Some(12));
        assert_eq!(done.row_count, Some(1));
        assert!(!done.pending());
    }

    #[test]
    fn a_failure_keeps_the_servers_own_words() {
        let store = Store::in_memory().unwrap();
        let id = store.record_query(None, "select oops", 1_000).unwrap();
        store
            .finish_query(id, &Finished::failed(3, "column \"oops\" does not exist"))
            .unwrap();

        let entry = store.history_entry(id).unwrap().unwrap();
        assert!(!entry.succeeded());
        assert_eq!(entry.outcome, Outcome::Failed);
        assert_eq!(
            entry.error.as_deref(),
            Some("column \"oops\" does not exist")
        );
    }

    #[test]
    fn a_canceled_statement_is_neither_a_success_nor_a_failure() {
        let store = Store::in_memory().unwrap();
        let id = store
            .record_query(None, "select pg_sleep(60)", 1_000)
            .unwrap();
        store
            .finish_query(
                id,
                &Finished {
                    outcome: Outcome::Canceled,
                    ..Finished::ok(4_200)
                },
            )
            .unwrap();

        let entry = store.history_entry(id).unwrap().unwrap();
        assert_eq!(entry.outcome, Outcome::Canceled);
        assert!(!entry.succeeded());
        // It ran for four seconds before anybody changed their mind, and that
        // is the thing worth knowing about it.
        assert_eq!(entry.duration_ms, Some(4_200));
        assert_eq!(entry.error, None);
    }

    #[test]
    fn what_the_server_said_on_the_side_survives_a_round_trip() {
        let store = Store::in_memory().unwrap();
        let id = store.record_query(None, "call rebuild()", 1_000).unwrap();
        store
            .finish_query(
                id,
                &Finished {
                    // A notice is a paragraph, so one of them holds the
                    // newline that must not be read back as two.
                    notices: vec![
                        "NOTICE: rebuilding\nDETAIL: 3 partitions".into(),
                        "WARNING: index left invalid".into(),
                    ],
                    ..Finished::ok(9)
                },
            )
            .unwrap();

        let entry = store.history_entry(id).unwrap().unwrap();
        assert_eq!(entry.notices.len(), 2);
        assert!(entry.notices[0].ends_with("3 partitions"));
        assert_eq!(entry.notices[1], "WARNING: index left invalid");
    }

    #[test]
    fn a_commit_is_recorded_in_one_go_because_it_is_already_over() {
        let store = Store::in_memory().unwrap();
        let id = store
            .record_event(
                None,
                Kind::Commit,
                "commit (2 inserts, 1 update)",
                1_000,
                &Finished {
                    affected: Some(3),
                    ..Finished::ok(31)
                },
            )
            .unwrap();

        let entry = store.history_entry(id).unwrap().unwrap();
        assert_eq!(entry.kind, Kind::Commit);
        assert_eq!(entry.affected, Some(3));
        // Rows changed, not rows returned: the two are different facts and the
        // log keeps them in different columns.
        assert_eq!(entry.row_count, None);
        assert!(!entry.pending());
    }

    #[test]
    fn a_statement_recorded_before_there_were_other_kinds_is_a_statement() {
        let store = Store::in_memory().unwrap();
        let id = store.record_query(None, "select 1", 1_000).unwrap();
        let entry = store.history_entry(id).unwrap().unwrap();
        assert_eq!(entry.kind, Kind::Statement);
        assert_eq!(entry.outcome, Outcome::Ok);
        assert!(entry.notices.is_empty());
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
            affected: None,
            error: None,
            outcome: Outcome::Ok,
            kind: Kind::Statement,
            notices: Vec::new(),
        };
        assert_eq!(entry.one_line(), "select * from users where id = 1");
    }
}
