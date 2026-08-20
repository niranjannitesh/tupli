//! What the SQL console offers to complete.
//!
//! The editor knows there is a word under the cursor and that something can be
//! asked about it; this is the something. It reads the same catalog the sidebar
//! draws, so the console and the tree can never disagree about what exists.
//!
//! Two things make the offers worth having rather than merely present. The
//! first is that the `from` clause is read: with `select … from orders o`, the
//! bare word being typed is completed against *orders*' columns before anything
//! else in the database, and `o.` is completed against them alone. The second
//! is that nothing is offered for a table nobody named — a database with four
//! hundred tables has ten thousand columns, and a list of all of them is a list
//! of none of them.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use db::SchemaSnapshot;
use editor::completion::{Completion, CompletionContext, CompletionKind, CompletionSource};

/// The catalog behind completion, shared by every editor in the window.
///
/// One handle is installed in each editor as it is built and never replaced:
/// re-reading the schema swaps what is inside, which is why this is a shared
/// cell rather than a snapshot copied into each one. An editor that had been
/// handed a copy would go on completing against a schema that had been dropped.
#[derive(Clone, Default)]
pub struct Catalog {
    snapshot: Rc<RefCell<Option<Arc<SchemaSnapshot>>>>,
}

impl Catalog {
    pub fn set(&self, snapshot: Option<Arc<SchemaSnapshot>>) {
        *self.snapshot.borrow_mut() = snapshot;
    }
}

/// A table named in the statement being typed, and what it is called there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InPlay {
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

impl InPlay {
    /// Whether `word` refers to this table: its alias if it has one, and
    /// otherwise its name. A table with an alias is *only* reachable by the
    /// alias, which is what Postgres itself does.
    fn answers_to(&self, word: &str) -> bool {
        match &self.alias {
            Some(alias) => alias.eq_ignore_ascii_case(word),
            None => self.name.eq_ignore_ascii_case(word),
        }
    }
}

/// The tables a statement has brought into scope, in the order they appear.
///
/// A word-level scan rather than a parse. The statement being completed is by
/// definition half-typed, and half-typed SQL is exactly what a parser cannot
/// read — but `from` is still followed by a table name in text that no grammar
/// would accept.
fn tables_in_play(text: &str) -> Vec<InPlay> {
    let mut out: Vec<InPlay> = Vec::new();
    // Commas survive the split: they are what says a table list continues, and
    // `from a, b` has to put both in play.
    let mut words = text
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter(|w| !w.is_empty())
        .peekable();

    while let Some(word) = words.next() {
        let introducer = word.to_ascii_lowercase();
        if !matches!(introducer.as_str(), "from" | "join" | "update" | "into") {
            continue;
        }
        // `insert into users (id)` names its columns straight after the table,
        // so a bare word there is not an alias. Postgres only accepts one on an
        // insert with an explicit `as`, and that is handled below.
        let bare_alias = introducer != "into";

        loop {
            let Some(name) = words.peek() else { break };
            // A comma anywhere in the entry — on the name, on the alias, or
            // standing on its own — says the list goes on.
            let mut more = name.ends_with(',');
            let Some(mut table) = parse_name(name) else {
                break;
            };
            words.next();

            // `orders o` and `orders as o`. A keyword here is the next clause
            // starting, not an alias.
            if !more {
                if let Some(next) = words.peek() {
                    let lowered = next.to_ascii_lowercase();
                    if lowered == "as" {
                        words.next();
                        if let Some(alias) = words.next() {
                            more = alias.ends_with(',');
                            table.alias = Some(clean(alias).to_string());
                        }
                    } else if bare_alias
                        && !is_clause_word(lowered.trim_end_matches(','))
                        && parse_name(next).is_some()
                    {
                        let alias = words.next().expect("peeked");
                        more = alias.ends_with(',');
                        table.alias = Some(clean(alias).to_string());
                    }
                }
            }
            if !out.contains(&table) {
                out.push(table);
            }
            if words.peek().is_some_and(|w| *w == ",") {
                words.next();
                more = true;
            }
            if !more {
                break;
            }
        }
    }
    out
}

/// A name with the punctuation SQL allows around it taken off.
fn clean(word: &str) -> &str {
    word.trim_end_matches(&[',', ';'][..]).trim_matches('"')
}

/// A dotted name, if `word` is one. `"Odd Name"` keeps its quotes off.
fn parse_name(word: &str) -> Option<InPlay> {
    let word = word.trim_end_matches(&[',', ';'][..]);
    if word.is_empty() {
        return None;
    }
    let mut parts = word.split('.').map(clean);
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() {
        // `db.schema.table` — more than this app connects to at once.
        return None;
    }
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == ' ' || c == '-')
            && !s.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    match second {
        Some(name) if ok(first) && ok(name) => Some(InPlay {
            schema: Some(first.to_string()),
            name: name.to_string(),
            alias: None,
        }),
        None if ok(first) && !is_clause_word(&first.to_ascii_lowercase()) => Some(InPlay {
            schema: None,
            name: first.to_string(),
            alias: None,
        }),
        _ => None,
    }
}

/// Words that start a clause, which can therefore never be a table's alias.
fn is_clause_word(lowered: &str) -> bool {
    matches!(
        lowered,
        "select"
            | "from"
            | "where"
            | "join"
            | "inner"
            | "left"
            | "right"
            | "full"
            | "outer"
            | "cross"
            | "lateral"
            | "on"
            | "using"
            | "group"
            | "order"
            | "having"
            | "limit"
            | "offset"
            | "union"
            | "set"
            | "values"
            | "returning"
            | "as"
            | "and"
            | "or"
            | "not"
            | "with"
    )
}

impl CompletionSource for Catalog {
    fn completions(&self, context: &CompletionContext) -> Vec<Completion> {
        let borrowed = self.snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            // Not connected: the keywords are still true.
            return keywords();
        };

        // Only the statement the cursor is in. A script's third statement has
        // nothing to do with the first one's tables.
        let statement = editor::sql::statement_at(&context.text, context.offset);
        let text: String = context
            .text
            .chars()
            .skip(statement.start)
            .take(statement.len())
            .collect();
        let in_play = tables_in_play(&text);

        // ---- qualified: `o.`, `orders.`, `public.` -----------------------
        if let Some(qualifier) = context.qualifier.as_deref() {
            // An alias or a table named in the statement.
            if let Some(table) = in_play.iter().find(|t| t.answers_to(qualifier)) {
                return columns_of(snapshot, table);
            }
            // `public.` — the schema's own relations.
            if let Some(schema) = snapshot.schemas.iter().find(|s| &*s.name == qualifier) {
                return schema
                    .relations
                    .iter()
                    .map(|relation| relation_completion(relation, false))
                    .collect();
            }
            // `public.orders.` — a table nobody put in the from clause.
            if let Some((schema, name)) = qualifier.split_once('.') {
                return columns_of(
                    snapshot,
                    &InPlay {
                        schema: Some(schema.to_string()),
                        name: name.to_string(),
                        alias: None,
                    },
                );
            }
            // A table that exists but was not named: still worth answering, or
            // the dot after a table you just typed offers nothing.
            return columns_of(
                snapshot,
                &InPlay {
                    schema: None,
                    name: qualifier.to_string(),
                    alias: None,
                },
            );
        }

        // ---- unqualified --------------------------------------------------
        let mut out = columns_in_play(snapshot, &in_play);
        // Relations on the search path go in unqualified; everything else is
        // offered with its schema in front, because that is what would have to
        // be typed for it to work.
        for schema in snapshot.schemas.iter().filter(|s| !s.is_system) {
            let on_path = snapshot.search_path.iter().any(|p| *p == schema.name);
            for relation in &schema.relations {
                out.push(relation_completion(relation, !on_path));
            }
            for routine in &schema.routines {
                out.push(
                    Completion::new(routine.name.to_string(), CompletionKind::Function)
                        .detail(routine.returns.to_string()),
                );
            }
        }
        for schema in snapshot.schemas.iter().filter(|s| !s.is_system) {
            out.push(Completion::new(
                schema.name.to_string(),
                CompletionKind::Schema,
            ));
        }
        out.extend(keywords());
        out
    }
}

fn relation_completion(relation: &db::Relation, qualify: bool) -> Completion {
    let kind = match relation.kind.is_view() {
        true => CompletionKind::View,
        false => CompletionKind::Table,
    };
    let label = relation.reference.name.to_string();
    let completion = Completion::new(label, kind).detail(relation.reference.schema.to_string());
    match qualify {
        true => completion.insert(relation.reference.qualified()),
        false => completion,
    }
}

/// The columns of every table the statement named.
///
/// One table and a column name says everything about itself. Two, and it stops:
/// `id` could be either table's, and Postgres does not guess — a bare `id` that
/// two tables in the query both have is an error, not a coin toss. So with more
/// than one table in play the source is written into the detail, and a name
/// they share inserts itself qualified, which is what would have had to be
/// typed anyway.
fn columns_in_play(snapshot: &SchemaSnapshot, in_play: &[InPlay]) -> Vec<Completion> {
    let tables: Vec<(String, Vec<Completion>)> = in_play
        .iter()
        .map(|table| {
            let word = table.alias.clone().unwrap_or_else(|| table.name.clone());
            (word, columns_of(snapshot, table))
        })
        .collect();
    if let [(_, columns)] = &tables[..] {
        return columns.clone();
    }

    let mut seen: Vec<&str> = Vec::new();
    let mut shared: Vec<&str> = Vec::new();
    for (_, columns) in &tables {
        for column in columns {
            match seen.contains(&column.label.as_ref()) {
                true => shared.push(column.label.as_ref()),
                false => seen.push(column.label.as_ref()),
            }
        }
    }
    let shared: Vec<String> = shared.into_iter().map(str::to_string).collect();

    let mut out = Vec::new();
    for (word, columns) in &tables {
        for column in columns {
            let detail = match &column.detail {
                Some(detail) => format!("{word} · {detail}"),
                None => word.clone(),
            };
            let mut completion = column.clone().detail(detail);
            if shared.iter().any(|name| name == column.label.as_ref()) {
                completion = completion.insert(format!("{word}.{}", column.label));
            }
            out.push(completion);
        }
    }
    out
}

/// A table's columns, in the order the table has them — which is the order they
/// mean something in, and never alphabetical.
fn columns_of(snapshot: &SchemaSnapshot, table: &InPlay) -> Vec<Completion> {
    let relation = match &table.schema {
        Some(schema) => snapshot
            .schema(schema)
            .and_then(|schema| schema.relation(&table.name)),
        // Unqualified: the search path decides, exactly as it would for the
        // query itself.
        None => snapshot
            .search_path
            .iter()
            .filter_map(|name| snapshot.schema(name))
            .find_map(|schema| schema.relation(&table.name))
            .or_else(|| {
                snapshot
                    .schemas
                    .iter()
                    .find_map(|schema| schema.relation(&table.name))
            }),
    };
    let Some(relation) = relation else {
        return Vec::new();
    };
    relation
        .columns
        .iter()
        .map(|column| {
            Completion::new(column.name.to_string(), CompletionKind::Column)
                .detail(column.type_name.to_string())
        })
        .collect()
}

fn keywords() -> Vec<Completion> {
    editor::sql::KEYWORDS
        .iter()
        .map(|word| Completion::new(*word, CompletionKind::Keyword))
        .chain(
            editor::sql::FUNCTIONS
                .iter()
                .map(|word| Completion::new(*word, CompletionKind::Function)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<(String, Option<String>)> {
        tables_in_play(text)
            .into_iter()
            .map(|t| (t.name, t.alias))
            .collect()
    }

    #[test]
    fn a_from_clause_puts_its_table_in_play() {
        assert_eq!(
            names("select * from orders"),
            [("orders".to_string(), None)]
        );
    }

    #[test]
    fn an_alias_is_what_the_table_answers_to() {
        assert_eq!(
            names("select * from orders o join users as u on u.id = o.user_id"),
            [
                ("orders".to_string(), Some("o".to_string())),
                ("users".to_string(), Some("u".to_string())),
            ]
        );
        let tables = tables_in_play("select * from orders o");
        assert!(tables[0].answers_to("o"));
        // Aliased, so the name itself no longer refers to it — which is what
        // Postgres says too.
        assert!(!tables[0].answers_to("orders"));
    }

    #[test]
    fn a_clause_word_is_not_an_alias() {
        assert_eq!(
            names("select * from orders where id = 1"),
            [("orders".to_string(), None)]
        );
        assert_eq!(
            names("select * from orders order by id"),
            [("orders".to_string(), None)]
        );
    }

    #[test]
    fn a_qualified_table_keeps_its_schema() {
        let tables = tables_in_play("select * from analytics.events e");
        assert_eq!(tables[0].schema.as_deref(), Some("analytics"));
        assert_eq!(tables[0].name, "events");
        assert_eq!(tables[0].alias.as_deref(), Some("e"));
    }

    #[test]
    fn a_half_typed_statement_still_reads() {
        // The case that matters: this is what the text looks like at the exact
        // moment completion is asked for.
        assert_eq!(
            names("select  from customers c where c."),
            [("customers".to_string(), Some("c".to_string()))]
        );
    }

    #[test]
    fn an_insert_and_an_update_name_a_table_too() {
        assert_eq!(
            names("insert into users (id) values (1)"),
            [("users".to_string(), None)]
        );
        assert_eq!(
            names("update users set id = 1"),
            [("users".to_string(), None)]
        );
    }

    #[test]
    fn a_comma_continues_the_table_list() {
        assert_eq!(
            names("select * from orders, users"),
            [("orders".to_string(), None), ("users".to_string(), None)]
        );
        assert_eq!(
            names("select * from orders o, users u"),
            [
                ("orders".to_string(), Some("o".to_string())),
                ("users".to_string(), Some("u".to_string())),
            ]
        );
    }

    /// Two tables, `id` in both and `email` in only one.
    fn catalog() -> Catalog {
        use db::{ColumnDef, Relation, RelationKind, Schema, ValueKind};

        fn column(position: i16, name: &str, type_name: &str) -> ColumnDef {
            ColumnDef {
                name: Arc::from(name),
                position,
                type_name: Arc::from(type_name),
                kind: ValueKind::Text,
                nullable: false,
                default: None,
                identity: None,
                is_generated: false,
                comment: None,
            }
        }

        fn table(name: &str, columns: Vec<ColumnDef>) -> Relation {
            Relation {
                reference: db::RelationRef::new(Arc::from("public"), Arc::from(name)),
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

        let catalog = Catalog::default();
        catalog.set(Some(Arc::new(SchemaSnapshot {
            database: Arc::from("app"),
            databases: vec![Arc::from("app")],
            server_version: Arc::from("17.0"),
            search_path: vec![Arc::from("public")],
            current_schema: Arc::from("public"),
            schemas: vec![Schema {
                name: Arc::from("public"),
                owner: Arc::from("postgres"),
                is_system: false,
                relations: vec![
                    table(
                        "orders",
                        vec![
                            column(1, "id", "bigint"),
                            column(2, "user_id", "bigint"),
                            column(3, "amount", "numeric"),
                        ],
                    ),
                    table(
                        "users",
                        vec![column(1, "id", "bigint"), column(2, "email", "text")],
                    ),
                ],
                routines: Vec::new(),
            }],
        })));
        catalog
    }

    fn offers(catalog: &Catalog, text: &str) -> Vec<Completion> {
        let offset = text.chars().count();
        let (range, qualifier) = editor::completion::word_at(text, offset);
        let prefix: String = text.chars().skip(range.start).take(range.len()).collect();
        catalog.completions(&CompletionContext {
            text: text.into(),
            offset,
            prefix,
            qualifier,
            explicit: false,
        })
    }

    #[test]
    fn an_alias_offers_its_own_columns_in_table_order() {
        let labels: Vec<_> = offers(&catalog(), "select  from orders o where o.")
            .iter()
            .map(|c| c.label.to_string())
            .collect();
        assert_eq!(labels, ["id", "user_id", "amount"]);
    }

    #[test]
    fn a_column_two_tables_share_completes_qualified() {
        let offers = offers(&catalog(), "select  from orders o join users u on ");
        let id: Vec<_> = offers
            .iter()
            .filter(|c| c.label.as_ref() == "id")
            .map(|c| c.text().to_string())
            .collect();
        // One from each table, and each says which — a bare `id` here is an
        // ambiguity error, not a guess Postgres makes.
        assert_eq!(id, ["o.id", "u.id"]);
        // A name only one of them has stays as it was typed.
        let email = offers
            .iter()
            .find(|c| c.label.as_ref() == "email")
            .expect("users.email is on offer");
        assert_eq!(email.text(), "email");
        assert_eq!(email.detail.as_deref(), Some("u · text"));
    }

    #[test]
    fn a_disconnected_catalog_still_offers_keywords() {
        let catalog = Catalog::default();
        let offers = catalog.completions(&CompletionContext {
            text: "sel".into(),
            offset: 3,
            prefix: "sel".into(),
            qualifier: None,
            explicit: false,
        });
        assert!(offers.iter().any(|c| c.label.as_ref() == "select"));
    }
}
