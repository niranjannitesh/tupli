//! The sidebar tree, flattened.
//!
//! One `Vec` in visit order rather than a graph of parents and children. The
//! sidebar is a scrolling list of rows, and a flat list is what a list wants:
//! filtering, keyboard navigation and virtualisation are all linear passes, and
//! collapsing a branch is "skip while depth is greater", which needs no
//! bookkeeping at all.

use db::{RelationKind, RelationRef, SchemaSnapshot};
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
}

impl NodeKind {
    /// Whether double-clicking this row should open a data tab.
    pub fn is_relation(self) -> bool {
        matches!(self, Self::Table | Self::View | Self::MaterializedView)
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
    pub target: Option<RelationRef>,
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

    fn expandable(mut self) -> Self {
        self.expandable = true;
        self
    }

    fn target(mut self, target: &RelationRef) -> Self {
        self.target = Some(target.clone());
        self
    }
}

/// Build the tree for one connected server.
///
/// Grouping rows — "tables", "views", "functions" — appear only when they have
/// something in them. A schema with no views should not make the reader check.
pub fn from_snapshot(connection: &str, snapshot: &SchemaSnapshot) -> Vec<TreeNode> {
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

    for (id, node) in nodes.iter_mut().enumerate() {
        node.id = id;
    }
    nodes
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
        let nodes = from_snapshot("local", &snapshot());
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
        let nodes = from_snapshot("local", &snapshot());
        assert!(nodes.iter().enumerate().all(|(i, node)| node.id == i));
        assert_eq!(nodes[0].kind, NodeKind::Connection);
        assert_eq!(nodes[0].meta.as_deref(), Some("16.4"));
        assert_eq!(nodes[1].kind, NodeKind::Database);
    }

    #[test]
    fn empty_groups_do_not_appear() {
        let nodes = from_snapshot("local", &snapshot());
        let names: Vec<_> = nodes.iter().map(|n| n.name.as_ref()).collect();
        assert!(names.contains(&"tables"));
        assert!(names.contains(&"views"));
        assert!(!names.contains(&"functions"));
        // The schema with nothing in it is still listed — it exists.
        assert!(names.contains(&"empty"));
    }

    #[test]
    fn relations_carry_the_reference_needed_to_open_them() {
        let nodes = from_snapshot("local", &snapshot());
        let users = nodes.iter().find(|n| n.name == *"users").unwrap();
        assert_eq!(users.target.as_ref().unwrap().qualified(), "public.users");
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
        let nodes = from_snapshot("local", &snapshot());
        let closed = initially_collapsed(&nodes);
        assert!(!closed.contains(&0));
        assert!(!closed.contains(&1));
        let public = nodes.iter().find(|n| n.name == *"public").unwrap();
        assert!(closed.contains(&public.id));
    }
}
