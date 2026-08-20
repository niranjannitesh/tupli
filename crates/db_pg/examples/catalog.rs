//! Prints what introspection makes of a server, for looking at a database this
//! app has never seen.
//!
//! ```text
//! cargo run -p db_pg --example catalog -- 'host=127.0.0.1 port=5432 db=x user=y sslmode=disable'
//! ```
//!
//! Read-only: it opens a connection, asks the catalog queries the app asks, and
//! prints. Nothing here writes, so it is safe to point at a database that
//! belongs to somebody else's afternoon.

use db::ConnectionConfig;
use db_pg::PgConnection;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();
    let spec = std::env::args().nth(1).expect("a connection spec");
    let config = ConnectionConfig::from_spec(&spec).expect("the spec");
    // The app keeps passwords in the Keychain; a command-line tool has nowhere
    // to keep one, so it reads `PGPASSWORD` like everything else in this family.
    let password = std::env::var("PGPASSWORD").ok();
    let connection = PgConnection::connect(&config, password.as_deref())
        .await
        .expect("connect");
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("introspect");

    println!(
        "server {}   {} schema(s)",
        snapshot.server_version,
        snapshot.schemas.len()
    );
    for schema in &snapshot.schemas {
        println!("\n{} — {} relation(s)", schema.name, schema.relations.len());
        for relation in &schema.relations {
            println!(
                "  {:<34} {:<18} {:>10} rows  {:>10} bytes  key {:?}",
                relation.reference.name,
                format!("{:?}", relation.kind),
                relation.estimated_rows,
                relation.size_bytes,
                relation
                    .primary_key()
                    .map(|pk| pk.columns.join(", "))
                    .unwrap_or_else(|| "—".into()),
            );
            for column in &relation.columns {
                println!(
                    "      {:<24} {:<30} {}{}{}{}",
                    column.name,
                    column.type_name,
                    if column.nullable { "null " } else { "" },
                    if column.is_identity() { "identity " } else { "" },
                    if column.is_generated {
                        "generated "
                    } else {
                        ""
                    },
                    column
                        .default
                        .as_deref()
                        .map(|d| format!("default {d}"))
                        .unwrap_or_default(),
                );
            }
            for fk in &relation.foreign_keys {
                println!(
                    "      fk {} ({}) -> {} ({})",
                    fk.name,
                    fk.columns.join(", "),
                    fk.target,
                    fk.target_columns.join(", ")
                );
            }
        }
    }
}
