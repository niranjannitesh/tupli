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
use editor::hover::{HoverContext, HoverInfo, HoverSource};

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

/// Words and lone punctuation marks, in order.
///
/// Enough of a tokeniser for the two questions below and no more. SQL it reads
/// wrong costs someone one wrong completion list, which is the budget this
/// whole file works to.
fn tokens(text: &str) -> Vec<&str> {
    let is_name = |c: char| c.is_alphanumeric() || c == '_' || c == '"';
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let mut end = at + c.len_utf8();
        if is_name(c) {
            while let Some((next, c)) = chars.peek().copied() {
                if !is_name(c) {
                    break;
                }
                end = next + c.len_utf8();
                chars.next();
            }
        }
        out.push(&text[at..end]);
    }
    out
}

/// Whether the cursor is somewhere a table name goes.
///
/// The difference matters more than any amount of ranking does: after `from `
/// the columns of the table you already named are the one thing that cannot go
/// there, and they were the first forty rows of the list.
///
/// Read backwards from the cursor and stop at the first token that settles it.
/// Names, commas and dots are all still part of a table list, so the scan walks
/// over them — which is what gets `from a, b, |` right. Anything else — an
/// open bracket, an operator, any other clause word — means the from-list, if
/// there ever was one, is behind us.
fn wants_a_table(before: &str) -> bool {
    for token in tokens(before).into_iter().rev() {
        match token.to_ascii_lowercase().as_str() {
            "from" | "join" | "into" | "update" => return true,
            "," | "." => continue,
            word if word
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '"') =>
            {
                // A name, unless it is a word that starts a clause of its own.
                if is_clause_word(word) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    false
}

/// The names a `with` clause invents.
///
/// They are tables for the length of one statement and exist nowhere else, so
/// the catalog has never heard of them — which meant that until now the one
/// kind of table the query itself defined was the one kind you had to type out
/// in full.
fn ctes(text: &str) -> Vec<String> {
    let tokens = tokens(text);
    if !tokens
        .first()
        .is_some_and(|w| w.eq_ignore_ascii_case("with"))
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    for window in tokens.windows(3) {
        let [name, as_, open] = window else { continue };
        if !as_.eq_ignore_ascii_case("as") || *open != "(" {
            continue;
        }
        let name = name.trim_matches('"');
        // `select count(*) as ( ` is not a thing, but `x as (` inside a
        // sub-select is, and so is a column alias immediately before one.
        // Requiring a plain name is as far as a scan of this kind can go.
        if !name.is_empty() && !is_clause_word(&name.to_ascii_lowercase()) {
            out.push(name.to_string());
        }
    }
    out.dedup();
    out
}

impl CompletionSource for Catalog {
    fn completions(&self, context: &CompletionContext) -> Vec<Completion> {
        let borrowed = self.snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            // Not connected: the keywords are still true.
            return keywords(&context.prefix);
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
        let ctes = ctes(&text);

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
        // Where the cursor is decides what half of the database can go there.
        // Ranking alone cannot do this job: after `from ` the columns are not
        // merely less likely than the tables, they are the one thing that
        // cannot appear, and forty of them stood between the cursor and the
        // answer.
        let word_start = context.offset - context.prefix.chars().count();
        let before: String = context
            .text
            .chars()
            .skip(statement.start)
            .take(word_start.saturating_sub(statement.start))
            .collect();
        let naming_a_table = wants_a_table(&before);

        let mut out = Vec::new();
        // A name the statement invented goes first: there are two of them and
        // four hundred tables, and it is the one the person just wrote down.
        out.extend(
            ctes.iter()
                .map(|name| Completion::new(name.clone(), CompletionKind::Table).detail("cte")),
        );
        if !naming_a_table {
            out.extend(columns_in_play(snapshot, &in_play));
        }
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
        // No keywords after `from`: nothing that starts a clause can stand
        // where a table name is expected, and the ones that can — `lateral`,
        // a bracketed sub-select — are not words anybody needs help spelling.
        if !naming_a_table {
            out.extend(keywords(&context.prefix));
        }
        out
    }
}

// ---- hover -----------------------------------------------------------------

impl HoverSource for Catalog {
    fn hover(&self, context: &HoverContext) -> Option<HoverInfo> {
        let borrowed = self.snapshot.borrow();
        let snapshot = borrowed.as_ref()?;

        // The statement the pointer is in, for the same reason completion reads
        // it: the aliases of one statement mean nothing in the next.
        let statement = editor::sql::statement_at(&context.text, context.offset);
        let text: String = context
            .text
            .chars()
            .skip(statement.start)
            .take(statement.len())
            .collect();
        let in_play = tables_in_play(&text);
        let word = context.word.trim_matches('"');

        if let Some(qualifier) = context.qualifier.as_deref() {
            // `o.amount` — the alias's table, then the column on it.
            if let Some(table) = in_play.iter().find(|t| t.answers_to(qualifier)) {
                return column_hover(snapshot, table, word);
            }
            // `public.orders` — a relation named through its schema.
            if let Some(relation) = snapshot.schema(qualifier).and_then(|s| s.relation(word)) {
                return Some(relation_hover(relation));
            }
            // `public.orders.amount`.
            if let Some((schema, name)) = qualifier.split_once('.') {
                let table = InPlay {
                    schema: Some(schema.to_string()),
                    name: name.to_string(),
                    alias: None,
                };
                return column_hover(snapshot, &table, word);
            }
            // `orders.amount`, where nobody put `orders` in a from clause.
            let table = InPlay {
                schema: None,
                name: qualifier.to_string(),
                alias: None,
            };
            return column_hover(snapshot, &table, word);
        }

        // A name the statement itself introduced: an alias, or the table it is
        // an alias for. Both describe the table, which is the question — `o` on
        // its own means nothing until you know what it stands for.
        if let Some(table) = in_play.iter().find(|t| t.answers_to(word)) {
            if let Some(relation) = find_relation(snapshot, table) {
                return Some(relation_hover(relation));
            }
        }

        // A bare column. Which table it came from is most of the answer, so the
        // title carries it — and when more than one table in play has a column
        // by that name, so does an extra line, because that is the case where
        // Postgres would refuse the query outright.
        let owners: Vec<&db::Relation> = in_play
            .iter()
            .filter_map(|table| find_relation(snapshot, table))
            .filter(|relation| relation.columns.iter().any(|c| &*c.name == word))
            .collect();
        if let Some(relation) = owners.first() {
            let info = column_of(relation, word)?;
            if owners.len() < 2 {
                return Some(info);
            }
            let names: Vec<String> = owners
                .iter()
                .map(|relation| relation.reference.name.to_string())
                .collect();
            return Some(info.row("ambiguous", names.join(", ")));
        }

        // Nothing the statement named: fall back to the database at large, in
        // the order a query would resolve them.
        let table = InPlay {
            schema: None,
            name: word.to_string(),
            alias: None,
        };
        if let Some(relation) = find_relation(snapshot, &table) {
            return Some(relation_hover(relation));
        }
        if let Some(schema) = snapshot.schema(word) {
            return Some(schema_hover(schema));
        }
        let routine = snapshot
            .schemas
            .iter()
            .flat_map(|schema| schema.routines.iter())
            .find(|routine| &*routine.name == word);
        if let Some(routine) = routine {
            return Some(routine_hover(routine));
        }
        // Keywords, literals, aliases of expressions, anything misspelled: the
        // panel stays away rather than guessing.
        None
    }
}

fn column_hover(snapshot: &SchemaSnapshot, table: &InPlay, column: &str) -> Option<HoverInfo> {
    column_of(find_relation(snapshot, table)?, column)
}

/// Everything the catalog knows about one column, as lines.
fn column_of(relation: &db::Relation, name: &str) -> Option<HoverInfo> {
    let column = relation.columns.iter().find(|c| &*c.name == name)?;
    let nullable = match column.nullable {
        true => "",
        false => " not null",
    };
    let mut info = HoverInfo::new(
        format!("{}.{}", relation.reference.name, column.name),
        CompletionKind::Column,
    )
    .subtitle(format!("{}{nullable}", column.type_name));

    if relation
        .primary_key()
        .is_some_and(|pk| pk.columns.iter().any(|c| &**c == name))
    {
        info = info.row("key", "primary key");
    }
    // Where the value points. Written as it would be read aloud — the target
    // table and the column in it — rather than as the constraint's name, which
    // is a thing nobody has ever wanted to know while reading a query.
    let reference = relation.foreign_keys.iter().find_map(|key| {
        let at = key.columns.iter().position(|c| &**c == name)?;
        let target = key.target_columns.get(at)?;
        Some(format!("{}.{target}", key.target.name))
    });
    if let Some(reference) = reference {
        info = info.row("references", reference);
    }
    if let Some(identity) = column.identity {
        info = info.row("identity", identity.keyword().to_lowercase());
    }
    if let Some(default) = column.default.clone() {
        let label = match column.is_generated {
            true => "generated",
            false => "default",
        };
        info = info.row(label, default.to_string());
    }
    if let Some(comment) = column.comment.clone() {
        info = info.doc(comment.to_string());
    }
    Some(info)
}

fn relation_hover(relation: &db::Relation) -> HoverInfo {
    let kind = match relation.kind.is_view() {
        true => CompletionKind::View,
        false => CompletionKind::Table,
    };
    // Shape first: how wide, how tall, how heavy. It is what someone reading
    // an unfamiliar query wants from a table name, and it fits on one line.
    let mut shape = vec![match relation.columns.len() {
        1 => "1 column".to_string(),
        n => format!("{n} columns"),
    }];
    if relation.estimated_rows >= 0 {
        shape.push(format!("{} rows", row_estimate(relation.estimated_rows)));
    }
    if relation.size_bytes > 0 {
        shape.push(db::value::byte_size(relation.size_bytes as usize));
    }
    let mut info = HoverInfo::new(relation.reference.to_string(), kind).subtitle(shape.join(" · "));
    if let Some(pk) = relation.primary_key() {
        let columns: Vec<String> = pk.columns.iter().map(|c| c.to_string()).collect();
        info = info.row("key", columns.join(", "));
    }
    if let Some(comment) = relation.comment.clone() {
        info = info.doc(comment.to_string());
    }
    info
}

fn schema_hover(schema: &db::Schema) -> HoverInfo {
    let mut shape = vec![match schema.relations.len() {
        1 => "1 table".to_string(),
        n => format!("{n} tables"),
    }];
    if !schema.routines.is_empty() {
        shape.push(match schema.routines.len() {
            1 => "1 routine".to_string(),
            n => format!("{n} routines"),
        });
    }
    HoverInfo::new(schema.name.to_string(), CompletionKind::Schema)
        .subtitle(shape.join(" · "))
        .row("owner", schema.owner.to_string())
}

fn routine_hover(routine: &db::Routine) -> HoverInfo {
    let info = HoverInfo::new(
        format!("{}.{}", routine.schema, routine.name),
        CompletionKind::Function,
    )
    .subtitle(format!("{}{}", routine.name, routine.arguments));
    match routine.is_procedure {
        // A procedure returns nothing, and saying `returns void` about one
        // reads as a fact about this procedure rather than about procedures.
        true => info.row("kind", "procedure"),
        false => info.row("returns", routine.returns.to_string()),
    }
}

/// A row count nobody should read as exact, because it is the planner's guess.
fn row_estimate(rows: i64) -> String {
    match rows {
        0 => "0".into(),
        n if n < 1_000 => format!("~{n}"),
        n if n < 1_000_000 => format!("~{:.0}k", n as f64 / 1_000.),
        n if n < 1_000_000_000 => format!("~{:.1}M", n as f64 / 1_000_000.),
        n => format!("~{:.1}B", n as f64 / 1_000_000_000.),
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

/// The relation a name in a statement refers to.
///
/// Unqualified, the search path decides, exactly as it would for the query
/// itself — and then, failing that, any schema at all, because a name that
/// resolves nowhere is more usefully explained than not explained.
fn find_relation<'a>(snapshot: &'a SchemaSnapshot, table: &InPlay) -> Option<&'a db::Relation> {
    match &table.schema {
        Some(schema) => snapshot
            .schema(schema)
            .and_then(|schema| schema.relation(&table.name)),
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
    }
}

/// A table's columns, in the order the table has them — which is the order they
/// mean something in, and never alphabetical.
fn columns_of(snapshot: &SchemaSnapshot, table: &InPlay) -> Vec<Completion> {
    let Some(relation) = find_relation(snapshot, table) else {
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

/// The words SQL itself knows, in the case the person is writing in.
///
/// Typing `SEL` and being handed `select` breaks the line you were writing in
/// half. Which case is right is not a setting — it is whichever one is already
/// under the cursor, and only the words that belong to SQL get this: a table
/// called `Orders` is called that, and shouting it would be a different table.
fn keywords(prefix: &str) -> Vec<Completion> {
    let shouting = prefix.chars().any(|c| c.is_alphabetic())
        && prefix
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(char::is_uppercase);
    let word = |word: &&str, kind| {
        let label = match shouting {
            true => word.to_ascii_uppercase(),
            false => word.to_string(),
        };
        Completion::new(label, kind)
    };
    editor::sql::KEYWORDS
        .iter()
        .map(|w| word(w, CompletionKind::Keyword))
        .chain(
            editor::sql::FUNCTIONS
                .iter()
                .map(|w| word(w, CompletionKind::Function)),
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
    fn a_table_name_goes_after_from_and_a_column_name_does_not() {
        assert!(wants_a_table("select * from "));
        assert!(wants_a_table("select * from orders o join "));
        assert!(wants_a_table("insert into "));
        assert!(wants_a_table("update "));
        // Still in the from-list two names later: `from a, b, ` completes the
        // same way `from ` does.
        assert!(wants_a_table("select * from orders, users, "));

        assert!(!wants_a_table("select "));
        assert!(!wants_a_table("select * from orders where "));
        assert!(!wants_a_table("select id, "));
        assert!(!wants_a_table("update users set "));
        assert!(!wants_a_table("select * from orders order by "));
        // The column list of an insert, which is the one place a bracket
        // stands between `into` and a name that is not a table's.
        assert!(!wants_a_table("insert into users ("));
        // Nothing typed yet at all.
        assert!(!wants_a_table(""));
    }

    #[test]
    fn a_with_clause_names_tables_the_catalog_never_heard_of() {
        assert_eq!(
            ctes("with recent as (select 1), totals as (select 2) select * from "),
            ["recent", "totals"]
        );
        assert_eq!(ctes("with recursive tree as (select 1) select 1"), ["tree"]);
        // Only a `with` statement invents names; `as (` elsewhere does not.
        assert!(ctes("select count(*) from orders o").is_empty());
    }

    #[test]
    fn a_keyword_arrives_in_the_case_it_is_being_typed_in() {
        let labels = |prefix: &str| -> Vec<String> {
            keywords(prefix)
                .into_iter()
                .map(|c| c.label.to_string())
                .collect()
        };
        assert!(labels("SEL").contains(&"SELECT".to_string()));
        assert!(labels("sel").contains(&"select".to_string()));
        // Mixed case is someone typing a word, not shouting one.
        assert!(labels("Sel").contains(&"select".to_string()));
        assert!(labels("").contains(&"select".to_string()));
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
                create_statement: None,
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

    /// What the panel would say about the word at `offset`.
    fn about(catalog: &Catalog, text: &str, offset: usize) -> Option<HoverInfo> {
        let (range, qualifier) = editor::hover::word_around(text, offset)?;
        let word: String = text.chars().skip(range.start).take(range.len()).collect();
        catalog.hover(&HoverContext {
            text: text.into(),
            offset: range.start,
            word,
            qualifier,
        })
    }

    fn offers(catalog: &Catalog, text: &str) -> Vec<Completion> {
        offers_at(catalog, text, text.chars().count())
    }

    /// With the cursor somewhere other than the end, which is the only way to
    /// ask what `select | from orders` offers.
    fn offers_at(catalog: &Catalog, text: &str, offset: usize) -> Vec<Completion> {
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

    /// The whole point of knowing which clause the cursor is in: `from ` used
    /// to answer with three columns of `orders` before it got to `orders`.
    #[test]
    fn from_answers_with_tables_and_select_answers_with_columns() {
        let catalog = catalog();
        let after_from: Vec<_> = offers(&catalog, "select * from orders o join ")
            .iter()
            .map(|c| c.label.to_string())
            .collect();
        assert_eq!(after_from, ["orders", "users", "public"]);

        let after_select: Vec<_> = offers_at(&catalog, "select  from orders o", 7)
            .iter()
            .map(|c| c.label.to_string())
            .collect();
        assert_eq!(&after_select[..3], ["id", "user_id", "amount"]);
    }

    #[test]
    fn a_cte_is_offered_where_a_table_would_be() {
        let text = "with recent as (select 1) select * from ";
        let labels: Vec<_> = offers(&catalog(), text)
            .iter()
            .map(|c| c.label.to_string())
            .collect();
        // First: the statement wrote the name itself a moment ago.
        assert_eq!(labels.first().map(String::as_str), Some("recent"));
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

    #[test]
    fn a_column_says_which_table_it_came_from() {
        let text = "select amount from orders";
        let info = about(&catalog(), text, text.find("amount").unwrap() + 2).expect("a column");
        assert_eq!(info.title, "orders.amount");
        assert_eq!(info.kind, CompletionKind::Column);
        assert_eq!(info.subtitle.as_deref(), Some("numeric not null"));
    }

    #[test]
    fn an_alias_describes_the_table_it_stands_for() {
        let text = "select * from orders o where o.id = 1";
        let info = about(&catalog(), text, text.find(" o ").unwrap() + 1).expect("a table");
        assert_eq!(info.title, "public.orders");
        assert_eq!(info.kind, CompletionKind::Table);
        assert_eq!(info.subtitle.as_deref(), Some("3 columns · 0 rows"));
    }

    #[test]
    fn a_qualified_column_is_read_through_its_alias() {
        let text = "select * from orders o where o.id = 1";
        let info = about(&catalog(), text, text.find("o.id").unwrap() + 2).expect("a column");
        assert_eq!(info.title, "orders.id");
    }

    /// The case Postgres refuses outright, which is exactly when saying so is
    /// worth a line.
    #[test]
    fn a_column_two_tables_share_says_so() {
        let text = "select id from orders o join users u on u.id = o.user_id";
        let info = about(&catalog(), text, text.find("id").unwrap()).expect("a column");
        assert!(
            info.rows
                .iter()
                .any(|(label, value)| label == "ambiguous" && value == "orders, users"),
            "{:?}",
            info.rows
        );
    }

    #[test]
    fn a_keyword_has_nothing_to_say() {
        assert!(about(&catalog(), "select id from orders", 2).is_none());
    }
}
