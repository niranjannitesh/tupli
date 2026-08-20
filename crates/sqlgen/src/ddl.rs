//! The object as SQL.
//!
//! What `\d` shows, written out as the statements that would recreate the
//! thing. Everything here reads from the snapshot the sidebar was built from —
//! no round trip, no `pg_dump`, and no chance of the DDL tab describing a
//! different table from the one the Structure tab is showing.
//!
//! It is a rendering, not a backup. Three things the catalog snapshot cannot
//! tell us, and which the output therefore does not carry:
//!
//! - **Unique constraints** come out as `CREATE UNIQUE INDEX` rather than as a
//!   `CONSTRAINT … UNIQUE`, because `pg_index` is where they are read from and
//!   both spellings land there identically. The rows they forbid are the same;
//!   the catalog name for the object is not.
//! - **Partitioning** — a partitioned parent's `PARTITION BY` clause and its
//!   children's bounds are not introspected yet, so a partitioned table renders
//!   as an ordinary one.
//! - **Storage, tablespaces, collations, ownership and grants** are all absent.
//!   This is the shape of the object, not its deployment.
//!
//! Anything that *is* here is the server's own text: check expressions and
//! trigger definitions are printed by `pg_get_constraintdef` and
//! `pg_get_triggerdef`, never reconstructed.

use std::fmt::Write as _;

use db::schema::quote_ident;
use db::{CheckConstraint, ForeignKey, IndexDef, Relation, RelationKind, TriggerDef};

/// Indent for anything inside a statement. Four spaces, matching the SQL the
/// app generates elsewhere.
pub(crate) const INDENT: &str = "    ";

/// The whole object: its `CREATE`, its indexes, its comments, its triggers.
pub fn relation(relation: &Relation) -> String {
    let mut out = match relation.kind {
        RelationKind::View | RelationKind::MaterializedView => view(relation),
        _ => table(relation),
    };
    for index in relation.indexes.iter().filter(|i| !i.is_primary) {
        out.push('\n');
        out.push_str(&create_index(relation, index));
    }
    for trigger in &relation.triggers {
        out.push('\n');
        let _ = writeln!(out, "{};", trigger.definition);
        if !trigger.enabled {
            let _ = writeln!(
                out,
                "ALTER TABLE {} DISABLE TRIGGER {};",
                relation.reference.qualified(),
                quote_ident(&trigger.name)
            );
        }
    }
    let comments = comments(relation);
    if !comments.is_empty() {
        out.push('\n');
        out.push_str(&comments);
    }
    out
}

/// `CREATE TABLE`, with the column list aligned into three columns.
///
/// Aligned rather than one-per-line-as-it-comes because this is read far more
/// often than it is run: a column list where the types line up can be scanned
/// for "which of these is nullable" in one pass.
fn table(relation: &Relation) -> String {
    let keyword = match relation.kind {
        RelationKind::Foreign => "CREATE FOREIGN TABLE",
        _ => "CREATE TABLE",
    };
    let mut out = format!("{keyword} {} (\n", relation.reference.qualified());

    let names: Vec<String> = relation
        .columns
        .iter()
        .map(|c| quote_ident(&c.name))
        .collect();
    let types: Vec<String> = relation
        .columns
        .iter()
        .map(|c| c.type_name.to_string())
        .collect();
    // A single runaway type — an array of a domain of a composite — would push
    // every other line halfway across the pane, so the pad is capped and the
    // outlier is the only line that breaks the grid.
    let name_width = width(&names);
    let type_width = width(&types);

    let mut lines: Vec<String> = Vec::new();
    for (i, column) in relation.columns.iter().enumerate() {
        let mut line = format!(
            "{INDENT}{:name_width$} {:type_width$}",
            names[i],
            types[i],
            name_width = name_width,
            type_width = type_width
        );
        if let Some(identity) = column.identity {
            let _ = write!(line, " GENERATED {} AS IDENTITY", identity.keyword());
        } else if column.is_generated {
            // The expression lives in the default slot; for a generated column
            // that is what it means.
            let _ = write!(
                line,
                " GENERATED ALWAYS AS ({}) STORED",
                column.default.as_deref().unwrap_or("")
            );
        } else if let Some(default) = &column.default {
            let _ = write!(line, " DEFAULT {default}");
        }
        if !column.nullable {
            line.push_str(" NOT NULL");
        }
        lines.push(trim_end(line));
    }

    let constraints = constraints(relation);
    let mut body = lines.join(",\n");
    if !constraints.is_empty() {
        body.push_str(",\n\n");
        body.push_str(&constraints.join(",\n"));
    }
    out.push_str(&body);
    out.push_str("\n);\n");
    out
}

/// Table-level constraints, in the order they are worth reading: the key, then
/// what the rows must satisfy, then what they point at.
fn constraints(relation: &Relation) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(pk) = relation.primary_key() {
        out.push(format!(
            "{INDENT}CONSTRAINT {} PRIMARY KEY ({})",
            quote_ident(&pk.name),
            column_list(&pk.columns)
        ));
    }
    out.extend(relation.checks.iter().map(check_constraint));
    out.extend(relation.foreign_keys.iter().map(foreign_key));
    out
}

fn check_constraint(check: &CheckConstraint) -> String {
    // `definition` is already `CHECK (…)`, straight from the server.
    format!(
        "{INDENT}CONSTRAINT {} {}",
        quote_ident(&check.name),
        check.definition
    )
}

fn foreign_key(fk: &ForeignKey) -> String {
    let mut out = format!(
        "{INDENT}CONSTRAINT {} FOREIGN KEY ({})\n{INDENT}{INDENT}REFERENCES {} ({})",
        quote_ident(&fk.name),
        column_list(&fk.columns),
        fk.target.qualified(),
        column_list(&fk.target_columns)
    );
    // `NO ACTION` is the default and saying so adds a line to every foreign key
    // in the database without adding a fact to any of them.
    if let Some(action) = action_words(fk.on_delete) {
        let _ = write!(out, "\n{INDENT}{INDENT}ON DELETE {action}");
    }
    if let Some(action) = action_words(fk.on_update) {
        let _ = write!(out, "\n{INDENT}{INDENT}ON UPDATE {action}");
    }
    out
}

fn action_words(action: db::RefAction) -> Option<&'static str> {
    match action {
        db::RefAction::NoAction => None,
        db::RefAction::Restrict => Some("RESTRICT"),
        db::RefAction::Cascade => Some("CASCADE"),
        db::RefAction::SetNull => Some("SET NULL"),
        db::RefAction::SetDefault => Some("SET DEFAULT"),
    }
}

fn create_index(relation: &Relation, index: &IndexDef) -> String {
    let mut out = format!(
        "CREATE {}INDEX {}\n{INDENT}ON {} USING {} ({})",
        if index.is_unique { "UNIQUE " } else { "" },
        quote_ident(&index.name),
        relation.reference.qualified(),
        index.method,
        // Index columns arrive already rendered by `pg_get_indexdef` — they can
        // be `lower(email)` or `created_at DESC`, neither of which is an
        // identifier to quote.
        index.columns.join(", ")
    );
    if let Some(predicate) = &index.predicate {
        let _ = write!(out, "\n{INDENT}WHERE {predicate}");
    }
    out.push_str(";\n");
    out
}

fn view(relation: &Relation) -> String {
    let keyword = match relation.kind {
        RelationKind::MaterializedView => "CREATE MATERIALIZED VIEW",
        _ => "CREATE VIEW",
    };
    let body = relation
        .definition
        .as_deref()
        .unwrap_or("-- definition unavailable")
        .trim_end()
        .trim_end_matches(';');
    format!("{keyword} {} AS\n{body};\n", relation.reference.qualified())
}

/// `COMMENT ON` for the object and for every column that has one.
fn comments(relation: &Relation) -> String {
    let object = match relation.kind {
        RelationKind::View => "VIEW",
        RelationKind::MaterializedView => "MATERIALIZED VIEW",
        RelationKind::Foreign => "FOREIGN TABLE",
        _ => "TABLE",
    };
    let name = relation.reference.qualified();
    let mut out = String::new();
    if let Some(comment) = &relation.comment {
        let _ = writeln!(
            out,
            "COMMENT ON {object} {name} IS {};",
            db::schema::quote_literal(comment)
        );
    }
    for column in relation.columns.iter() {
        if let Some(comment) = &column.comment {
            let _ = writeln!(
                out,
                "COMMENT ON COLUMN {name}.{} IS {};",
                quote_ident(&column.name),
                db::schema::quote_literal(comment)
            );
        }
    }
    out
}

/// The keyword the object is called by in a `DROP` or an `ALTER`.
///
/// A view dropped with `DROP TABLE` is an error, not a near miss, so this is
/// read off the catalog rather than assumed. A foreign table is a `FOREIGN
/// TABLE` for both verbs; a partitioned parent is an ordinary `TABLE`.
pub fn object_keyword(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::View => "VIEW",
        RelationKind::MaterializedView => "MATERIALIZED VIEW",
        RelationKind::Foreign => "FOREIGN TABLE",
        RelationKind::Table | RelationKind::Partitioned => "TABLE",
    }
}

/// `ALTER TABLE public.users RENAME TO people`.
///
/// The new name is bare, not qualified: `RENAME TO` cannot move an object
/// between schemas, and a schema written there is an error rather than a move.
pub fn rename(reference: &db::RelationRef, kind: RelationKind, to: &str) -> String {
    format!(
        "ALTER {} {} RENAME TO {}",
        object_keyword(kind),
        reference.qualified(),
        quote_ident(to)
    )
}

/// `TRUNCATE TABLE public.users`.
///
/// `RESTART IDENTITY` is offered because emptying a table and then having the
/// next row come back as id 4391 is rarely what was meant; `CASCADE` is offered
/// because without it a table that anything references cannot be truncated at
/// all. Both are off unless asked for — each destroys more than the bare
/// statement does.
pub fn truncate(reference: &db::RelationRef, restart_identity: bool, cascade: bool) -> String {
    let mut out = format!("TRUNCATE TABLE {}", reference.qualified());
    if restart_identity {
        out.push_str(" RESTART IDENTITY");
    }
    if cascade {
        out.push_str(" CASCADE");
    }
    out
}

/// `DROP TABLE public.users`.
///
/// No `IF EXISTS`: this is generated from a catalog read, so the object was
/// there a moment ago, and swallowing "it is already gone" would hide the one
/// case worth seeing — that something else dropped it first.
pub fn drop_object(reference: &db::RelationRef, kind: RelationKind, cascade: bool) -> String {
    let mut out = format!("DROP {} {}", object_keyword(kind), reference.qualified());
    if cascade {
        out.push_str(" CASCADE");
    }
    out
}

/// The trigger list on its own, for the structure tab's summary line.
pub fn trigger_summary(trigger: &TriggerDef) -> String {
    format!("{} → {}", trigger.timing, trigger.function)
}

fn column_list(columns: &[std::sync::Arc<str>]) -> String {
    columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The pad width for a column of strings: the longest, but never wide enough
/// that one outlier ruins the alignment for everything else.
pub(crate) fn width(items: &[String]) -> usize {
    const CAP: usize = 24;
    items
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0)
        .min(CAP)
}

fn trim_end(mut line: String) -> String {
    while line.ends_with(' ') {
        line.pop();
    }
    line
}

#[cfg(test)]
mod tests {
    use db::{
        CheckConstraint, ColumnDef, ForeignKey, IdentityKind, IndexDef, RefAction, Relation,
        RelationKind, RelationRef, TriggerDef, ValueKind,
    };

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

    fn table(columns: Vec<ColumnDef>) -> Relation {
        Relation {
            reference: RelationRef::new("public", "users"),
            kind: RelationKind::Table,
            columns,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers: Vec::new(),
            definition: None,
            estimated_rows: 0,
            size_bytes: 0,
            comment: None,
            detail_loaded: true,
        }
    }

    #[test]
    fn a_table_renders_its_columns_aligned_and_its_key_as_a_constraint() {
        let mut t = table(vec![
            ColumnDef {
                identity: Some(IdentityKind::Always),
                ..column("id", "bigint", false)
            },
            column("email", "text", false),
            column("full_name", "text", true),
        ]);
        t.indexes.push(IndexDef {
            name: "users_pkey".into(),
            columns: vec!["id".into()],
            is_unique: true,
            is_primary: true,
            method: "btree".into(),
            predicate: None,
        });

        assert_eq!(
            relation(&t),
            "CREATE TABLE public.users (\n\
             \x20   id        bigint GENERATED ALWAYS AS IDENTITY NOT NULL,\n\
             \x20   email     text   NOT NULL,\n\
             \x20   full_name text,\n\
             \n\
             \x20   CONSTRAINT users_pkey PRIMARY KEY (id)\n\
             );\n"
        );
    }

    #[test]
    fn a_default_and_a_generated_column_are_told_apart_by_their_flags() {
        let t = table(vec![
            ColumnDef {
                default: Some("'free'".into()),
                ..column("plan", "text", false)
            },
            ColumnDef {
                default: Some("(mrr_cents * 12)".into()),
                is_generated: true,
                ..column("arr_cents", "bigint", true)
            },
        ]);
        let sql = relation(&t);
        assert!(
            sql.contains("plan      text   DEFAULT 'free' NOT NULL"),
            "{sql}"
        );
        assert!(
            sql.contains("arr_cents bigint GENERATED ALWAYS AS ((mrr_cents * 12)) STORED"),
            "{sql}"
        );
    }

    #[test]
    fn a_foreign_key_says_nothing_about_the_actions_it_leaves_at_the_default() {
        let mut t = table(vec![column("organization_id", "uuid", false)]);
        t.foreign_keys.push(ForeignKey {
            name: "users_organization_id_fkey".into(),
            columns: vec!["organization_id".into()],
            target: RelationRef::new("public", "organizations"),
            target_columns: vec!["id".into()],
            on_delete: RefAction::Cascade,
            on_update: RefAction::NoAction,
        });
        let sql = relation(&t);
        assert!(sql.contains("ON DELETE CASCADE"), "{sql}");
        assert!(!sql.contains("ON UPDATE"), "{sql}");
    }

    #[test]
    fn a_check_is_the_servers_own_text_and_is_not_wrapped_twice() {
        let mut t = table(vec![column("price", "numeric", false)]);
        t.checks.push(CheckConstraint {
            name: "price_positive".into(),
            definition: "CHECK ((price > (0)::numeric))".into(),
        });
        assert!(relation(&t).contains("CONSTRAINT price_positive CHECK ((price > (0)::numeric))"));
    }

    #[test]
    fn indexes_follow_the_table_and_a_partial_one_keeps_its_predicate() {
        let mut t = table(vec![column("email", "text", false)]);
        t.indexes.push(IndexDef {
            name: "users_active".into(),
            columns: vec!["lower(email)".into()],
            is_unique: true,
            is_primary: false,
            method: "btree".into(),
            predicate: Some("(is_active)".into()),
        });
        let sql = relation(&t);
        assert!(sql.contains("CREATE UNIQUE INDEX users_active"), "{sql}");
        assert!(sql.contains("USING btree (lower(email))"), "{sql}");
        assert!(sql.contains("WHERE (is_active)"), "{sql}");
    }

    #[test]
    fn a_view_is_its_definition_with_one_semicolon() {
        let mut v = table(vec![column("id", "bigint", false)]);
        v.kind = RelationKind::View;
        v.reference = RelationRef::new("public", "active_users");
        v.definition = Some(" SELECT id\n   FROM users\n  WHERE is_active;".into());
        assert_eq!(
            relation(&v),
            "CREATE VIEW public.active_users AS\n \
             SELECT id\n   FROM users\n  WHERE is_active;\n"
        );
    }

    #[test]
    fn a_disabled_trigger_is_shown_and_then_disabled_again() {
        let mut t = table(vec![column("id", "bigint", false)]);
        t.triggers.push(TriggerDef {
            name: "touch".into(),
            definition: "CREATE TRIGGER touch BEFORE UPDATE ON public.users \
                         FOR EACH ROW EXECUTE FUNCTION touch()"
                .into(),
            timing: "BEFORE UPDATE".into(),
            function: "public.touch".into(),
            enabled: false,
        });
        let sql = relation(&t);
        assert!(sql.contains("CREATE TRIGGER touch BEFORE UPDATE"), "{sql}");
        assert!(
            sql.contains("ALTER TABLE public.users DISABLE TRIGGER touch;"),
            "{sql}"
        );
    }

    #[test]
    fn comments_come_last_and_are_quoted_as_literals() {
        let mut t = table(vec![ColumnDef {
            comment: Some("it's the key".into()),
            ..column("id", "bigint", false)
        }]);
        t.comment = Some("people".into());
        let sql = relation(&t);
        assert!(
            sql.contains("COMMENT ON TABLE public.users IS 'people';"),
            "{sql}"
        );
        assert!(
            sql.contains("COMMENT ON COLUMN public.users.id IS 'it''s the key';"),
            "{sql}"
        );
    }

    #[test]
    fn a_reserved_word_column_is_quoted() {
        let t = table(vec![column("order", "int4", true)]);
        assert!(relation(&t).contains("\"order\""));
    }

    #[test]
    fn an_object_is_altered_and_dropped_by_its_own_keyword() {
        let reference = RelationRef::new("public", "users");
        assert_eq!(
            rename(&reference, RelationKind::MaterializedView, "people"),
            "ALTER MATERIALIZED VIEW public.users RENAME TO people"
        );
        assert_eq!(
            drop_object(&reference, RelationKind::View, false),
            "DROP VIEW public.users"
        );
        assert_eq!(
            drop_object(&reference, RelationKind::Partitioned, true),
            "DROP TABLE public.users CASCADE"
        );
    }

    #[test]
    fn a_name_that_needs_quoting_gets_it_on_both_sides() {
        let reference = RelationRef::new("public", "select");
        assert_eq!(
            rename(&reference, RelationKind::Table, "Order Items"),
            r#"ALTER TABLE public."select" RENAME TO "Order Items""#
        );
    }

    #[test]
    fn truncate_says_only_what_it_was_asked_to() {
        let reference = RelationRef::new("public", "events");
        assert_eq!(
            truncate(&reference, false, false),
            "TRUNCATE TABLE public.events"
        );
        assert_eq!(
            truncate(&reference, true, true),
            "TRUNCATE TABLE public.events RESTART IDENTITY CASCADE"
        );
    }
}
