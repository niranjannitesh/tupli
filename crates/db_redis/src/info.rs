//! What the server says about itself.
//!
//! `INFO`, `CLIENT LIST` and `SLOWLOG` are the three answers a database client
//! needs and none of them is a table: `INFO` is sectioned `key:value` text,
//! `CLIENT LIST` is one `key=value` line per client, and `SLOWLOG` is nested
//! arrays. Each becomes a [`ResultSet`] here, so the same grid draws them.
//!
//! The parsing is deliberately forgiving. These formats have grown a field at
//! a time for fifteen years and every fork adds its own; a parser that refused
//! a line it did not recognise would break on the next release of something.

use db::{ColumnMeta, DbResult, ResultSet, ValueKind};

use crate::client::{argv, RedisConnection};
use crate::resp::RespValue;
use crate::rows;

/// One `# Section` of an `INFO` reply.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

impl Section {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One non-empty database, as the `Keyspace` section reports it.
///
/// This is how the sidebar knows which of the sixteen databases are worth
/// showing: Redis has no catalog to ask, and probing all sixteen with
/// `DBSIZE` is sixteen round trips for a question `INFO` already answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Database {
    pub index: u8,
    pub keys: u64,
    /// How many of those keys have an expiry.
    pub expires: u64,
}

/// The whole `INFO`, parsed.
pub async fn info(conn: &RedisConnection) -> DbResult<Vec<Section>> {
    let text = conn.text(&argv([b"INFO", b"everything"])).await;
    // `everything` arrived in 6.0; older servers want `all`, and the oldest
    // only understand `INFO` on its own.
    let text = match text {
        Ok(text) => text,
        Err(_) => match conn.text(&argv([b"INFO", b"all"])).await {
            Ok(text) => text,
            Err(_) => conn.text(&argv([b"INFO"])).await?,
        },
    };
    Ok(parse(&text))
}

/// The databases that have anything in them.
pub async fn databases(conn: &RedisConnection) -> DbResult<Vec<Database>> {
    let text = conn.text(&argv([b"INFO", b"keyspace"])).await?;
    Ok(databases_in(&parse(&text)))
}

/// Everyone connected, as a table.
pub async fn clients(conn: &RedisConnection) -> DbResult<ResultSet> {
    let text = conn.text(&argv([b"CLIENT", b"LIST"])).await?;
    Ok(parse_client_list(&text))
}

/// The slow query log, as a table.
pub async fn slowlog(conn: &RedisConnection, count: usize) -> DbResult<ResultSet> {
    let reply = conn
        .command(&argv([b"SLOWLOG", b"GET", count.to_string().as_bytes()]))
        .await?;
    Ok(parse_slowlog(&reply))
}

/// The server's configuration, as a table.
pub async fn config(conn: &RedisConnection, pattern: &str) -> DbResult<ResultSet> {
    let reply = conn
        .command(&argv([b"CONFIG", b"GET", pattern.as_bytes()]))
        .await?;
    let pairs = crate::keys::pairs(&reply);
    let names: Vec<_> = pairs.iter().map(|(name, _)| name.clone()).collect();
    let values: Vec<_> = pairs.iter().map(|(_, value)| value.clone()).collect();
    Ok(ResultSet::new(vec![
        rows::nullable_value_column("parameter", &names),
        rows::nullable_value_column("value", &values),
    ]))
}

/// `INFO` text → sections.
pub fn parse(text: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(name) = line.strip_prefix('#') {
            sections.push(Section {
                name: name.trim().to_string(),
                fields: Vec::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // A `key:value` before any `# Section` header. Real servers do not do
        // this, but a proxy in front of one might.
        let section = match sections.last_mut() {
            Some(section) => section,
            None => {
                sections.push(Section {
                    name: "Server".into(),
                    fields: Vec::new(),
                });
                sections.last_mut().expect("just pushed")
            }
        };
        section
            .fields
            .push((key.trim().to_string(), value.trim().to_string()));
    }
    sections
}

/// The `Keyspace` section → the databases with keys in them.
///
/// Lines read `db0:keys=41,expires=2,avg_ttl=0`. A database with no keys has
/// no line at all, which is exactly the distinction the sidebar wants.
pub fn databases_in(sections: &[Section]) -> Vec<Database> {
    let mut databases = Vec::new();
    for section in sections.iter().filter(|s| s.name.eq_ignore_ascii_case("keyspace")) {
        for (name, stats) in &section.fields {
            let Some(index) = name.strip_prefix("db").and_then(|n| n.parse().ok()) else {
                continue;
            };
            let field = |wanted: &str| {
                stats
                    .split(',')
                    .filter_map(|part| part.split_once('='))
                    .find(|(key, _)| *key == wanted)
                    .and_then(|(_, value)| value.parse().ok())
                    .unwrap_or(0)
            };
            databases.push(Database {
                index,
                keys: field("keys"),
                expires: field("expires"),
            });
        }
    }
    databases.sort_by_key(|database| database.index);
    databases
}

/// Sections → a three-column table, for the info pane.
pub fn to_result_set(sections: &[Section]) -> ResultSet {
    let rows: Vec<_> = sections
        .iter()
        .flat_map(|section| {
            section
                .fields
                .iter()
                .map(move |(key, value)| (section.name.as_str(), key.as_str(), value.as_str()))
        })
        .collect();
    let column = |name: &str, values: Vec<&str>| {
        rows::text_column(
            ColumnMeta::new(name, ValueKind::Text, "string"),
            values.into_iter().map(Some),
        )
    };
    ResultSet::new(vec![
        column("section", rows.iter().map(|row| row.0).collect()),
        column("field", rows.iter().map(|row| row.1).collect()),
        column("value", rows.iter().map(|row| row.2).collect()),
    ])
}

/// `CLIENT LIST` text → a table.
///
/// One line per client, each `key=value` separated by spaces, and the set of
/// keys grows with every release. So the columns are whatever the reply
/// actually contained, in the order it contained them, and a client missing a
/// field gets a null rather than a shifted row.
pub fn parse_client_list(text: &str) -> ResultSet {
    let mut names: Vec<&str> = Vec::new();
    let mut clients: Vec<Vec<(&str, &str)>> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line
            .split(' ')
            .filter_map(|field| field.split_once('='))
            .collect();
        for (name, _) in &fields {
            if !names.contains(name) {
                names.push(name);
            }
        }
        clients.push(fields);
    }

    let columns = names
        .iter()
        .map(|name| {
            let values: Vec<_> = clients
                .iter()
                .map(|client| {
                    client
                        .iter()
                        .find(|(field, _)| field == name)
                        .map(|(_, value)| *value)
                })
                .collect();
            // `id` and `age` are numbers and read better aligned as numbers,
            // but every other field is text and several are text that looks
            // numeric. Not worth the guessing: the pane sorts by string.
            rows::text_column(
                ColumnMeta::new(*name, ValueKind::Text, "string"),
                values.iter().copied(),
            )
        })
        .collect();
    ResultSet::new(columns)
}

/// A `SLOWLOG GET` reply → a table.
///
/// Each entry is `[id, unix time, microseconds, [command…], client, name]`.
/// The command is flattened back into one line, which is what makes the log
/// readable — and what the console can paste to run it again.
pub fn parse_slowlog(reply: &RespValue) -> ResultSet {
    let RespValue::Array(entries) = reply else {
        return ResultSet::new(Vec::new());
    };
    let field = |entry: &RespValue, at: usize| match entry {
        RespValue::Array(parts) => parts.get(at).cloned(),
        _ => None,
    };
    let numbers = |at: usize| -> Vec<Option<i64>> {
        entries
            .iter()
            .map(|entry| field(entry, at).and_then(|value| value.as_i64()))
            .collect()
    };
    let strings = |at: usize| -> Vec<Option<Vec<u8>>> {
        entries
            .iter()
            .map(|entry| field(entry, at).and_then(|value| value.as_bytes()))
            .collect()
    };
    let commands: Vec<Option<Vec<u8>>> = entries
        .iter()
        .map(|entry| match field(entry, 3) {
            Some(RespValue::Array(args)) => Some(
                args.iter()
                    .filter_map(RespValue::as_bytes)
                    .collect::<Vec<_>>()
                    .join(&b' '),
            ),
            other => other.and_then(|value| value.as_bytes()),
        })
        .collect();

    ResultSet::new(vec![
        rows::int_column(ColumnMeta::new("id", ValueKind::Int, "integer"), numbers(0)),
        rows::int_column(
            ColumnMeta::new("at", ValueKind::Int, "unix time"),
            numbers(1),
        ),
        rows::int_column(
            ColumnMeta::new("microseconds", ValueKind::Int, "integer"),
            numbers(2),
        ),
        rows::nullable_value_column("command", &commands),
        rows::nullable_value_column("client", &strings(4)),
        rows::nullable_value_column("name", &strings(5)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Server\r\nredis_version:7.2.4\r\nuptime_in_seconds:900\r\n\r\n# Keyspace\r\ndb0:keys=41,expires=2,avg_ttl=0\r\ndb3:keys=7,expires=0,avg_ttl=0\r\n";

    #[test]
    fn info_splits_into_sections() {
        let sections = parse(SAMPLE);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].name, "Server");
        assert_eq!(sections[0].get("redis_version"), Some("7.2.4"));
        assert_eq!(sections[0].get("nothing"), None);
        assert_eq!(sections[1].fields.len(), 2);
    }

    #[test]
    fn a_value_containing_a_colon_keeps_it() {
        // `executable:/usr/bin/redis-server` and every `addr:host:port` field.
        let sections = parse("# Server\nexecutable:/usr/local/bin/redis-server\n");
        assert_eq!(
            sections[0].get("executable"),
            Some("/usr/local/bin/redis-server")
        );
    }

    #[test]
    fn the_keyspace_section_says_which_databases_exist() {
        // The point of parsing this at all: fifteen of the sixteen databases
        // are usually empty, and a sidebar listing all sixteen would be
        // sixteen things to ignore.
        let databases = databases_in(&parse(SAMPLE));
        assert_eq!(
            databases,
            vec![
                Database { index: 0, keys: 41, expires: 2 },
                Database { index: 3, keys: 7, expires: 0 },
            ]
        );
        assert!(databases_in(&parse("# Keyspace\n")).is_empty());
    }

    #[test]
    fn info_becomes_a_table_of_every_field() {
        let rows = to_result_set(&parse(SAMPLE));
        assert_eq!(rows.column_count(), 3);
        assert_eq!(rows.row_count(), 4);
    }

    #[test]
    fn client_list_columns_are_whatever_the_server_sent() {
        let text = "id=4 addr=127.0.0.1:60086 name= age=9 cmd=client|list\n\
                    id=5 addr=127.0.0.1:60087 name=worker age=1 cmd=get lib-name=tupli\n";
        let rows = parse_client_list(text);
        // Five fields from the first line, plus the one only the second has.
        assert_eq!(rows.column_count(), 6);
        assert_eq!(rows.row_count(), 2);
        let mut scratch = String::new();
        // The client that did not report `lib-name` gets a null, not a shift.
        assert!(matches!(
            rows.columns[5].render(0, &mut scratch),
            db::CellText::Null
        ));
    }

    #[test]
    fn a_slowlog_entry_keeps_its_whole_command() {
        let entry = RespValue::Array(vec![
            RespValue::Int(3),
            RespValue::Int(1_700_000_000),
            RespValue::Int(15_000),
            RespValue::Array(vec![
                RespValue::Bulk(b"KEYS".to_vec()),
                RespValue::Bulk(b"*".to_vec()),
            ]),
            RespValue::Bulk(b"127.0.0.1:60086".to_vec()),
            RespValue::Bulk(b"".to_vec()),
        ]);
        let rows = parse_slowlog(&RespValue::Array(vec![entry]));
        assert_eq!(rows.column_count(), 6);
        assert_eq!(rows.row_count(), 1);
        let mut scratch = String::new();
        assert!(matches!(
            rows.columns[3].render(0, &mut scratch),
            db::CellText::Borrowed("KEYS *")
        ));
        // An empty log is an empty table and not an error.
        assert_eq!(parse_slowlog(&RespValue::Array(vec![])).row_count(), 0);
        assert_eq!(parse_slowlog(&RespValue::Nil).column_count(), 0);
    }
}
