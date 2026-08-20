//! Saved queries.
//!
//! The statements someone keeps: the monthly revenue rollup, the one that finds
//! the orphaned rows. Unlike history, these are named, edited, and deleted by
//! hand, so they carry a UUID rather than a rowid — a saved query survives an
//! export and re-import of the file, and history does not have to.
//!
//! A saved query may belong to a connection or to no connection at all. The
//! second case is the useful one more often than it looks: `select version()`
//! is not about any one server.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::Store;

#[derive(Clone, Debug, PartialEq)]
pub struct SavedQuery {
    pub id: Uuid,
    /// The connection this belongs to, or `None` for one that applies anywhere.
    pub connection: Option<Uuid>,
    pub name: String,
    pub sql: String,
    /// Unix milliseconds, stamped by the caller.
    pub updated_at: i64,
}

impl SavedQuery {
    pub fn new(name: impl Into<String>, sql: impl Into<String>, updated_at: i64) -> Self {
        Self {
            id: Uuid::new_v4(),
            connection: None,
            name: name.into(),
            sql: sql.into(),
            updated_at,
        }
    }

    pub fn for_connection(mut self, connection: Uuid) -> Self {
        self.connection = Some(connection);
        self
    }
}

impl Store {
    /// Everything saved for `connection`, plus everything saved for no
    /// connection, ordered by name. Both are shown together because a person
    /// looking at a server wants the queries that apply to it, and the
    /// unattached ones apply to all of them.
    pub fn saved_queries(&self, connection: Option<Uuid>) -> Result<Vec<SavedQuery>> {
        match connection {
            Some(id) => self.query_saved(
                "select id, connection_id, name, sql, updated_at from saved_queries
                 where connection_id = ?1 or connection_id is null
                 order by name collate nocase",
                params![id.to_string()],
            ),
            None => self.query_saved(
                "select id, connection_id, name, sql, updated_at from saved_queries
                 order by name collate nocase",
                params![],
            ),
        }
    }

    /// Insert or update. Keyed on the UUID, so renaming a query is an update
    /// and not a second row with the same text under a different name.
    pub fn save_query(&self, query: &SavedQuery) -> Result<()> {
        self.db().execute(
            "insert into saved_queries (id, connection_id, name, sql, updated_at)
             values (?1, ?2, ?3, ?4, ?5)
             on conflict(id) do update set
                 connection_id = excluded.connection_id,
                 name          = excluded.name,
                 sql           = excluded.sql,
                 updated_at    = excluded.updated_at",
            params![
                query.id.to_string(),
                query.connection.map(|id| id.to_string()),
                query.name,
                query.sql,
                query.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn saved_query(&self, id: Uuid) -> Result<Option<SavedQuery>> {
        Ok(self
            .db()
            .query_row(
                "select id, connection_id, name, sql, updated_at from saved_queries where id = ?1",
                params![id.to_string()],
                read_saved,
            )
            .optional()?)
    }

    pub fn delete_saved_query(&self, id: Uuid) -> Result<()> {
        self.db().execute(
            "delete from saved_queries where id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    fn query_saved(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<SavedQuery>> {
        let mut statement = self.db().prepare(sql)?;
        let rows = statement.query_map(params, read_saved)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn read_saved(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedQuery> {
    Ok(SavedQuery {
        id: row
            .get::<_, String>(0)
            .ok()
            .and_then(|text| Uuid::parse_str(&text).ok())
            .unwrap_or_else(Uuid::nil),
        connection: row
            .get::<_, Option<String>>(1)?
            .and_then(|text| Uuid::parse_str(&text).ok()),
        name: row.get(2)?,
        sql: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_the_same_id_twice_is_an_edit_not_a_copy() {
        let store = Store::in_memory().unwrap();
        let mut query = SavedQuery::new("mrr", "select 1", 1_000);
        store.save_query(&query).unwrap();

        query.name = "mrr_by_plan".into();
        query.sql = "select 2".into();
        query.updated_at = 2_000;
        store.save_query(&query).unwrap();

        let all = store.saved_queries(None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "mrr_by_plan");
        assert_eq!(all[0].sql, "select 2");
    }

    #[test]
    fn a_connections_list_includes_the_unattached_ones() {
        let store = Store::in_memory().unwrap();
        let mine = Uuid::new_v4();
        let theirs = Uuid::new_v4();

        store
            .save_query(&SavedQuery::new("ours", "select 1", 0).for_connection(mine))
            .unwrap();
        store
            .save_query(&SavedQuery::new("theirs", "select 2", 0).for_connection(theirs))
            .unwrap();
        store
            .save_query(&SavedQuery::new("anywhere", "select version()", 0))
            .unwrap();

        let names: Vec<_> = store
            .saved_queries(Some(mine))
            .unwrap()
            .into_iter()
            .map(|q| q.name)
            .collect();
        assert_eq!(names, ["anywhere", "ours"]);
    }

    #[test]
    fn deleting_leaves_the_rest_alone() {
        let store = Store::in_memory().unwrap();
        let keep = SavedQuery::new("keep", "select 1", 0);
        let drop = SavedQuery::new("drop", "select 2", 0);
        store.save_query(&keep).unwrap();
        store.save_query(&drop).unwrap();

        store.delete_saved_query(drop.id).unwrap();
        assert!(store.saved_query(drop.id).unwrap().is_none());
        assert_eq!(store.saved_queries(None).unwrap().len(), 1);
    }
}
