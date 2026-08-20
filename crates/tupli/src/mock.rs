//! Placeholder content.
//!
//! The layout has to be judged against realistic text — long identifiers, wide
//! numbers, NULLs — long before the Postgres driver lands. Everything here is
//! shaped like what `db` will eventually hand us so swapping it out is a type
//! change, not a rewrite.

use db::{ColumnData, ColumnMeta, NullMask, ResultSet, TextColumnBuilder, ValueKind};
use gpui::SharedString;

pub use crate::tree::{NodeKind, TreeNode};

use crate::results::{MessageTone, RunMessage};

/// A sample Messages log, for judging the layout of a tab that is empty until
/// somebody has actually run something. `at_ms` is fixed rather than relative
/// to now, so two screenshots taken a minute apart are byte-identical.
pub fn messages() -> Vec<RunMessage> {
    use std::time::Duration;
    let base = 1_755_600_000_000i64; // a fixed wall clock, not `now`.
                                     // The last field is what the server said on the side, as `severity` and
                                     // `message` — one statement here has a `WARNING` because a log with none
                                     // of them never gets its layout looked at.
    let rows: &[(i64, &str, u64, MessageTone, &str, &[(&str, &str)])] = &[
        (0, "select * from users order by created_at desc limit 500", 41, MessageTone::Ok, "500 rows", &[]),
        (
            7_000,
            "update users set is_active = false where last_seen_at < now() - interval '1 year'",
            812,
            MessageTone::Ok,
            "1,204 affected",
            &[("WARNING", "1204 rows deactivated by a statement with no explicit transaction")],
        ),
        (
            31_000,
            "select u.id, u.email, o.name\n  from users u\n  join organisations o on o.id = u.organization_id",
            12,
            MessageTone::Failed,
            "relation \"organisations\" does not exist · Perhaps you meant to reference the table \"organizations\".",
            &[],
        ),
        (
            44_000,
            "select count(*) from webhook_deliveries where status = 'failed'",
            2_310,
            MessageTone::Ok,
            "1 row",
            &[("NOTICE", "index scan skipped: \"webhook_deliveries_status_idx\" is not valid")],
        ),
    ];
    rows.iter()
        .map(|(offset, sql, ms, tone, text, notices)| RunMessage {
            at_ms: base + offset,
            sql: (*sql).into(),
            elapsed: Duration::from_millis(*ms),
            tone: *tone,
            text: (*text).into(),
            notices: notices
                .iter()
                .map(|(severity, message)| db::Notice {
                    severity: (*severity).into(),
                    message: (*message).into(),
                    detail: None,
                    hint: None,
                })
                .collect(),
        })
        .collect()
}

pub fn tree() -> Vec<TreeNode> {
    let rows: &[(usize, NodeKind, &str, Option<&str>, bool)] = &[
        (0, NodeKind::Connection, "local · postgres@16", None, true),
        (1, NodeKind::Database, "tupli_dev", None, true),
        (2, NodeKind::SchemaGroup, "schemas", Some("3"), true),
        (3, NodeKind::Schema, "public", None, true),
        (4, NodeKind::TableGroup, "tables", Some("9"), true),
        (5, NodeKind::Table, "accounts", None, true),
        (5, NodeKind::Table, "audit_log", None, true),
        (5, NodeKind::Table, "invoice_line_items", None, true),
        (5, NodeKind::Table, "invoices", None, true),
        (5, NodeKind::Table, "organizations", None, true),
        (5, NodeKind::Table, "sessions", None, true),
        (5, NodeKind::Table, "subscriptions", None, true),
        (5, NodeKind::Table, "users", None, true),
        (5, NodeKind::Table, "webhook_deliveries", None, true),
        (4, NodeKind::View, "active_subscriptions", None, true),
        (4, NodeKind::MaterializedView, "mrr_by_month", None, true),
        (4, NodeKind::FunctionGroup, "functions", Some("4"), true),
        (3, NodeKind::Schema, "analytics", None, false),
        (3, NodeKind::Schema, "audit", None, false),
        (1, NodeKind::Database, "tupli_test", None, false),
        (0, NodeKind::Connection, "staging · rds", None, false),
    ];

    rows.iter()
        .enumerate()
        .map(|(id, &(depth, kind, name, meta, expandable))| TreeNode {
            id,
            depth,
            kind,
            name: name.into(),
            meta: meta.map(Into::into),
            expandable,
            // The real tree hangs a relation off every table and view row, and
            // anything that reads the tree — the palette's object list, most
            // of all — is only exercised if the mock does too.
            target: kind.is_relation().then(|| {
                crate::tree::Target::Relation(db::RelationRef::new("public", name))
            }),
        })
        .collect()
}

/// Deterministic pseudo-random rows — no `rand` dependency and identical across
/// runs, which matters when comparing screenshots.
pub fn cell(row: usize, col: usize) -> Option<SharedString> {
    let h = {
        let mut x = (row as u64)
            .wrapping_mul(6364136223846793005)
            .wrapping_add(col as u64 + 1);
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        x
    };
    let plans = ["free", "pro", "team", "enterprise"];
    let names = [
        "Ada Lovelace",
        "Grace Hopper",
        "Alan Turing",
        "Barbara Liskov",
        "Ken Thompson",
        "Radia Perlman",
        "Leslie Lamport",
        "Margaret Hamilton",
    ];
    Some(match col {
        0 => format!("{}", row + 1).into(),
        1 => format!("user{}@example.com", row + 1).into(),
        2 => {
            if h % 17 == 0 {
                return None;
            }
            names[(h % names.len() as u64) as usize].into()
        }
        3 => format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            h as u32,
            (h >> 16) as u16,
            (h >> 32) & 0xfff,
            ((h >> 48) as u16 & 0x3fff) | 0x8000,
            h & 0xffff_ffff_ffff
        )
        .into(),
        4 => plans[(h % 4) as usize].into(),
        5 => {
            if h % 11 == 0 {
                return None;
            }
            format!("{}", (h % 500_00) as f64 / 100.).into()
        }
        6 => if h % 3 == 0 { "false" } else { "true" }.into(),
        _ => format!(
            "2026-{:02}-{:02} {:02}:{:02}:{:02}+00",
            1 + h % 12,
            1 + (h >> 8) % 28,
            (h >> 16) % 24,
            (h >> 24) % 60,
            (h >> 32) % 60
        )
        .into(),
    })
}

pub const SAMPLE_SQL: &str = r#"-- Monthly recurring revenue by plan, current organizations only.
select
    o.name                as organization,
    u.plan,
    count(*)              as seats,
    sum(u.mrr_cents) / 100.0 as mrr
from public.users u
join public.organizations o on o.id = u.organization_id
where u.is_active
  and u.created_at >= now() - interval '12 months'
group by 1, 2
having sum(u.mrr_cents) > 0
order by mrr desc
limit 100;
"#;

/// The mock `public.users` result set, in the columnar layout the driver will
/// eventually produce. Same generator as [`cell`], so the inspector and the
/// grid still agree while both exist.
///
/// `rows` is a parameter because the honest answer to "how many rows" is "as
/// many as you asked for": the app defaults to a hundred thousand, which builds
/// in a blink, and the benchmark asks for a million.
pub fn result_set(rows: usize) -> ResultSet {
    let mut id = Vec::with_capacity(rows);
    let mut email = TextColumnBuilder::new();
    let mut full_name = TextColumnBuilder::new();
    let mut org = TextColumnBuilder::new();
    let mut plan = TextColumnBuilder::new();
    let mut mrr = Vec::with_capacity(rows);
    let mut mrr_nulls = NullMask::with_capacity(rows);
    let mut active = Vec::with_capacity(rows);
    let mut created = TextColumnBuilder::new();
    let mut buf = String::new();

    for row in 0..rows {
        id.push(row as i64 + 1);

        buf.clear();
        use std::fmt::Write as _;
        let _ = write!(buf, "user{}@example.com", row + 1);
        email.push(Some(&buf));

        full_name.push(cell(row, 2).as_deref());
        org.push(cell(row, 3).as_deref());
        plan.push(cell(row, 4).as_deref());

        match cell(row, 5) {
            Some(v) => {
                mrr_nulls.push(false, row);
                mrr.push(v.parse::<f64>().unwrap_or(0.));
            }
            None => {
                mrr_nulls.push(true, row);
                mrr.push(0.);
            }
        }

        active.push(cell(row, 6).as_deref() == Some("true"));
        created.push(cell(row, 7).as_deref());
    }

    let mut nulls = NullMask::with_capacity(rows);
    for row in 0..rows {
        nulls.push(false, row);
    }

    ResultSet::new(vec![
        db::Column {
            meta: ColumnMeta::new("id", ValueKind::Int, "int8")
                .pk()
                .not_null(),
            nulls: nulls.clone(),
            data: ColumnData::I64(id),
        },
        email.finish(ColumnMeta::new("email", ValueKind::Text, "text").not_null()),
        full_name.finish(ColumnMeta::new("full_name", ValueKind::Text, "text")),
        org.finish(
            ColumnMeta::new("organization_id", ValueKind::Uuid, "uuid")
                .fk()
                .not_null(),
        ),
        plan.finish(ColumnMeta::new("plan", ValueKind::Text, "text").not_null()),
        db::Column {
            meta: ColumnMeta::new("mrr_cents", ValueKind::Decimal, "numeric(12,2)"),
            nulls: mrr_nulls,
            data: ColumnData::F64(mrr),
        },
        db::Column {
            meta: ColumnMeta::new("is_active", ValueKind::Bool, "bool").not_null(),
            nulls: nulls.clone(),
            data: ColumnData::Bool(active),
        },
        created
            .finish(ColumnMeta::new("created_at", ValueKind::Timestamp, "timestamptz").not_null()),
    ])
}
