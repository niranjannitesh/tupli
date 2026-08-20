//! The table being designed, and the statements that make it so.
//!
//! The structure editor edits a [`TableDraft`] — a plain value with no server
//! and no window in it — and asks this module twice: once for the problems
//! with what has been typed, and once for the SQL that would make the server
//! agree with it. Both answers are pure functions of the draft, which is what
//! lets the whole of "what would Save do" be tested without a database.
//!
//! The diff is by *origin*, not by position. A column carries the name it has
//! on the server, so renaming `email` to `contact_email` is one `RENAME
//! COLUMN`, not a drop and an add that would take the data with it. A column
//! with no origin has never been to the server and can only be added.
//!
//! What this deliberately does not do:
//!
//! - **Reorder columns.** Postgres has no `ALTER TABLE … SET ORDER`, so a list
//!   that could be dragged would be promising something the server cannot do.
//! - **Guess a cast.** A type change is written as a plain `ALTER COLUMN …
//!   TYPE`, with no `USING`. When Postgres cannot see a way across it says so,
//!   and that error is a better answer than a `USING` clause the app invented.
//! - **Touch indexes, foreign keys or checks.** Those are their own editors;
//!   this one is columns, the primary key, and the two comments.

use std::fmt::Write as _;
use std::sync::Arc;

use db::schema::{quote_ident, quote_literal};
use db::{IdentityKind, Relation, RelationRef};

use crate::ddl::INDENT;

/// One column as the editor holds it: strings, because that is what is typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnDraft {
    /// The name this column answers to on the server, if it is there at all.
    /// This is what makes a rename a rename.
    pub origin: Option<Arc<str>>,
    pub name: String,
    /// The type verbatim — `text`, `numeric(10,2)`, `timestamptz`. Not parsed:
    /// the set of things Postgres accepts here includes domains and arrays of
    /// composites, and a whitelist would only ever be wrong.
    pub type_name: String,
    pub nullable: bool,
    /// The default expression as SQL, unquoted — `now()`, `0`, `'draft'`.
    /// Empty means no default.
    pub default: String,
    pub is_pk: bool,
    /// Set on the server's own columns; the editor shows it and leaves it
    /// alone. A new column may carry one, which is how a blank draft's `id`
    /// becomes an identity column.
    pub identity: Option<IdentityKind>,
    /// `GENERATED ALWAYS AS (…) STORED`. Shown, never edited.
    pub is_generated: bool,
    pub comment: String,
}

impl ColumnDraft {
    /// A column that is already on the server.
    pub fn of(column: &db::ColumnDef, is_pk: bool) -> Self {
        Self {
            origin: Some(column.name.clone()),
            name: column.name.to_string(),
            type_name: column.type_name.to_string(),
            nullable: column.nullable,
            // A generated column keeps its expression in the default slot, and
            // showing it as a default would invite someone to edit it into an
            // `ALTER COLUMN … SET DEFAULT` that Postgres refuses.
            default: match column.is_generated {
                true => String::new(),
                false => column.default.as_deref().unwrap_or("").to_string(),
            },
            is_pk,
            identity: column.identity,
            is_generated: column.is_generated,
            comment: column.comment.as_deref().unwrap_or("").to_string(),
        }
    }

    /// A blank row, ready to be typed into.
    pub fn new() -> Self {
        Self {
            nullable: true,
            ..Self::default()
        }
    }

    /// Is this column the server's to fill in? Those are shown but not edited:
    /// changing the type of an identity column, or the default of a generated
    /// one, is a different statement from the one this editor writes.
    pub fn is_server_owned(&self) -> bool {
        self.identity.is_some() || self.is_generated
    }

    /// Has this column been to the server?
    pub fn is_new(&self) -> bool {
        self.origin.is_none()
    }
}

/// A whole table as the editor holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableDraft {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ColumnDraft>,
    pub comment: String,
    /// The name of the primary key constraint as it exists, so that changing
    /// the key can drop the right one. `None` when the table has no key yet,
    /// or does not exist yet.
    pub pk_name: Option<Arc<str>>,
}

impl TableDraft {
    /// The table as it stands on the server.
    pub fn of(relation: &Relation) -> Self {
        let pk = relation.primary_key();
        let key: Vec<&str> = pk
            .map(|pk| pk.columns.iter().map(|c| &**c).collect())
            .unwrap_or_default();
        Self {
            schema: relation.reference.schema.to_string(),
            name: relation.reference.name.to_string(),
            columns: relation
                .columns
                .iter()
                .map(|column| ColumnDraft::of(column, key.contains(&&*column.name)))
                .collect(),
            comment: relation.comment.as_deref().unwrap_or("").to_string(),
            pk_name: pk.map(|pk| pk.name.clone()),
        }
    }

    /// A new table: one identity key column, because every table gets one and
    /// typing it out again is not a decision anybody wants to make.
    pub fn blank(schema: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: String::new(),
            columns: vec![ColumnDraft {
                name: "id".into(),
                type_name: "bigint".into(),
                nullable: false,
                is_pk: true,
                identity: Some(IdentityKind::ByDefault),
                ..ColumnDraft::new()
            }],
            comment: String::new(),
            pk_name: None,
        }
    }

    pub fn reference(&self) -> RelationRef {
        RelationRef::new(self.schema.trim(), self.name.trim())
    }

    /// The key columns, in the order they are listed.
    fn key(&self) -> Vec<&ColumnDraft> {
        self.columns.iter().filter(|c| c.is_pk).collect()
    }

    fn qualified(&self) -> String {
        self.reference().qualified()
    }
}

/// What is wrong with the draft, in the words the sheet will print.
///
/// Everything here is something the app can see for itself. Whether `numeric(x)`
/// is a type and whether the new `NOT NULL` column can be added to a table with
/// rows in it are the server's questions, and it answers them better.
pub fn problems(draft: &TableDraft) -> Vec<String> {
    let mut out = Vec::new();
    if draft.name.trim().is_empty() {
        out.push("The table needs a name.".into());
    }
    let live: Vec<&ColumnDraft> = draft.columns.iter().collect();
    if live.is_empty() {
        out.push("A table needs at least one column.".into());
    }
    for (i, column) in live.iter().enumerate() {
        let name = column.name.trim();
        if name.is_empty() {
            out.push(format!("Column {} has no name.", i + 1));
            continue;
        }
        if column.type_name.trim().is_empty() {
            out.push(format!("Column \"{name}\" has no type."));
        }
        if live
            .iter()
            .take(i)
            .any(|other| other.name.trim() == name && !name.is_empty())
        {
            out.push(format!("Two columns are called \"{name}\"."));
        }
    }
    out
}

/// `CREATE TABLE`, and the comments that go with it.
///
/// The column list is aligned the way [`crate::ddl`] aligns it, because this
/// is the same text the DDL tab will show once the table exists and the two
/// disagreeing about whitespace would look like a change.
pub fn create(draft: &TableDraft) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let names: Vec<String> = draft
        .columns
        .iter()
        .map(|c| quote_ident(c.name.trim()))
        .collect();
    let types: Vec<String> = draft
        .columns
        .iter()
        .map(|c| c.type_name.trim().to_string())
        .collect();
    let name_width = crate::ddl::width(&names);
    let type_width = crate::ddl::width(&types);

    for (i, column) in draft.columns.iter().enumerate() {
        let mut line = format!(
            "{INDENT}{:name_width$} {:type_width$}",
            names[i],
            types[i],
            name_width = name_width,
            type_width = type_width
        );
        line.push_str(&column_suffix(column));
        lines.push(trim_end(line));
    }
    let key = draft.key();
    if !key.is_empty() {
        lines.push(format!("\n{INDENT}PRIMARY KEY ({})", key_list(&key)));
    }

    let mut out = vec![format!(
        "CREATE TABLE {} (\n{}\n)",
        draft.qualified(),
        lines.join(",\n")
    )];
    out.extend(comment_statements(&draft.qualified(), draft, None));
    out
}

/// Everything that has to be said to turn `before` into `after`.
///
/// Order matters and is not the order the editor lists things in:
///
/// 1. Renames first, so every statement after them can name a column once.
/// 2. Drops before adds, so a name can be freed and reused in one save.
/// 3. The key after the columns it is made of exist.
/// 4. The table's own rename last, so everything above it uses one name.
///
/// `before` is the table as it was read from the catalog, not as it was first
/// drawn: a draft is diffed against the server, and anything already true
/// there produces no statement.
pub fn alter(before: &TableDraft, after: &TableDraft) -> Vec<String> {
    let table = before.qualified();
    let mut out = Vec::new();

    // Renames. Compared trimmed, because a trailing space is a typo and not a
    // rename anybody meant.
    for column in &after.columns {
        let Some(origin) = &column.origin else {
            continue;
        };
        let name = column.name.trim();
        if name != &**origin {
            out.push(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {}",
                quote_ident(origin),
                quote_ident(name)
            ));
        }
    }

    // Drops: everything the server has that the draft no longer lists.
    let kept: Vec<&str> = after
        .columns
        .iter()
        .filter_map(|c| c.origin.as_deref())
        .collect();
    for column in &before.columns {
        let Some(origin) = &column.origin else {
            continue;
        };
        if !kept.contains(&&**origin) {
            out.push(format!(
                "ALTER TABLE {table} DROP COLUMN {}",
                quote_ident(origin)
            ));
        }
    }

    for column in after.columns.iter().filter(|c| c.is_new()) {
        let mut line = format!(
            "ALTER TABLE {table} ADD COLUMN {} {}",
            quote_ident(column.name.trim()),
            column.type_name.trim()
        );
        line.push_str(&column_suffix(column));
        out.push(trim_end(line));
    }

    // Changes to the columns that were already there.
    for column in &after.columns {
        let Some(origin) = &column.origin else {
            continue;
        };
        let Some(was) = before
            .columns
            .iter()
            .find(|c| c.origin.as_deref() == Some(&**origin))
        else {
            continue;
        };
        let name = quote_ident(column.name.trim());
        if column.type_name.trim() != was.type_name.trim() {
            out.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {name} TYPE {}",
                column.type_name.trim()
            ));
        }
        if column.nullable != was.nullable {
            out.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {name} {} NOT NULL",
                if column.nullable { "DROP" } else { "SET" }
            ));
        }
        if column.default.trim() != was.default.trim() {
            out.push(match column.default.trim() {
                "" => format!("ALTER TABLE {table} ALTER COLUMN {name} DROP DEFAULT"),
                default => {
                    format!("ALTER TABLE {table} ALTER COLUMN {name} SET DEFAULT {default}")
                }
            });
        }
    }

    // The key. Any difference at all — a column added to it, one taken out,
    // the order changed — is a drop and a re-add, because that is the only
    // thing Postgres offers.
    let key_before: Vec<String> = before
        .key()
        .iter()
        .map(|c| c.name.trim().to_string())
        .collect();
    let key_after: Vec<String> = after
        .key()
        .iter()
        .map(|c| c.name.trim().to_string())
        .collect();
    if key_before != key_after {
        if let Some(name) = &before.pk_name {
            out.push(format!(
                "ALTER TABLE {table} DROP CONSTRAINT {}",
                quote_ident(name)
            ));
        }
        if !key_after.is_empty() {
            out.push(format!(
                "ALTER TABLE {table} ADD PRIMARY KEY ({})",
                key_after
                    .iter()
                    .map(|c| quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    out.extend(comment_statements(&table, after, Some(before)));

    // The table's own name, last: everything above it named the table.
    if after.name.trim() != before.name.trim() {
        out.push(format!(
            "ALTER TABLE {table} RENAME TO {}",
            quote_ident(after.name.trim())
        ));
    }
    out
}

/// `COMMENT ON` for the table and for any column whose comment changed.
///
/// `before` is `None` when the table is being created, in which case every
/// comment there is is new.
fn comment_statements(table: &str, draft: &TableDraft, before: Option<&TableDraft>) -> Vec<String> {
    let mut out = Vec::new();
    let was_comment = before.map(|b| b.comment.trim()).unwrap_or("");
    if draft.comment.trim() != was_comment {
        out.push(format!(
            "COMMENT ON TABLE {table} IS {}",
            comment_literal(draft.comment.trim())
        ));
    }
    for column in &draft.columns {
        let was = before.and_then(|b| {
            let origin = column.origin.as_deref()?;
            b.columns
                .iter()
                .find(|c| c.origin.as_deref() == Some(origin))
                .map(|c| c.comment.trim())
        });
        // A new column in an existing table has no "before" to compare with,
        // and an empty comment on it is nothing to say.
        let was = was.unwrap_or("");
        if column.comment.trim() != was {
            out.push(format!(
                "COMMENT ON COLUMN {table}.{} IS {}",
                quote_ident(column.name.trim()),
                comment_literal(column.comment.trim())
            ));
        }
    }
    out
}

/// A comment as SQL: the empty string is `NULL`, which is how a comment is
/// taken off rather than set to nothing.
fn comment_literal(comment: &str) -> String {
    match comment {
        "" => "NULL".to_string(),
        text => quote_literal(text),
    }
}

/// Everything after the name and the type: identity, generated, default, null.
/// Shared by `CREATE TABLE` and `ADD COLUMN`, which spell it the same way.
fn column_suffix(column: &ColumnDraft) -> String {
    let mut out = String::new();
    if let Some(identity) = column.identity {
        let _ = write!(out, " GENERATED {} AS IDENTITY", identity.keyword());
    } else if column.is_generated {
        let _ = write!(
            out,
            " GENERATED ALWAYS AS ({}) STORED",
            column.default.trim()
        );
    } else if !column.default.trim().is_empty() {
        let _ = write!(out, " DEFAULT {}", column.default.trim());
    }
    if !column.nullable {
        out.push_str(" NOT NULL");
    }
    out
}

fn key_list(key: &[&ColumnDraft]) -> String {
    key.iter()
        .map(|c| quote_ident(c.name.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn trim_end(mut line: String) -> String {
    while line.ends_with(' ') {
        line.pop();
    }
    line
}

#[cfg(test)]
mod tests {
    use db::{ColumnDef, IndexDef, RelationKind, ValueKind};

    use super::*;

    fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            position: 1,
            type_name: type_name.into(),
            kind: ValueKind::Text,
            nullable,
            default: None,
            identity: None,
            is_generated: false,
            comment: None,
        }
    }

    fn users() -> Relation {
        Relation {
            reference: RelationRef::new("public", "users"),
            kind: RelationKind::Table,
            columns: vec![
                ColumnDef {
                    identity: Some(IdentityKind::ByDefault),
                    nullable: false,
                    ..column("id", "bigint", false)
                },
                column("email", "text", false),
                column("note", "text", true),
            ],
            indexes: vec![IndexDef {
                name: "users_pkey".into(),
                columns: vec!["id".into()],
                is_unique: true,
                is_primary: true,
                method: "btree".into(),
                predicate: None,
            }],
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers: Vec::new(),
            definition: None,
            create_statement: None,
            estimated_rows: 0,
            size_bytes: 0,
            comment: None,
            detail_loaded: true,
        }
    }

    /// The draft a table is opened with says nothing at all, because nothing
    /// has been typed yet.
    #[test]
    fn opening_a_table_and_saving_it_sends_nothing() {
        let relation = users();
        let draft = TableDraft::of(&relation);
        assert!(alter(&draft, &draft.clone()).is_empty());
    }

    #[test]
    fn the_key_column_comes_back_marked_as_the_key() {
        let draft = TableDraft::of(&users());
        assert!(draft.columns[0].is_pk);
        assert!(!draft.columns[1].is_pk);
        assert_eq!(draft.pk_name.as_deref(), Some("users_pkey"));
    }

    #[test]
    fn a_new_table_is_created_with_its_key_inline() {
        let mut draft = TableDraft::blank("public");
        draft.name = "invoices".into();
        draft.columns.push(ColumnDraft {
            name: "total".into(),
            type_name: "numeric(10,2)".into(),
            default: "0".into(),
            ..ColumnDraft::new()
        });
        let sql = create(&draft);
        assert_eq!(sql.len(), 1);
        assert_eq!(
            sql[0],
            "CREATE TABLE public.invoices (\n    \
             id    bigint        GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \
             total numeric(10,2) DEFAULT 0,\n\n    \
             PRIMARY KEY (id)\n)"
        );
    }

    #[test]
    fn a_comment_is_its_own_statement_and_an_empty_one_is_null() {
        let mut draft = TableDraft::blank("public");
        draft.name = "invoices".into();
        draft.comment = "What we sent".into();
        draft.columns[0].comment = "the key".into();
        let sql = create(&draft);
        assert_eq!(sql[1], "COMMENT ON TABLE public.invoices IS 'What we sent'");
        assert_eq!(sql[2], "COMMENT ON COLUMN public.invoices.id IS 'the key'");

        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.columns[1].comment = String::new();
        assert!(alter(&before, &after).is_empty());
        let mut after = before.clone();
        after.comment = String::new();
        assert!(alter(&before, &after).is_empty());
    }

    #[test]
    fn a_renamed_column_is_renamed_rather_than_replaced() {
        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.columns[1].name = "contact_email".into();
        assert_eq!(
            alter(&before, &after),
            vec!["ALTER TABLE public.users RENAME COLUMN email TO contact_email"]
        );
    }

    /// The rename lands first, so everything after it can use one name.
    #[test]
    fn a_column_renamed_and_retyped_at_once_is_altered_by_its_new_name() {
        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.columns[1].name = "contact_email".into();
        after.columns[1].type_name = "citext".into();
        after.columns[1].nullable = true;
        assert_eq!(
            alter(&before, &after),
            vec![
                "ALTER TABLE public.users RENAME COLUMN email TO contact_email",
                "ALTER TABLE public.users ALTER COLUMN contact_email TYPE citext",
                "ALTER TABLE public.users ALTER COLUMN contact_email DROP NOT NULL",
            ]
        );
    }

    #[test]
    fn a_dropped_column_frees_its_name_for_the_added_one() {
        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.columns.remove(2);
        after.columns.push(ColumnDraft {
            name: "note".into(),
            type_name: "jsonb".into(),
            ..ColumnDraft::new()
        });
        assert_eq!(
            alter(&before, &after),
            vec![
                "ALTER TABLE public.users DROP COLUMN note",
                "ALTER TABLE public.users ADD COLUMN note jsonb",
            ]
        );
    }

    #[test]
    fn a_default_set_and_taken_off_again() {
        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.columns[2].default = "'none'".into();
        assert_eq!(
            alter(&before, &after),
            vec!["ALTER TABLE public.users ALTER COLUMN note SET DEFAULT 'none'"]
        );
        let back = alter(&after, &before);
        assert_eq!(
            back,
            vec!["ALTER TABLE public.users ALTER COLUMN note DROP DEFAULT"]
        );
    }

    #[test]
    fn changing_the_key_drops_the_constraint_it_found() {
        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.columns[1].is_pk = true;
        assert_eq!(
            alter(&before, &after),
            vec![
                "ALTER TABLE public.users DROP CONSTRAINT users_pkey",
                "ALTER TABLE public.users ADD PRIMARY KEY (id, email)",
            ]
        );
    }

    /// The table's own rename is last so that every statement before it is
    /// about a table that still has the name the catalog knows.
    #[test]
    fn the_table_rename_comes_after_everything_it_would_have_broken() {
        let before = TableDraft::of(&users());
        let mut after = before.clone();
        after.name = "people".into();
        after.columns[2].nullable = false;
        assert_eq!(
            alter(&before, &after),
            vec![
                "ALTER TABLE public.users ALTER COLUMN note SET NOT NULL",
                "ALTER TABLE public.users RENAME TO people",
            ]
        );
    }

    #[test]
    fn what_the_editor_can_see_is_wrong_before_the_server_does() {
        let mut draft = TableDraft::blank("public");
        assert_eq!(problems(&draft), vec!["The table needs a name."]);

        draft.name = "invoices".into();
        draft.columns.push(ColumnDraft {
            name: "id".into(),
            type_name: "text".into(),
            ..ColumnDraft::new()
        });
        draft.columns.push(ColumnDraft::new());
        assert_eq!(
            problems(&draft),
            vec!["Two columns are called \"id\".", "Column 3 has no name."]
        );

        draft.columns.truncate(1);
        assert!(problems(&draft).is_empty());
        draft.columns.clear();
        assert_eq!(problems(&draft), vec!["A table needs at least one column."]);
    }

    /// A generated column keeps its expression where Postgres keeps it, and
    /// the editor must not turn that into a `SET DEFAULT`.
    #[test]
    fn a_generated_column_is_not_read_as_having_a_default() {
        let mut relation = users();
        relation.columns.push(ColumnDef {
            default: Some("(email || '!')".into()),
            is_generated: true,
            ..column("shout", "text", true)
        });
        let draft = TableDraft::of(&relation);
        assert_eq!(draft.columns[3].default, "");
        assert!(draft.columns[3].is_server_owned());
        assert!(alter(&draft, &draft.clone()).is_empty());
    }
}
