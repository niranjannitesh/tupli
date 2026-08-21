//! The sidebar tree, flattened.
//!
//! One `Vec` in visit order rather than a graph of parents and children. The
//! sidebar is a scrolling list of rows, and a flat list is what a list wants:
//! filtering, keyboard navigation and virtualisation are all linear passes, and
//! collapsing a branch is "skip while depth is greater", which needs no
//! bookkeeping at all.

use std::sync::Arc;

use db::{
    Capabilities, KeyInfo, KeyType, Keyspace, RelationKind, RelationRef, Role, RoleSet,
    SchemaSnapshot,
};
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
    /// A hash of the path from the connection down to this row, and not its
    /// position: the sidebar holds several servers at once, each of which
    /// gains and loses rows as catalogs land, and collapse state is remembered
    /// by id. A positional id means opening a database three rows above
    /// renumbers everything below it, and every branch the reader had open
    /// folds onto a different branch.
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
    /// Which session's row this is. Every row in the tree belongs to a
    /// connection now that there is more than one of them on screen, and a
    /// click has to say which server it meant before it says what it wanted.
    pub origin: Origin,
}

/// The session a row belongs to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Origin {
    pub connection: uuid::Uuid,
    /// The database this row is under. `None` above the level where databases
    /// are chosen — the connection row itself, and the roles beside them.
    pub database: Option<SharedString>,
}

impl TreeNode {
    fn new(depth: usize, kind: NodeKind, name: impl Into<SharedString>) -> Self {
        Self {
            // Both filled in by a pass over the finished subtree — see
            // [`identify`] and [`stamp`]. Neither is knowable here: an id is a
            // hash of the path down to the row, and an origin is the same
            // answer for every row under one root.
            id: 0,
            depth,
            kind,
            name: name.into(),
            meta: None,
            expandable: false,
            target: None,
            origin: Origin {
                connection: uuid::Uuid::nil(),
                database: None,
            },
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

/// One connection as a root of the sidebar: the row, and whatever is open
/// under it.
///
/// Borrowed rather than owned because the caller is already holding every
/// session open, and the tree is rebuilt every time a catalog lands. Copying
/// the catalogs in order to describe them would be the largest allocation in
/// the app, made the most often.
pub struct Server<'a> {
    /// The saved connection's own id, which is what makes this root's node ids
    /// its own. The id and not the name: two saved connections may both be
    /// called `dev`, and a folded branch is remembered by node id.
    pub id: uuid::Uuid,
    pub name: SharedString,
    pub capabilities: Capabilities,
    /// The database the connection row itself stands for — the one a click on
    /// the row, or on the roles beside it, should act on. A database row below
    /// overrides it for its own subtree.
    pub database: Option<&'a str>,
    /// Every database on the server, in the order to draw them, each carrying
    /// its catalog if a session on it has one yet.
    pub databases: Vec<Database<'a>>,
    pub keyspace: Option<Keys<'a>>,
    pub roles: Option<&'a RoleSet>,
    /// What the row says on the right while there is nothing under it yet:
    /// where it would connect to, or how far it has got trying.
    pub state: Option<SharedString>,
}

/// A database on a server, and its catalog if it has been read.
pub struct Database<'a> {
    pub name: &'a str,
    /// `None` for a database that is on the server but that nobody has opened.
    /// It is a name with nothing under it until it is clicked.
    pub snapshot: Option<&'a SchemaSnapshot>,
}

/// A key-value server's keyspace, as far as it has been walked.
pub struct Keys<'a> {
    pub version: SharedString,
    pub keyspace: &'a Keyspace,
    pub keys: &'a [KeyInfo],
    /// Whether the walk that produced `keys` reached the end.
    pub complete: bool,
}

/// Build the whole sidebar: every connection, in the order they are saved.
///
/// Grouping rows — "tables", "views", "functions" — appear only when they have
/// something in them. A schema with no views should not make the reader check.
pub fn from_servers(servers: &[Server]) -> Vec<TreeNode> {
    let mut nodes = Vec::new();
    for server in servers {
        let from = nodes.len();
        push_server(&mut nodes, server);
        identify(&mut nodes[from..], seed(server.id));
        stamp(&mut nodes[from..], server);
    }
    nodes
}

/// Build the tree for one connected server.
pub fn from_snapshot(
    connection: &str,
    snapshot: &SchemaSnapshot,
    roles: Option<&RoleSet>,
    capabilities: Capabilities,
) -> Vec<TreeNode> {
    let names = databases(snapshot);
    let databases = match capabilities.databases {
        true => names
            .iter()
            .map(|name| Database {
                name,
                // Only the open one has a catalog: a Postgres session sees one
                // database and nothing else on the server.
                snapshot: (**name == *snapshot.database).then_some(snapshot),
            })
            .collect(),
        false => vec![Database {
            name: &snapshot.database,
            snapshot: Some(snapshot),
        }],
    };
    from_servers(&[Server {
        id: uuid::Uuid::nil(),
        name: connection.to_string().into(),
        capabilities,
        database: Some(&snapshot.database),
        databases,
        keyspace: None,
        roles,
        state: None,
    }])
}

/// Build the tree for one connected key-value server.
pub fn from_keyspace(
    connection: &str,
    version: &str,
    keyspace: &Keyspace,
    keys: &[KeyInfo],
    complete: bool,
) -> Vec<TreeNode> {
    from_servers(&[Server {
        id: uuid::Uuid::nil(),
        name: connection.to_string().into(),
        capabilities: Capabilities::REDIS,
        database: None,
        databases: Vec::new(),
        keyspace: Some(Keys {
            version: version.to_string().into(),
            keyspace,
            keys,
            complete,
        }),
        roles: None,
        state: None,
    }])
}

/// The rows of one connection, from its own row down.
fn push_server(nodes: &mut Vec<TreeNode>, server: &Server) {
    let root = nodes.len();
    let version = server
        .databases
        .iter()
        .filter_map(|db| db.snapshot)
        .map(|snapshot| short_version(&snapshot.server_version))
        .next()
        .or_else(|| server.keyspace.as_ref().map(|k| short_version(&k.version)))
        .or_else(|| server.state.as_ref().map(SharedString::to_string));
    nodes.push(TreeNode::new(0, NodeKind::Connection, server.name.clone()).when_some(version));

    match server.capabilities.databases {
        // Every database on the server, not only the open one — and every one
        // that has a catalog opens, not only the one the app is pointed at.
        // Two databases open at once is the ordinary case: a schema in one and
        // the table it is being compared against in the other.
        true => {
            for database in &server.databases {
                let mut node = TreeNode::new(1, NodeKind::Database, database.name.to_string());
                if let Some(snapshot) = database.snapshot {
                    node = node
                        .meta(plural(snapshot.schemas.len(), "schema", "schemas"))
                        .expandable();
                }
                nodes.push(node);
                if let Some(snapshot) = database.snapshot {
                    push_schemas(nodes, snapshot, 2);
                }
            }
        }
        // ClickHouse calls a schema a database and has nothing above it. The
        // level would be a row named `analytics` above a list that also
        // contains `analytics`, opening onto itself.
        false => {
            for snapshot in server.databases.iter().filter_map(|db| db.snapshot) {
                push_schemas(nodes, snapshot, 1);
            }
        }
    }

    if let Some(keys) = &server.keyspace {
        push_key_databases(nodes, keys, 1);
    }

    // After every database, because it is about all of them. Only when there
    // is more than one role: a server with nothing but `postgres` on it has no
    // access story to tell, and a folder that opens onto the name you are
    // already logged in as is a row that costs more than it says.
    if let Some(roles) = server.roles.filter(|roles| roles.roles.len() > 1) {
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

    // A connection with nothing under it is a door, not a folder: clicking it
    // connects, and a disclosure triangle over an empty branch would promise
    // rows that are not there yet. Decided by what was actually built rather
    // than by predicting it, because there are four things that can put a row
    // under a connection and every one of them can be absent.
    nodes[root].expandable = nodes.len() > root + 1;
}

/// Give every row an id that is a hash of the path down to it.
///
/// The path and not the position, so that a branch the reader folded stays
/// folded when a catalog arrives three rows above it — see [`TreeNode::id`].
/// One pass, using the fact that the list is in visit order: the stack of
/// ancestor ids is the prefix of the list at each depth.
fn identify(nodes: &mut [TreeNode], seed: u64) {
    use std::hash::{Hash, Hasher};
    let mut stack: Vec<u64> = Vec::new();
    for node in nodes {
        stack.truncate(node.depth);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stack.last().copied().unwrap_or(seed).hash(&mut hasher);
        (node.kind as u8).hash(&mut hasher);
        node.name.hash(&mut hasher);
        let id = hasher.finish();
        node.id = id as usize;
        stack.push(id);
    }
}

/// Which session each row belongs to.
///
/// A pass afterwards rather than an argument to the six builders above,
/// because it is the same answer for every row under one root and threading it
/// through would only be six chances to forget.
fn stamp(nodes: &mut [TreeNode], server: &Server) {
    let base: Option<SharedString> = server.database.map(|name| name.to_string().into());
    let mut database = base.clone();
    for node in nodes {
        // A database row and everything under it belongs to that database's
        // session; the connection row and the roles beside it do not.
        if node.depth <= 1 {
            database = base.clone();
        }
        if node.kind == NodeKind::Database {
            database = Some(node.name.clone());
        }
        node.origin = Origin {
            connection: server.id,
            database: database.clone(),
        };
    }
}

/// The connection id, as somewhere for the hashes to start.
fn seed(id: uuid::Uuid) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// Every schema in the snapshot, starting at `depth`.
///
/// Parameterised because whether there is a database row above them is an
/// engine's business, not this function's — see [`Capabilities::databases`].
fn push_schemas(nodes: &mut Vec<TreeNode>, snapshot: &SchemaSnapshot, depth: usize) {
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
            TreeNode::new(depth, NodeKind::Schema, schema.name.to_string())
                .meta(plural(schema.relations.len(), "table", "tables"))
                .expandable(),
        );

        if !tables.is_empty() {
            nodes.push(
                TreeNode::new(depth + 1, NodeKind::TableGroup, "tables")
                    .meta(tables.len().to_string())
                    .expandable(),
            );
            for relation in tables {
                nodes.push(
                    TreeNode::new(
                        depth + 2,
                        NodeKind::Table,
                        relation.reference.name.to_string(),
                    )
                    .meta(row_estimate(relation.estimated_rows))
                    .target(&relation.reference),
                );
            }
        }

        if !views.is_empty() {
            nodes.push(
                TreeNode::new(depth + 1, NodeKind::TableGroup, "views")
                    .meta(views.len().to_string())
                    .expandable(),
            );
            for relation in views {
                let kind = match relation.kind {
                    RelationKind::MaterializedView => NodeKind::MaterializedView,
                    _ => NodeKind::View,
                };
                nodes.push(
                    TreeNode::new(depth + 2, kind, relation.reference.name.to_string())
                        .target(&relation.reference),
                );
            }
        }

        if !schema.routines.is_empty() {
            nodes.push(
                TreeNode::new(depth + 1, NodeKind::FunctionGroup, "functions")
                    .meta(schema.routines.len().to_string())
                    .expandable(),
            );
            for routine in &schema.routines {
                nodes.push(TreeNode::new(
                    depth + 2,
                    NodeKind::Function,
                    routine.name.to_string(),
                ));
            }
        }
    }
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

/// The `DB n` rows of a key-value server.
///
/// The shape below them is a lie that everybody tells: Redis has one flat
/// namespace and no folders at all, but every keyspace in the world is named
/// `user:1:cart` and read as though the colons were slashes. So the colons are
/// treated as slashes here, and the folders are labelled with what is actually
/// under them rather than with a promise — see [`scanned`].
///
/// `Keys::complete` is whether the walk that produced them reached the end. It
/// decides the wording and nothing else: a browser that has seen eight
/// thousand of ten million keys must not draw a tree that looks like the
/// keyspace.
fn push_key_databases(nodes: &mut Vec<TreeNode>, keys: &Keys, depth: usize) {
    for database in &keys.keyspace.databases {
        let current = database.index == keys.keyspace.current;
        let mut node = TreeNode::new(
            depth,
            NodeKind::KeyDatabase,
            format!("DB {}", database.index),
        )
        .meta(plural(database.keys as usize, "key", "keys"));
        // Only the open database has keys to show. Switching to another one
        // is a `SELECT` on the connection, which is what clicking it does.
        if current {
            node = node.expandable();
        }
        nodes.push(node);
        if current {
            push_keys(nodes, keys.keys, depth + 1);
        }
    }

    // A server that would not answer `INFO keyspace`, or one whose databases
    // are all empty: the connection is open and there is nothing under it, and
    // a tree with no database in it at all would read as a failure to connect.
    if keys.keyspace.databases.is_empty() {
        nodes.push(
            TreeNode::new(
                depth,
                NodeKind::KeyDatabase,
                format!("DB {}", keys.keyspace.current),
            )
            .meta(scanned(keys.keys.len(), keys.complete))
            .expandable(),
        );
        push_keys(nodes, keys.keys, depth + 1);
    }
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
///
/// Judged one catalog at a time. "One schema, so open it" is a fact about a
/// single database, and with several of them on screen at once a count over
/// the whole tree would let one connection decide how another one opens.
pub fn initially_collapsed(nodes: &[TreeNode]) -> Vec<usize> {
    let mut ids = Vec::new();
    let mut rest: Vec<&TreeNode> = Vec::new();
    let mut index = 0;
    while index < nodes.len() {
        if nodes[index].kind == NodeKind::Database {
            let end = extent(nodes, index);
            // The database row itself is not part of the scope it heads: it
            // stays open either way, and counting it in would put the floor a
            // level too high.
            rest.push(&nodes[index]);
            collapse(&nodes[index + 1..end].iter().collect::<Vec<_>>(), &mut ids);
            index = end;
            continue;
        }
        rest.push(&nodes[index]);
        index += 1;
    }
    collapse(&rest, &mut ids);
    ids
}

/// One catalog's worth of rows, judged on its own.
fn collapse(nodes: &[&TreeNode], ids: &mut Vec<usize>) {
    let schemas = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Schema)
        .count();
    // Measured rather than assumed: an engine with no database level puts its
    // schemas a row higher, and a floor counted from the top would leave every
    // one of them open.
    let level = nodes
        .iter()
        .find(|node| node.kind == NodeKind::Schema)
        .map_or(2, |node| node.depth);
    // …and not if that schema is enormous. A thousand tables listed on arrival
    // is a scroll bar, not a starting point.
    let objects = nodes.iter().filter(|node| node.depth >= level + 2).count();
    let floor = match schemas == 1 && objects <= 200 {
        true => level + 2,
        false => level,
    };
    ids.extend(
        nodes
            .iter()
            .filter(|node| node.expandable && node.depth >= floor)
            .map(|node| node.id),
    );
}

/// Where the subtree rooted at `start` ends.
fn extent(nodes: &[TreeNode], start: usize) -> usize {
    let depth = nodes[start].depth;
    let mut end = start + 1;
    while end < nodes.len() && nodes[end].depth > depth {
        end += 1;
    }
    end
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
            create_statement: None,
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
        let nodes = from_snapshot("local", &snapshot(), None, Capabilities::POSTGRES);
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
    fn the_tree_is_flat_and_in_visit_order() {
        let nodes = from_snapshot("local", &snapshot(), None, Capabilities::POSTGRES);
        assert_eq!(nodes[0].kind, NodeKind::Connection);
        assert_eq!(nodes[0].meta.as_deref(), Some("16.4"));
        assert_eq!(nodes[1].kind, NodeKind::Database);
        assert_eq!(nodes[1].depth, 1);
        let ids: std::collections::HashSet<usize> = nodes.iter().map(|node| node.id).collect();
        assert_eq!(ids.len(), nodes.len(), "every row is its own key");
    }

    #[test]
    fn a_rows_id_is_its_path_and_survives_another_database_opening() {
        let snapshot = snapshot();
        let server = |databases| Server {
            id: uuid::Uuid::from_u128(7),
            name: "local".into(),
            capabilities: Capabilities::POSTGRES,
            database: Some("app"),
            databases,
            keyspace: None,
            roles: None,
            state: None,
        };
        let one = from_servers(&[server(vec![Database {
            name: "app",
            snapshot: Some(&snapshot),
        }])]);
        // `app_test` opens too, and sorts *before* `app` — so every row of
        // `app` moves down the list. Positional ids would renumber all of
        // them, and the reader's folded branches would land somewhere else.
        let two = from_servers(&[server(vec![
            Database {
                name: "app_test",
                snapshot: Some(&snapshot),
            },
            Database {
                name: "app",
                snapshot: Some(&snapshot),
            },
        ])]);
        let users = |nodes: &[TreeNode]| {
            nodes
                .iter()
                .find(|node| {
                    node.kind == NodeKind::Table
                        && node.name == "users"
                        && node.origin.database.as_deref() == Some("app")
                })
                .map(|node| node.id)
                .expect("app.public.users")
        };
        assert_eq!(users(&one), users(&two));
    }

    #[test]
    fn two_connections_with_the_same_name_do_not_share_a_single_id() {
        let snapshot = snapshot();
        let server = |id| Server {
            id,
            name: "dev".into(),
            capabilities: Capabilities::POSTGRES,
            database: Some("app"),
            databases: vec![Database {
                name: "app",
                snapshot: Some(&snapshot),
            }],
            keyspace: None,
            roles: None,
            state: None,
        };
        let nodes = from_servers(&[
            server(uuid::Uuid::from_u128(1)),
            server(uuid::Uuid::from_u128(2)),
        ]);
        let ids: std::collections::HashSet<usize> = nodes.iter().map(|node| node.id).collect();
        assert_eq!(ids.len(), nodes.len());
        // …and each row knows which of the two it came out of, which is the
        // only way a click on the second `app` reaches the second server.
        let roots: Vec<_> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Connection)
            .map(|node| node.origin.connection)
            .collect();
        assert_eq!(
            roots,
            vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)]
        );
    }

    #[test]
    fn a_database_nobody_has_opened_is_a_name_and_a_click() {
        let snapshot = snapshot();
        let nodes = from_servers(&[Server {
            id: uuid::Uuid::nil(),
            name: "local".into(),
            capabilities: Capabilities::POSTGRES,
            database: None,
            databases: vec![
                Database {
                    name: "app",
                    snapshot: Some(&snapshot),
                },
                Database {
                    name: "app_test",
                    snapshot: None,
                },
            ],
            keyspace: None,
            roles: None,
            state: None,
        }]);
        let databases: Vec<_> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Database)
            .collect();
        assert!(databases[0].expandable);
        assert!(!databases[1].expandable);
        // Both open at once is the case this whole tree exists for: the second
        // one's rows are under it, not instead of the first one's.
        assert!(nodes
            .iter()
            .any(|node| node.origin.database.as_deref() == Some("app")
                && node.kind == NodeKind::Schema));
    }

    #[test]
    fn a_connection_with_nothing_open_says_where_it_would_go() {
        let nodes = from_servers(&[Server {
            id: uuid::Uuid::nil(),
            name: "staging".into(),
            capabilities: Capabilities::POSTGRES,
            database: None,
            databases: Vec::new(),
            keyspace: None,
            roles: None,
            state: Some("db.internal:5432".into()),
        }]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].meta.as_deref(), Some("db.internal:5432"));
        // No triangle: there is nothing behind it until the click that
        // connects, and an empty branch would promise rows that are not there.
        assert!(!nodes[0].expandable);
    }

    #[test]
    fn empty_groups_do_not_appear() {
        let nodes = from_snapshot("local", &snapshot(), None, Capabilities::POSTGRES);
        let names: Vec<_> = nodes.iter().map(|n| n.name.as_ref()).collect();
        assert!(names.contains(&"tables"));
        assert!(names.contains(&"views"));
        assert!(!names.contains(&"functions"));
        // The schema with nothing in it is still listed — it exists.
        assert!(names.contains(&"empty"));
    }

    #[test]
    fn relations_carry_the_reference_needed_to_open_them() {
        let nodes = from_snapshot("local", &snapshot(), None, Capabilities::POSTGRES);
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
    fn a_server_whose_schemas_are_its_databases_gets_one_level_not_two() {
        // ClickHouse's `analytics` is a schema, and drawing it under a database
        // row of the same name is a row that opens onto itself.
        let nodes = from_snapshot("local", &snapshot(), None, Capabilities::CLICKHOUSE);
        assert!(!nodes.iter().any(|node| node.kind == NodeKind::Database));
        let schema = nodes
            .iter()
            .find(|node| node.kind == NodeKind::Schema)
            .expect("a schema");
        assert_eq!(schema.depth, 1);
        // And the rows under it move up with it rather than leaving a gap.
        assert!(nodes
            .iter()
            .any(|node| node.kind == NodeKind::Table && node.depth == 3));
    }

    #[test]
    fn schemas_start_closed_but_the_root_does_not() {
        let nodes = from_snapshot("local", &snapshot(), None, Capabilities::POSTGRES);
        let closed = initially_collapsed(&nodes);
        assert!(!closed.contains(&0));
        assert!(!closed.contains(&1));
        let public = nodes.iter().find(|n| n.name == *"public").unwrap();
        assert!(closed.contains(&public.id));
    }
}
