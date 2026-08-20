//! The sidebar tree, flattened.
//!
//! One `Vec` in visit order rather than a graph of parents and children. The
//! sidebar is a scrolling list of rows, and a flat list is what a list wants:
//! filtering, keyboard navigation and virtualisation are all linear passes, and
//! collapsing a branch is "skip while depth is greater", which needs no
//! bookkeeping at all.

use std::sync::Arc;

use db::{KeyInfo, KeyType, Keyspace, RelationKind, RelationRef, Role, RoleSet, SchemaSnapshot};
use gpui::SharedString;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Connection,
    Database,
    SchemaGroup,
    Schema,
    TableGroup,
    Table,
    View,
    MaterializedView,
    FunctionGroup,
    Function,
    /// The heading over the roles. A sibling of the databases rather than a
    /// child of one, because a role is a property of the server: the same
    /// `analytics` grants apply in every database on it.
    RoleGroup,
    /// A role that can log in.
    Role,
    /// A role that cannot, which everybody calls a group even though Postgres
    /// stopped having a separate concept for it twenty years ago.
    RoleGroupMember,
    /// A logical database of a key-value server — `DB 0`.
    KeyDatabase,
    /// A `:`-separated prefix that several keys share. Not a thing on the
    /// server: the server has a flat namespace and this is the convention
    /// everybody names keys by, drawn as the folder it is pretending to be.
    KeyFolder,
    Key,
}

impl NodeKind {
    /// Whether double-clicking this row should open a data tab.
    pub fn is_relation(self) -> bool {
        matches!(self, Self::Table | Self::View | Self::MaterializedView)
    }
}

/// What a row opens.
///
/// Two engines, two kinds of thing to open, one tree. The alternative was a
/// second tree type with a second sidebar drawing it, which would have been
/// two copies of filtering, collapsing and keyboard navigation to keep in
/// step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Relation(RelationRef),
    /// A key and what it holds. The type comes along because the listing
    /// already knew it, and asking the server again on the way to opening it
    /// would be a round trip to learn something nobody had forgotten.
    Key(Arc<[u8]>, KeyType),
}

impl Target {
    pub fn relation(&self) -> Option<&RelationRef> {
        match self {
            Self::Relation(reference) => Some(reference),
            Self::Key(..) => None,
        }
    }

    pub fn key(&self) -> Option<(&Arc<[u8]>, &KeyType)> {
        match self {
            Self::Key(key, kind) => Some((key, kind)),
            Self::Relation(_) => None,
        }
    }
}

#[derive(Clone)]
pub struct TreeNode {
    pub id: usize,
    pub depth: usize,
    pub kind: NodeKind,
    pub name: SharedString,
    /// The grey text on the right of the row: a count, a row estimate, a size.
    pub meta: Option<SharedString>,
    pub expandable: bool,
    /// What to open when the row is activated. `None` for the grouping rows,
    /// which exist only to be collapsed.
    pub target: Option<Target>,
}

impl TreeNode {
    fn new(depth: usize, kind: NodeKind, name: impl Into<SharedString>) -> Self {
        Self {
            // Assigned in one pass at the end: a node's identity is its
            // position, and threading a counter through the builder would only
            // make it easier to get wrong.
            id: 0,
            depth,
            kind,
            name: name.into(),
            meta: None,
            expandable: false,
            target: None,
        }
    }

    fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    fn when_some(mut self, meta: Option<String>) -> Self {
        if let Some(meta) = meta {
            self.meta = Some(meta.into());
        }
        self
    }

    fn expandable(mut self) -> Self {
        self.expandable = true;
        self
    }

    fn target(mut self, target: &RelationRef) -> Self {
        self.target = Some(Target::Relation(target.clone()));
        self
    }

    fn key(mut self, key: &Arc<[u8]>, kind: &KeyType) -> Self {
        self.target = Some(Target::Key(key.clone(), kind.clone()));
        self
    }
}

/// Build the tree for one connected server.
///
/// Grouping rows — "tables", "views", "functions" — appear only when they have
/// something in them. A schema with no views should not make the reader check.
pub fn from_snapshot(
    connection: &str,
    snapshot: &SchemaSnapshot,
    roles: Option<&RoleSet>,
) -> Vec<TreeNode> {
    let mut nodes = Vec::new();

    nodes.push(
        TreeNode::new(0, NodeKind::Connection, connection.to_string())
            .meta(short_version(&snapshot.server_version))
            .expandable(),
    );
    // Every database on the server, not only the one that is open. A Postgres
    // session sees exactly one database and nothing else on the server, so the
    // others are names with nothing under them until one is clicked and the
    // app connects again — which is why only the open one is expandable.
    for database in databases(snapshot) {
        let current = database == snapshot.database;
        let mut node = TreeNode::new(1, NodeKind::Database, database.to_string());
        if current {
            node = node
                .meta(plural(snapshot.schemas.len(), "schema", "schemas"))
                .expandable();
        }
        nodes.push(node);
        if !current {
            continue;
        }

        for schema in &snapshot.schemas {
            let tables: Vec<_> = schema
                .relations
                .iter()
                .filter(|r| {
                    matches!(
                        r.kind,
                        RelationKind::Table | RelationKind::Partitioned | RelationKind::Foreign
                    )
                })
                .collect();
            let views: Vec<_> = schema
                .relations
                .iter()
                .filter(|r| r.kind.is_view())
                .collect();

            nodes.push(
                TreeNode::new(2, NodeKind::Schema, schema.name.to_string())
                    .meta(plural(schema.relations.len(), "table", "tables"))
                    .expandable(),
            );

            if !tables.is_empty() {
                nodes.push(
                    TreeNode::new(3, NodeKind::TableGroup, "tables")
                        .meta(tables.len().to_string())
                        .expandable(),
                );
                for relation in tables {
                    nodes.push(
                        TreeNode::new(4, NodeKind::Table, relation.reference.name.to_string())
                            .meta(row_estimate(relation.estimated_rows))
                            .target(&relation.reference),
                    );
                }
            }

            if !views.is_empty() {
                nodes.push(
                    TreeNode::new(3, NodeKind::TableGroup, "views")
                        .meta(views.len().to_string())
                        .expandable(),
                );
                for relation in views {
                    let kind = match relation.kind {
                        RelationKind::MaterializedView => NodeKind::MaterializedView,
                        _ => NodeKind::View,
                    };
                    nodes.push(
                        TreeNode::new(4, kind, relation.reference.name.to_string())
                            .target(&relation.reference),
                    );
                }
            }

            if !schema.routines.is_empty() {
                nodes.push(
                    TreeNode::new(3, NodeKind::FunctionGroup, "functions")
                        .meta(schema.routines.len().to_string())
                        .expandable(),
                );
                for routine in &schema.routines {
                    nodes.push(TreeNode::new(
                        4,
                        NodeKind::Function,
                        routine.name.to_string(),
                    ));
                }
            }
        }
    }

    // After every database, because it is about all of them. Only when there
    // is more than one role: a server with nothing but `postgres` on it has no
    // access story to tell, and a folder that opens onto the name you are
    // already logged in as is a row that costs more than it says.
    if let Some(roles) = roles.filter(|roles| roles.roles.len() > 1) {
        nodes.push(
            TreeNode::new(1, NodeKind::RoleGroup, "roles")
                .meta(roles.roles.len().to_string())
                .expandable(),
        );
        for role in &roles.roles {
            let kind = match role.can_login {
                true => NodeKind::Role,
                false => NodeKind::RoleGroupMember,
            };
            let mine = &*role.name == &*roles.current;
            nodes.push(
                TreeNode::new(2, kind, role.name.to_string()).when_some(role_meta(role, mine)),
            );
        }
    }

    for (id, node) in nodes.iter_mut().enumerate() {
        node.id = id;
    }
    nodes
}

/// A role's line in the sidebar's narrow right-hand column.
///
/// Not [`Role::summary`], which is the whole story and belongs in the
/// Privileges tab: `postgres` on a fresh server has five attributes, and a
/// meta column that long squeezes out the name it is describing. Two chips,
/// because [`Role::attributes`] already orders them by how much they matter,
/// and "you" outranks any of them — which one you are is the single most
/// useful fact in the list.
fn role_meta(role: &Role, mine: bool) -> Option<String> {
    const SHOWN: usize = 2;
    let mut parts: Vec<&str> = Vec::new();
    if mine {
        parts.push("you");
    }
    parts.extend(role.attributes().into_iter().take(SHOWN - parts.len()));
    match parts.is_empty() {
        true => match role.member_of.len() {
            0 => None,
            n => Some(format!("member of {n}")),
        },
        false => Some(parts.join(" · ")),
    }
}

/// Build the tree for one connected key-value server.
///
/// The shape is a lie that everybody tells: Redis has one flat namespace and
/// no folders at all, but every keyspace in the world is named `user:1:cart`
/// and read as though the colons were slashes. So the colons are treated as
/// slashes here, and the folders are labelled with what is actually under them
/// rather than with a promise — see [`scanned`].
///
/// `complete` is whether the walk that produced `keys` reached the end. It
/// decides the wording and nothing else: a browser that has seen eight
/// thousand of ten million keys must not draw a tree that looks like the
/// keyspace.
pub fn from_keyspace(
    connection: &str,
    version: &str,
    keyspace: &Keyspace,
    keys: &[KeyInfo],
    complete: bool,
) -> Vec<TreeNode> {
    let mut nodes = vec![
        TreeNode::new(0, NodeKind::Connection, connection.to_string())
            .meta(short_version(version))
            .expandable(),
    ];

    for database in &keyspace.databases {
        let current = database.index == keyspace.current;
        let mut node = TreeNode::new(1, NodeKind::KeyDatabase, format!("DB {}", database.index))
            .meta(plural(database.keys as usize, "key", "keys"));
        // Only the open database has keys to show. Switching to another one
        // is a `SELECT` on the connection, which is what clicking it does.
        if current {
            node = node.expandable();
        }
        nodes.push(node);
        if current {
            push_keys(&mut nodes, keys, 2);
        }
    }

    // A server that would not answer `INFO keyspace`, or one whose databases
    // are all empty: the connection is open and there is nothing under it, and
    // a tree with no database in it at all would read as a failure to connect.
    if keyspace.databases.is_empty() {
        nodes.push(
            TreeNode::new(1, NodeKind::KeyDatabase, format!("DB {}", keyspace.current))
                .meta(scanned(keys.len(), complete))
                .expandable(),
        );
        push_keys(&mut nodes, keys, 2);
    }

    for (id, node) in nodes.iter_mut().enumerate() {
        node.id = id;
    }
    nodes
}

/// The keys, folded into folders on their colons.
///
/// One pass over a sorted list rather than a trie built and then walked: the
/// keys are already in the order the tree wants them in, so the only state
/// needed is what the previous key's path was, and the folders to open are the
/// segments where the two paths stop agreeing.
fn push_keys(nodes: &mut Vec<TreeNode>, keys: &[KeyInfo], depth: usize) {
    let mut rows: Vec<(Vec<&str>, &KeyInfo)> = keys
        .iter()
        .map(|info| (segments(&info.key), info))
        .collect();
    rows.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.key.cmp(&y.key)));

    // How many keys sit under each folder path, so a folder can say so before
    // it is opened. Counted first because the row is written before its
    // children are.
    let mut counts: std::collections::HashMap<&[&str], usize> = std::collections::HashMap::new();
    for (path, _) in &rows {
        for cut in 1..path.len() {
            *counts.entry(&path[..cut]).or_default() += 1;
        }
    }

    let mut open: Vec<&str> = Vec::new();
    for (path, info) in &rows {
        let folders = &path[..path.len() - 1];
        let shared = open.iter().zip(folders).take_while(|(a, b)| a == b).count();
        open.truncate(shared);
        for cut in shared..folders.len() {
            open.push(folders[cut]);
            nodes.push(
                TreeNode::new(depth + cut, NodeKind::KeyFolder, folders[cut].to_string())
                    .meta(plural(
                        counts.get(&path[..cut + 1]).copied().unwrap_or(0),
                        "key",
                        "keys",
                    ))
                    .expandable(),
            );
        }
        // The last segment, not the whole key: the folders above it already
        // spell out the prefix, and a column of `large:pokemon:…` elided in a
        // narrow panel tells the reader nothing the folder did not.
        let name = path.last().copied().unwrap_or_default();
        nodes.push(
            TreeNode::new(depth + folders.len(), NodeKind::Key, name.to_string())
                .key(&info.key, &info.kind)
                .when_some(ttl_meta(info.ttl)),
        );
    }
}

/// A key split on its colons, with empty segments kept.
///
/// `user::1` has an empty middle segment and it is a real one — the key is not
/// `user:1` and must not be drawn as though it were.
fn segments(key: &[u8]) -> Vec<&str> {
    // Not `from_utf8_lossy`: the borrow has to outlive this call, and a key
    // that is not UTF-8 is drawn from its bytes further down rather than
    // silently regrouped on colons that may be inside a multi-byte character.
    match std::str::from_utf8(key) {
        Ok(text) => text.split(':').collect(),
        Err(_) => Vec::new(),
    }
}

/// `8,409 keys` or `8,409 scanned` — which one being the whole point.
///
/// A scan is a sample and not an inventory, so a count that came out of an
/// unfinished walk is labelled as how far it got rather than as how many there
/// are.
fn scanned(count: usize, complete: bool) -> String {
    match complete {
        true => plural(count, "key", "keys"),
        false => format!("{count} scanned"),
    }
}

/// The grey text on a key's row: how long it has left, and nothing when it has
/// forever. Most keys have no expiry, and a column of `Forever` would be a
/// column of the word "Forever".
fn ttl_meta(ttl: Option<i64>) -> Option<String> {
    ttl.filter(|ttl| *ttl >= 0)
        .map(|ttl| db::format_ttl(Some(ttl)))
}

/// Ids of the rows that should start closed.
///
/// Everything below the schemas, because a server with forty schemas would
/// otherwise open to a tree thousands of rows long. The connection and the
/// database stay open: a tree whose root is shut is a tree that looks broken.
///
/// The exception is a database with a single schema, which is what most of them
/// are. There is no choice to make there, and making somebody click twice
/// through `public` → `tables` to reach the only tables on the server is two
/// clicks charged for no information. Its folders open with it; what is inside
/// them does not, because that is where the row counts get large.
pub fn initially_collapsed(nodes: &[TreeNode]) -> Vec<usize> {
    let schemas = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Schema)
        .count();
    // …and not if that schema is enormous. A thousand tables listed on arrival
    // is a scroll bar, not a starting point.
    let objects = nodes.iter().filter(|node| node.depth >= 4).count();
    let floor = match schemas == 1 && objects <= 200 {
        true => 4,
        false => 2,
    };
    nodes
        .iter()
        .filter(|node| node.expandable && node.depth >= floor)
        .map(|node| node.id)
        .collect()
}

/// The database names to list, current one included.
///
/// A snapshot from a server that would not answer `pg_database` — or from a
/// test — has an empty list, and a sidebar with no database in it at all would
/// look like a failure. The open one is always there.
fn databases(snapshot: &SchemaSnapshot) -> Vec<std::sync::Arc<str>> {
    match snapshot.databases.is_empty() {
        true => vec![snapshot.database.clone()],
        false => snapshot.databases.clone(),
    }
}

/// `16.4 (Homebrew)` → `16.4`. The build string is interesting exactly once,
/// and the sidebar is not where it belongs.
fn short_version(version: &str) -> String {
    version
        .split_whitespace()
        .next()
        .unwrap_or(version)
        .to_string()
}

/// `reltuples`, phrased as an estimate.
///
/// `-1` means the table has never been analysed, which is different from empty
/// and worth not lying about.
fn row_estimate(rows: i64) -> String {
    match rows {
        ..=-1 => "—".into(),
        0 => "0".into(),
        n if n < 1_000 => format!("~{n}"),
        n if n < 1_000_000 => format!("~{:.0}k", n as f64 / 1_000.),
        n if n < 1_000_000_000 => format!("~{:.1}M", n as f64 / 1_000_000.),
        n => format!("~{:.1}B", n as f64 / 1_000_000_000.),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::{Relation, RelationKind, Schema};
    use std::sync::Arc;

    fn role(name: &str, superuser: bool) -> Role {
        Role {
            name: Arc::from(name),
            superuser,
            can_login: true,
            create_db: superuser,
            create_role: superuser,
            inherit: true,
            replication: superuser,
            bypass_rls: superuser,
            connection_limit: -1,
            valid_until: None,
            member_of: Vec::new(),
            comment: None,
        }
    }

    #[test]
    fn the_role_you_are_connected_as_says_so_before_anything_else() {
        assert_eq!(
            role_meta(&role("postgres", true), true).as_deref(),
            Some("you · Superuser")
        );
    }

    #[test]
    fn a_long_list_of_attributes_is_cut_so_the_name_still_fits() {
        // Five attributes in a column this narrow would elide the name they
        // are describing, which is the one thing the row is for.
        assert_eq!(
            role_meta(&role("deploy", true), false).as_deref(),
            Some("Superuser · Create DB")
        );
    }

    #[test]
    fn a_role_with_nothing_to_say_falls_back_to_who_it_inherits_from() {
        let mut member = role("reporting", false);
        member.member_of = vec![Arc::from("analytics")];
        assert_eq!(role_meta(&member, false).as_deref(), Some("member of 1"));
        assert_eq!(role_meta(&role("app", false), false), None);
    }

    fn relation(schema: &str, name: &str, kind: RelationKind, rows: i64) -> Relation {
        Relation {
            reference: RelationRef::new(Arc::from(schema), Arc::from(name)),
            kind,
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers: Vec::new(),
            definition: None,
            estimated_rows: rows,
            size_bytes: 0,
            comment: None,
            detail_loaded: true,
        }
    }

    fn snapshot() -> SchemaSnapshot {
        SchemaSnapshot {
            database: Arc::from("app"),
            databases: vec![Arc::from("app"), Arc::from("app_test")],
            server_version: Arc::from("16.4 (Homebrew)"),
            search_path: vec![Arc::from("pg_catalog"), Arc::from("public")],
            current_schema: Arc::from("public"),
            schemas: vec![
                Schema {
                    name: Arc::from("public"),
                    owner: Arc::from("postgres"),
                    is_system: false,
                    relations: vec![
                        relation("public", "users", RelationKind::Table, 12_400),
                        relation("public", "active_users", RelationKind::View, 0),
                    ],
                    routines: Vec::new(),
                },
                Schema {
                    name: Arc::from("empty"),
                    owner: Arc::from("postgres"),
                    is_system: false,
                    relations: Vec::new(),
                    routines: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn every_database_on_the_server_is_a_row_and_only_the_open_one_opens() {
        let nodes = from_snapshot("local", &snapshot(), None);
        let databases: Vec<_> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Database)
            .collect();
        assert_eq!(
            databases
                .iter()
                .map(|n| n.name.as_ref())
                .collect::<Vec<_>>(),
            ["app", "app_test"]
        );
        // The one that is open has schemas under it and a disclosure to hide
        // them with; the other is a name and a click.
        assert!(databases[0].expandable);
        assert!(!databases[1].expandable);
        // ...and nothing of the closed one's is in the tree, because a session
        // on `app` has never seen inside `app_test`. It sorts last, so the
        // last row in the tree is the row itself and not a child of it.
        assert_eq!(nodes.last().map(|node| node.kind), Some(NodeKind::Database));
    }

    #[test]
    fn the_tree_is_flat_and_ids_are_positions() {
        let nodes = from_snapshot("local", &snapshot(), None);
        assert!(nodes.iter().enumerate().all(|(i, node)| node.id == i));
        assert_eq!(nodes[0].kind, NodeKind::Connection);
        assert_eq!(nodes[0].meta.as_deref(), Some("16.4"));
        assert_eq!(nodes[1].kind, NodeKind::Database);
    }

    #[test]
    fn empty_groups_do_not_appear() {
        let nodes = from_snapshot("local", &snapshot(), None);
        let names: Vec<_> = nodes.iter().map(|n| n.name.as_ref()).collect();
        assert!(names.contains(&"tables"));
        assert!(names.contains(&"views"));
        assert!(!names.contains(&"functions"));
        // The schema with nothing in it is still listed — it exists.
        assert!(names.contains(&"empty"));
    }

    #[test]
    fn relations_carry_the_reference_needed_to_open_them() {
        let nodes = from_snapshot("local", &snapshot(), None);
        let users = nodes.iter().find(|n| n.name == *"users").unwrap();
        let reference = users.target.as_ref().unwrap().relation().unwrap();
        assert_eq!(reference.qualified(), "public.users");
        // Grouping rows are not openable.
        let group = nodes.iter().find(|n| n.name == *"tables").unwrap();
        assert!(group.target.is_none());
    }

    #[test]
    fn a_never_analysed_table_says_so_rather_than_claiming_to_be_empty() {
        assert_eq!(row_estimate(-1), "—");
        assert_eq!(row_estimate(0), "0");
        assert_eq!(row_estimate(940), "~940");
        assert_eq!(row_estimate(12_400), "~12k");
        assert_eq!(row_estimate(3_400_000), "~3.4M");
    }

    #[test]
    fn schemas_start_closed_but_the_root_does_not() {
        let nodes = from_snapshot("local", &snapshot(), None);
        let closed = initially_collapsed(&nodes);
        assert!(!closed.contains(&0));
        assert!(!closed.contains(&1));
        let public = nodes.iter().find(|n| n.name == *"public").unwrap();
        assert!(closed.contains(&public.id));
    }
}
