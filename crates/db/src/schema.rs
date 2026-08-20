//! What the server says it has.
//!
//! Introspection produces one immutable [`SchemaSnapshot`] per connection,
//! handed around in an `Arc`. Everything downstream — the sidebar tree,
//! autocomplete, the SQL generator, the structure tab — reads from that one
//! value instead of issuing its own catalog queries, which is what keeps the
//! app from asking the server the same question four times per keystroke.
//!
//! A snapshot is a fact about a moment. DDL run through the app invalidates it
//! and a refresh replaces it wholesale; nothing mutates a snapshot in place.

use std::sync::Arc;

use crate::value::ValueKind;

/// The kinds of thing a schema contains, in the order the sidebar groups them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum RelationKind {
    Table,
    View,
    MaterializedView,
    /// `FOREIGN TABLE`.
    Foreign,
    /// A partitioned parent; its partitions are relations of their own.
    Partitioned,
}

impl RelationKind {
    pub fn is_view(self) -> bool {
        matches!(self, Self::View | Self::MaterializedView)
    }

    /// Can the grid offer to edit rows here?
    ///
    /// Views are excluded even though Postgres will happily update a simple
    /// one: whether a given view is updatable depends on rules the app cannot
    /// see, and offering an edit that fails at commit is worse than not
    /// offering it.
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Table | Self::Partitioned)
    }
}

/// Which of the two identity flavours a column was declared with.
///
/// Not a boolean, because the difference is what decides whether an `INSERT`
/// may name the column at all: `ALWAYS` refuses a supplied value without
/// `OVERRIDING SYSTEM VALUE`, `BY DEFAULT` accepts one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdentityKind {
    Always,
    ByDefault,
}

impl IdentityKind {
    pub fn from_pg(c: u8) -> Option<Self> {
        match c {
            b'a' => Some(Self::Always),
            b'd' => Some(Self::ByDefault),
            _ => None,
        }
    }

    /// As it is written in DDL.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Always => "ALWAYS",
            Self::ByDefault => "BY DEFAULT",
        }
    }
}

/// One column of a relation.
#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub name: Arc<str>,
    /// Position, 1-based, as the server orders them.
    pub position: i16,
    /// The type as the server names it: `character varying(64)`, `int4`,
    /// `timestamp with time zone`. Kept verbatim because it is what the
    /// structure tab shows and what generated DDL has to reproduce.
    pub type_name: Arc<str>,
    pub kind: ValueKind,
    pub nullable: bool,
    /// The column default, or — for a generated column — the expression it is
    /// computed from, which is where Postgres keeps it.
    pub default: Option<Arc<str>>,
    /// `GENERATED … AS IDENTITY`. A `serial` is not one of these: it is an
    /// ordinary column whose default happens to call `nextval`.
    pub identity: Option<IdentityKind>,
    /// `GENERATED ALWAYS AS (…) STORED`.
    pub is_generated: bool,
    pub comment: Option<Arc<str>>,
}

impl ColumnDef {
    pub fn is_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// Can a row be written without naming this column? True for anything the
    /// server will fill in itself, which is what the grid needs to know before
    /// it insists on a value.
    pub fn has_server_value(&self) -> bool {
        self.default.is_some() || self.is_identity() || self.is_generated
    }
}

/// An index, including the one backing a primary key.
#[derive(Clone, Debug)]
pub struct IndexDef {
    pub name: Arc<str>,
    pub columns: Vec<Arc<str>>,
    pub is_unique: bool,
    pub is_primary: bool,
    /// `btree`, `gin`, `gist`, …
    pub method: Arc<str>,
    /// The `WHERE` of a partial index.
    pub predicate: Option<Arc<str>>,
}

/// What to do to the child when the parent goes away.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl RefAction {
    /// The single-letter code `pg_constraint` stores.
    pub fn from_pg(c: u8) -> Self {
        match c {
            b'r' => Self::Restrict,
            b'c' => Self::Cascade,
            b'n' => Self::SetNull,
            b'd' => Self::SetDefault,
            _ => Self::NoAction,
        }
    }
}

/// A foreign key, from the referencing side.
#[derive(Clone, Debug)]
pub struct ForeignKey {
    pub name: Arc<str>,
    pub columns: Vec<Arc<str>>,
    pub target: RelationRef,
    pub target_columns: Vec<Arc<str>>,
    pub on_delete: RefAction,
    pub on_update: RefAction,
}

/// A `CHECK` constraint, as the server prints it back.
///
/// The expression is `pg_get_constraintdef`'s own text rather than anything
/// this app reconstructs: Postgres parses `price > 0` and stores a tree, and
/// what it prints — `CHECK ((price > (0)::numeric))` — is the only rendering
/// guaranteed to mean the same thing when it is sent back.
#[derive(Clone, Debug)]
pub struct CheckConstraint {
    pub name: Arc<str>,
    /// The whole `CHECK (…)` clause, parentheses included.
    pub definition: Arc<str>,
}

/// A trigger, minus the function body it calls.
#[derive(Clone, Debug)]
pub struct TriggerDef {
    pub name: Arc<str>,
    /// The whole `CREATE TRIGGER …` statement from `pg_get_triggerdef`, which
    /// is what the DDL tab reproduces verbatim.
    pub definition: Arc<str>,
    /// `BEFORE INSERT OR UPDATE`, extracted for the structure table so it can
    /// have a column that is not two hundred characters wide.
    pub timing: Arc<str>,
    /// The function the trigger calls, schema-qualified.
    pub function: Arc<str>,
    /// A disabled trigger still exists and still shows, greyed: finding out
    /// why nothing happened is exactly what this tab is for.
    pub enabled: bool,
}

/// A schema-qualified name. Comparing these is how the app decides two
/// references mean the same relation, so it is a plain value with no interning
/// games.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct RelationRef {
    pub schema: Arc<str>,
    pub name: Arc<str>,
}

impl RelationRef {
    pub fn new(schema: impl Into<Arc<str>>, name: impl Into<Arc<str>>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    /// The form to put in SQL, quoting only what needs it.
    pub fn qualified(&self) -> String {
        format!("{}.{}", quote_ident(&self.schema), quote_ident(&self.name))
    }
}

impl std::fmt::Display for RelationRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

/// A table, view, or anything else with columns.
#[derive(Clone, Debug)]
pub struct Relation {
    pub reference: RelationRef,
    pub kind: RelationKind,
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub foreign_keys: Vec<ForeignKey>,
    pub checks: Vec<CheckConstraint>,
    pub triggers: Vec<TriggerDef>,
    /// For a view or a materialised view, the `SELECT` behind it as the server
    /// prints it. `None` for anything with rows of its own.
    pub definition: Option<Arc<str>>,
    /// The whole `CREATE`, verbatim, for an engine that will print one.
    ///
    /// Postgres will not — there is no `pg_get_tabledef`, which is why the DDL
    /// tab reconstructs a table from the catalog. ClickHouse keeps the
    /// statement it was created with in `system.tables`, and that text beats
    /// anything this app could assemble: it carries the engine, the sorting
    /// key, the TTLs and the settings, in the dialect that made them.
    pub create_statement: Option<Arc<str>>,
    /// The planner's row estimate. Exact counts cost a sequential scan, so the
    /// sidebar shows this and the grid shows the real count once it has one.
    ///
    /// Negative means *unknown*: Postgres 14 and later store `reltuples = -1`
    /// for a relation that has never been vacuumed or analysed, which is the
    /// normal state of a freshly restored database. Anything that prints this
    /// has to say so rather than printing the number.
    pub estimated_rows: i64,
    /// Total size including indexes and TOAST, in bytes.
    pub size_bytes: i64,
    pub comment: Option<Arc<str>>,
    /// Loaded lazily: a database with ten thousand tables should not pay for
    /// every column of every one of them at connect time.
    pub detail_loaded: bool,
}

impl Relation {
    pub fn primary_key(&self) -> Option<&IndexDef> {
        self.indexes.iter().find(|i| i.is_primary)
    }

    /// The columns that identify a row for an `UPDATE`/`DELETE`: the primary
    /// key, or failing that the narrowest unique index over non-null columns.
    /// Without one the grid stays read-only rather than guessing.
    pub fn row_identity(&self) -> Option<&IndexDef> {
        if let Some(pk) = self.primary_key() {
            return Some(pk);
        }
        self.indexes
            .iter()
            .filter(|i| i.is_unique && i.predicate.is_none())
            .filter(|i| {
                i.columns.iter().all(|c| {
                    self.columns
                        .iter()
                        .find(|col| col.name == *c)
                        .is_some_and(|col| !col.nullable)
                })
            })
            .min_by_key(|i| i.columns.len())
    }

    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| &*c.name == name)
    }
}

/// A stored routine, for the sidebar's functions group.
#[derive(Clone, Debug)]
pub struct Routine {
    pub schema: Arc<str>,
    pub name: Arc<str>,
    /// Rendered argument list, e.g. `(uuid, timestamptz)`, used to
    /// disambiguate overloads in the tree.
    pub arguments: Arc<str>,
    pub returns: Arc<str>,
    pub is_procedure: bool,
}

#[derive(Clone, Debug)]
pub struct Schema {
    pub name: Arc<str>,
    pub owner: Arc<str>,
    /// `pg_catalog`, `information_schema` and the like: present so they can be
    /// shown on request, hidden by default.
    pub is_system: bool,
    pub relations: Vec<Relation>,
    pub routines: Vec<Routine>,
}

impl Schema {
    pub fn relation(&self, name: &str) -> Option<&Relation> {
        self.relations.iter().find(|r| &*r.reference.name == name)
    }
}

/// Everything introspection found, at one moment.
#[derive(Clone, Debug, Default)]
pub struct SchemaSnapshot {
    pub database: Arc<str>,
    /// Every database on this server the role is allowed to connect to, in
    /// name order, including the one this snapshot is of. A server is not one
    /// database — the sidebar lists them all, and switching means connecting
    /// again, because a Postgres session can only ever see the one it opened.
    pub databases: Vec<Arc<str>>,
    /// The server's `server_version` string, for the status bar.
    pub server_version: Arc<str>,
    /// `search_path` as the session resolves it, first entry first, *including*
    /// the implicit `pg_catalog`. The SQL generator uses it to decide when a
    /// name can go unqualified, and for that question the implicit entries
    /// count.
    pub search_path: Vec<Arc<str>>,
    /// Where an unqualified `create table` would land: the first schema on the
    /// path that the session could actually write to. Distinct from
    /// `search_path[0]`, which is almost always `pg_catalog` and is never the
    /// answer to "which schema am I in".
    pub current_schema: Arc<str>,
    pub schemas: Vec<Schema>,
}

impl SchemaSnapshot {
    pub fn schema(&self, name: &str) -> Option<&Schema> {
        self.schemas.iter().find(|s| &*s.name == name)
    }

    pub fn relation(&self, reference: &RelationRef) -> Option<&Relation> {
        self.schema(&reference.schema)?.relation(&reference.name)
    }

    /// Every relation, system schemas included, in schema then name order.
    pub fn relations(&self) -> impl Iterator<Item = &Relation> {
        self.schemas.iter().flat_map(|s| s.relations.iter())
    }

    /// Is `name` reachable unqualified? Governs whether generated SQL writes
    /// `users` or `public.users`.
    pub fn in_search_path(&self, schema: &str) -> bool {
        self.search_path.iter().any(|s| &**s == schema)
    }
}

/// Quote an identifier only when Postgres would need it.
///
/// Quoting everything would be simpler and is what most generators do, but it
/// makes the SQL preview — which users read before committing a change —
/// noticeably harder to read, and the preview is the whole safety mechanism.
pub fn quote_ident(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !is_reserved(name);
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Reserved words that cannot be a bare identifier. Not the full list — the
/// full list is 400 entries and the cost of a false negative is a quoted name
/// that did not need quoting.
fn is_reserved(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "all",
        "analyse",
        "analyze",
        "and",
        "any",
        "array",
        "as",
        "asc",
        "authorization",
        "binary",
        "both",
        "case",
        "cast",
        "check",
        "collate",
        "column",
        "constraint",
        "create",
        "cross",
        "current_date",
        "current_role",
        "current_time",
        "current_timestamp",
        "current_user",
        "default",
        "deferrable",
        "desc",
        "distinct",
        "do",
        "else",
        "end",
        "except",
        "false",
        "for",
        "foreign",
        "from",
        "full",
        "grant",
        "group",
        "having",
        "in",
        "initially",
        "inner",
        "intersect",
        "into",
        "is",
        "join",
        "leading",
        "left",
        "like",
        "limit",
        "localtime",
        "localtimestamp",
        "natural",
        "not",
        "null",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "outer",
        "overlaps",
        "placing",
        "primary",
        "references",
        "returning",
        "right",
        "select",
        "session_user",
        "similar",
        "some",
        "symmetric",
        "table",
        "then",
        "to",
        "trailing",
        "true",
        "union",
        "unique",
        "user",
        "using",
        "variadic",
        "verbose",
        "when",
        "where",
        "window",
        "with",
    ];
    RESERVED.binary_search(&name).is_ok()
}

/// A string literal, for the SQL preview and for values that cannot be bound
/// as parameters (identifiers in DDL, mostly).
pub fn quote_literal(value: &str) -> String {
    if value.contains('\\') {
        // E'' strings are the only way to write a backslash unambiguously
        // regardless of `standard_conforming_strings`.
        format!("E'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            position: 1,
            type_name: "int8".into(),
            kind: ValueKind::Int,
            nullable,
            default: None,
            identity: None,
            is_generated: false,
            comment: None,
        }
    }

    fn index(name: &str, columns: &[&str], unique: bool, primary: bool) -> IndexDef {
        IndexDef {
            name: name.into(),
            columns: columns.iter().map(|c| (*c).into()).collect(),
            is_unique: unique,
            is_primary: primary,
            method: "btree".into(),
            predicate: None,
        }
    }

    fn relation(columns: Vec<ColumnDef>, indexes: Vec<IndexDef>) -> Relation {
        Relation {
            reference: RelationRef::new("public", "users"),
            kind: RelationKind::Table,
            columns,
            indexes,
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

    #[test]
    fn the_primary_key_wins_over_a_narrower_unique_index() {
        let r = relation(
            vec![column("id", false), column("email", false)],
            vec![
                index("users_email_key", &["email"], true, false),
                index("users_pkey", &["id"], true, true),
            ],
        );
        assert_eq!(&*r.row_identity().unwrap().name, "users_pkey");
    }

    #[test]
    fn a_nullable_unique_index_is_not_an_identity() {
        let r = relation(
            vec![column("email", true)],
            vec![index("users_email_key", &["email"], true, false)],
        );
        assert!(r.row_identity().is_none());
    }

    #[test]
    fn identifiers_are_quoted_only_when_they_have_to_be() {
        assert_eq!(quote_ident("users"), "users");
        assert_eq!(quote_ident("created_at"), "created_at");
        assert_eq!(quote_ident("Users"), "\"Users\"");
        assert_eq!(quote_ident("order"), "\"order\"");
        assert_eq!(quote_ident("2fa"), "\"2fa\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn literals_escape_quotes_and_backslashes() {
        assert_eq!(quote_literal("o'neill"), "'o''neill'");
        assert_eq!(quote_literal("a\\b"), "E'a\\\\b'");
    }

    #[test]
    fn reserved_word_list_is_sorted_for_binary_search() {
        // The lookup is a binary search, so an out-of-order entry would be a
        // silent miss rather than a failure.
        let mut sorted = RESERVED_FOR_TEST.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, RESERVED_FOR_TEST.to_vec());
    }

    // Mirrors the list in `is_reserved`; kept here so the sortedness test can
    // see it without making the constant public.
    const RESERVED_FOR_TEST: &[&str] = &[
        "all",
        "analyse",
        "analyze",
        "and",
        "any",
        "array",
        "as",
        "asc",
        "authorization",
        "binary",
        "both",
        "case",
        "cast",
        "check",
        "collate",
        "column",
        "constraint",
        "create",
        "cross",
        "current_date",
        "current_role",
        "current_time",
        "current_timestamp",
        "current_user",
        "default",
        "deferrable",
        "desc",
        "distinct",
        "do",
        "else",
        "end",
        "except",
        "false",
        "for",
        "foreign",
        "from",
        "full",
        "grant",
        "group",
        "having",
        "in",
        "initially",
        "inner",
        "intersect",
        "into",
        "is",
        "join",
        "leading",
        "left",
        "like",
        "limit",
        "localtime",
        "localtimestamp",
        "natural",
        "not",
        "null",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "outer",
        "overlaps",
        "placing",
        "primary",
        "references",
        "returning",
        "right",
        "select",
        "session_user",
        "similar",
        "some",
        "symmetric",
        "table",
        "then",
        "to",
        "trailing",
        "true",
        "union",
        "unique",
        "user",
        "using",
        "variadic",
        "verbose",
        "when",
        "where",
        "window",
        "with",
    ];
}
