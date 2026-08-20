//! Reading the catalog.
//!
//! Seven bulk queries against `pg_catalog`, not one query per object. A database
//! with two thousand tables would otherwise cost two thousand round trips to
//! draw a sidebar, and the sidebar is the first thing anyone sees.
//!
//! `pg_catalog` rather than `information_schema` throughout: the standard views
//! are defined in terms of joins and permission checks that make them an order
//! of magnitude slower, and they hide exactly the Postgres-specific columns —
//! `relkind`, `attidentity`, `indpred` — that a Postgres client needs.

use std::collections::HashMap;
use std::sync::Arc;

use db::{
    CheckConstraint, ColumnDef, DbResult, ErrorClass, ForeignKey, IndexDef, RefAction, Relation,
    RelationKind, RelationRef, Routine, Schema, SchemaSnapshot, TriggerDef,
};
use tokio_postgres::Row;

use crate::client::{classify, PgConnection};
use crate::types;

/// Schemas that are never interesting and are excluded at the source, so the
/// bytes never leave the server.
const HIDDEN: &str = "n.nspname not in ('pg_catalog', 'information_schema') \
     and n.nspname not like 'pg\\_toast%' and n.nspname not like 'pg\\_temp%'";

/// Read the whole catalog.
pub async fn snapshot(connection: &PgConnection) -> DbResult<SchemaSnapshot> {
    let client = connection.client();

    let database = connection.scalar("select current_database()").await?;
    let current_schema = connection.scalar("select current_schema()").await?;
    let search_path = search_path(connection).await?;
    let databases = databases(connection).await?;

    let mut schemas = load_schemas(client).await?;
    // Every later query names its schema as a string and is placed by this
    // map. An earlier version paired the lists by position, which is wrong the
    // moment a schema holds no relations — or no routines — and so drops out
    // of one list while staying in the other.
    let by_name: HashMap<Arc<str>, usize> = schemas
        .iter()
        .enumerate()
        .map(|(position, schema)| (schema.name.clone(), position))
        .collect();
    let mut index: HashMap<RelationRef, (usize, usize)> = HashMap::new();

    for relation in load_relations(client).await? {
        let Some(&schema_index) = by_name.get(&relation.reference.schema) else {
            continue;
        };
        let Some(schema) = schemas.get_mut(schema_index) else {
            continue;
        };
        index.insert(
            relation.reference.clone(),
            (schema_index, schema.relations.len()),
        );
        schema.relations.push(relation);
    }

    // Columns, indexes and foreign keys arrive as flat lists over the whole
    // database and are dropped onto their relation by name. Anything naming a
    // relation the first query did not return — a schema the user cannot see
    // into, most often — is skipped rather than treated as an error.
    for (reference, column) in load_columns(client).await? {
        if let Some(relation) = lookup(&mut schemas, &index, &reference) {
            relation.columns.push(column);
        }
    }
    for (reference, index_def) in load_indexes(client).await? {
        if let Some(relation) = lookup(&mut schemas, &index, &reference) {
            relation.indexes.push(index_def);
        }
    }
    for (reference, fk) in load_foreign_keys(client).await? {
        if let Some(relation) = lookup(&mut schemas, &index, &reference) {
            relation.foreign_keys.push(fk);
        }
    }
    for (reference, check) in load_checks(client).await? {
        if let Some(relation) = lookup(&mut schemas, &index, &reference) {
            relation.checks.push(check);
        }
    }
    for (reference, trigger) in load_triggers(client).await? {
        if let Some(relation) = lookup(&mut schemas, &index, &reference) {
            relation.triggers.push(trigger);
        }
    }
    for schema in &mut schemas {
        for relation in &mut schema.relations {
            relation.detail_loaded = true;
        }
    }

    for routine in load_routines(client).await? {
        if let Some(schema) = by_name
            .get(&routine.schema)
            .and_then(|&i| schemas.get_mut(i))
        {
            schema.routines.push(routine);
        }
    }

    Ok(SchemaSnapshot {
        database: database.into(),
        databases,
        server_version: connection.server_version().clone(),
        current_schema: current_schema.into(),
        search_path,
        schemas,
    })
}

fn lookup<'a>(
    schemas: &'a mut [Schema],
    index: &HashMap<RelationRef, (usize, usize)>,
    reference: &RelationRef,
) -> Option<&'a mut Relation> {
    let (schema, relation) = *index.get(reference)?;
    schemas.get_mut(schema)?.relations.get_mut(relation)
}

/// `search_path` as the session actually resolves it.
///
/// `show search_path` returns the literal setting, `"$user", public`, and the
/// `$user` entry only counts if a schema by that name exists. Asking the server
/// with `current_schemas` gets the resolved answer instead of reimplementing
/// the rule here.
async fn search_path(connection: &PgConnection) -> DbResult<Vec<Arc<str>>> {
    let rows = connection
        .client()
        .query("select unnest(current_schemas(true))::text", &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;
    Ok(rows.iter().map(|row| text(row, 0)).collect())
}

/// The other databases on this server.
///
/// Templates are left out — connecting to `template0` is refused and
/// connecting to `template1` is a way to break `createdb` — and so is anything
/// with `datallowconn` off, which is the server saying the same thing about a
/// database being restored or retired. What is left is the list the sidebar
/// can actually open.
async fn databases(connection: &PgConnection) -> DbResult<Vec<Arc<str>>> {
    let rows = connection
        .client()
        .query(
            "select datname::text
             from pg_catalog.pg_database
             where datallowconn and not datistemplate
             order by 1",
            &[],
        )
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;
    Ok(rows.iter().map(|row| text(row, 0)).collect())
}

async fn load_schemas(client: &tokio_postgres::Client) -> DbResult<Vec<Schema>> {
    let sql = format!(
        "select n.nspname::text,
                pg_catalog.pg_get_userbyid(n.nspowner)::text
         from pg_catalog.pg_namespace n
         where {HIDDEN}
         order by 1"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;
    Ok(rows
        .iter()
        .map(|row| Schema {
            name: text(row, 0),
            owner: text(row, 1),
            is_system: false,
            relations: Vec::new(),
            routines: Vec::new(),
        })
        .collect())
}

/// Every relation in the database, in schema-then-name order. The caller
/// places each one by the schema name inside its `reference`.
async fn load_relations(client: &tokio_postgres::Client) -> DbResult<Vec<Relation>> {
    let sql = format!(
        "select n.nspname::text,
                c.relname::text,
                c.relkind::text,
                -- reltuples is -1 on a table that has never been analysed,
                -- which is a fact worth keeping rather than rounding to zero.
                c.reltuples::int8,
                -- `coalesce`, because the size function reads the relation
                -- rather than the catalog row: a table dropped between the
                -- scan and the call returns null, and a refresh that raced a
                -- migration should report a size of zero, not fail.
                coalesce(
                    case when c.relkind in ('r', 'm', 'p', 'f')
                         then pg_catalog.pg_total_relation_size(c.oid)
                         else 0 end,
                    0)::int8,
                pg_catalog.obj_description(c.oid, 'pg_class')::text,
                -- Only for the two kinds that have one. `pg_get_viewdef` on a
                -- table raises rather than returning null, so the `case` is
                -- what keeps a database with one view from failing the whole
                -- scan.
                case when c.relkind in ('v', 'm')
                     then pg_catalog.pg_get_viewdef(c.oid, true) end::text
         from pg_catalog.pg_class c
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         where c.relkind = any('{{r,v,m,f,p}}') and {HIDDEN}
         order by 1, 2"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(rows
        .iter()
        .map(|row| Relation {
            reference: RelationRef::new(text(row, 0), text(row, 1)),
            kind: relation_kind(&row.get::<_, String>(2)),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            estimated_rows: row.get::<_, Option<i64>>(3).unwrap_or(0),
            size_bytes: row.get::<_, Option<i64>>(4).unwrap_or(0),
            checks: Vec::new(),
            triggers: Vec::new(),
            definition: row.get::<_, Option<String>>(6).map(Into::into),
            comment: row.get::<_, Option<String>>(5).map(Into::into),
            detail_loaded: false,
        })
        .collect())
}

fn relation_kind(relkind: &str) -> RelationKind {
    match relkind {
        "v" => RelationKind::View,
        "m" => RelationKind::MaterializedView,
        "f" => RelationKind::Foreign,
        "p" => RelationKind::Partitioned,
        _ => RelationKind::Table,
    }
}

async fn load_columns(client: &tokio_postgres::Client) -> DbResult<Vec<(RelationRef, ColumnDef)>> {
    let sql = format!(
        "select n.nspname::text,
                c.relname::text,
                a.attname::text,
                a.attnum,
                pg_catalog.format_type(a.atttypid, a.atttypmod)::text,
                a.atttypid::int8,
                t.typbasetype::int8,
                t.typtype::text,
                t.typcategory::text,
                not a.attnotnull,
                pg_catalog.pg_get_expr(d.adbin, d.adrelid)::text,
                a.attidentity::text,
                a.attgenerated <> ''::\"char\",
                pg_catalog.col_description(c.oid, a.attnum)::text
         from pg_catalog.pg_attribute a
         join pg_catalog.pg_class c on c.oid = a.attrelid
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         join pg_catalog.pg_type t on t.oid = a.atttypid
         left join pg_catalog.pg_attrdef d
                on d.adrelid = a.attrelid and d.adnum = a.attnum
         where a.attnum > 0 and not a.attisdropped
           and c.relkind = any('{{r,v,m,f,p}}') and {HIDDEN}
         order by 1, 2, 4"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(rows
        .iter()
        .map(|row| {
            let reference = RelationRef::new(text(row, 0), text(row, 1));
            let column = ColumnDef {
                name: text(row, 2),
                position: row.get::<_, i16>(3),
                type_name: text(row, 4),
                kind: types::kind_for_catalog(
                    row.get::<_, i64>(5) as u32,
                    row.get::<_, i64>(6) as u32,
                    &row.get::<_, String>(7),
                    &row.get::<_, String>(8),
                ),
                nullable: row.get::<_, bool>(9),
                default: row.get::<_, Option<String>>(10).map(Into::into),
                identity: db::IdentityKind::from_pg(
                    row.get::<_, String>(11).bytes().next().unwrap_or(b' '),
                ),
                is_generated: row.get::<_, bool>(12),
                comment: row.get::<_, Option<String>>(13).map(Into::into),
            };
            (reference, column)
        })
        .collect())
}

async fn load_indexes(client: &tokio_postgres::Client) -> DbResult<Vec<(RelationRef, IndexDef)>> {
    // The column list comes from `pg_get_indexdef` per position rather than
    // from `pg_attribute`, because an index column can be an expression —
    // `lower(email)` — and there is no attribute to look up for one.
    let sql = format!(
        "select n.nspname::text,
                c.relname::text,
                ic.relname::text,
                i.indisunique,
                i.indisprimary,
                am.amname::text,
                pg_catalog.pg_get_expr(i.indpred, i.indrelid)::text,
                (select array_agg(pg_catalog.pg_get_indexdef(i.indexrelid, k + 1, true)
                                  order by k)
                   from generate_series(0, i.indnkeyatts - 1) as k)::text[]
         from pg_catalog.pg_index i
         join pg_catalog.pg_class c on c.oid = i.indrelid
         join pg_catalog.pg_class ic on ic.oid = i.indexrelid
         join pg_catalog.pg_am am on am.oid = ic.relam
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         where i.indisvalid and {HIDDEN}
         order by 1, 2, 3"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(rows
        .iter()
        .map(|row| {
            let reference = RelationRef::new(text(row, 0), text(row, 1));
            let index = IndexDef {
                name: text(row, 2),
                // Nullable elements, not just a nullable array: the definition
                // function reads the index rather than the catalog row, so one
                // dropped between the scan and the call comes back as a null
                // *inside* the array. A column that no longer exists is left
                // out — the snapshot is already stale, and a refresh is the
                // only fix for that.
                columns: row
                    .get::<_, Option<Vec<Option<String>>>>(7)
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                    .map(Into::into)
                    .collect(),
                is_unique: row.get::<_, bool>(3),
                is_primary: row.get::<_, bool>(4),
                method: text(row, 5),
                predicate: row.get::<_, Option<String>>(6).map(Into::into),
            };
            (reference, index)
        })
        .collect())
}

async fn load_foreign_keys(
    client: &tokio_postgres::Client,
) -> DbResult<Vec<(RelationRef, ForeignKey)>> {
    let sql = format!(
        "select n.nspname::text,
                c.relname::text,
                con.conname::text,
                (select array_agg(a.attname::text order by k.ord)
                   from unnest(con.conkey) with ordinality as k(attnum, ord)
                   join pg_catalog.pg_attribute a
                     on a.attrelid = con.conrelid and a.attnum = k.attnum)::text[],
                fn.nspname::text,
                fc.relname::text,
                (select array_agg(a.attname::text order by k.ord)
                   from unnest(con.confkey) with ordinality as k(attnum, ord)
                   join pg_catalog.pg_attribute a
                     on a.attrelid = con.confrelid and a.attnum = k.attnum)::text[],
                con.confdeltype::text,
                con.confupdtype::text
         from pg_catalog.pg_constraint con
         join pg_catalog.pg_class c on c.oid = con.conrelid
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         join pg_catalog.pg_class fc on fc.oid = con.confrelid
         join pg_catalog.pg_namespace fn on fn.oid = fc.relnamespace
         where con.contype = 'f' and {HIDDEN}
         order by 1, 2, 3"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(rows
        .iter()
        .map(|row| {
            let reference = RelationRef::new(text(row, 0), text(row, 1));
            let fk = ForeignKey {
                name: text(row, 2),
                columns: strings(row, 3),
                target: RelationRef::new(text(row, 4), text(row, 5)),
                target_columns: strings(row, 6),
                on_delete: action(row, 7),
                on_update: action(row, 8),
            };
            (reference, fk)
        })
        .collect())
}

/// `CHECK` constraints, minus the ones Postgres invents.
///
/// A `NOT NULL` on a domain and an inherited check both show up in
/// `pg_constraint`; `conislocal` keeps the list to the ones actually written on
/// this table, which is what someone reading the DDL expects to see.
async fn load_checks(
    client: &tokio_postgres::Client,
) -> DbResult<Vec<(RelationRef, CheckConstraint)>> {
    let sql = format!(
        "select n.nspname::text,
                c.relname::text,
                con.conname::text,
                pg_catalog.pg_get_constraintdef(con.oid, true)::text
         from pg_catalog.pg_constraint con
         join pg_catalog.pg_class c on c.oid = con.conrelid
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         where con.contype = 'c' and con.conislocal and {HIDDEN}
         order by 1, 2, 3"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    // `filter_map`, for the same reason the index columns are: the constraint
    // could have been dropped since the scan, and `pg_get_constraintdef` says
    // so by returning null rather than by failing.
    Ok(rows
        .iter()
        .filter_map(|row| {
            let reference = RelationRef::new(text(row, 0), text(row, 1));
            let check = CheckConstraint {
                name: text(row, 2),
                definition: row.get::<_, Option<String>>(3)?.into(),
            };
            Some((reference, check))
        })
        .collect())
}

/// Triggers, excluding the internal ones that implement foreign keys.
///
/// `tgisinternal` is the whole filter: every FK on a table creates two hidden
/// triggers, and a list that showed them would say a two-key table has four
/// triggers nobody wrote.
async fn load_triggers(
    client: &tokio_postgres::Client,
) -> DbResult<Vec<(RelationRef, TriggerDef)>> {
    let sql = format!(
        "select n.nspname::text,
                c.relname::text,
                t.tgname::text,
                -- Not the `pretty` form: pretty-printing drops the schema
                -- from `ON <table>` whenever the table is on the search path,
                -- and a `CREATE TRIGGER` in a DDL block that qualifies
                -- everything else would then be the one line that lands
                -- somewhere else when it is run.
                pg_catalog.pg_get_triggerdef(t.oid)::text,
                pg_catalog.quote_ident(fn_.nspname) || '.'
                  || pg_catalog.quote_ident(p.proname)::text,
                t.tgenabled <> 'D'
         from pg_catalog.pg_trigger t
         join pg_catalog.pg_class c on c.oid = t.tgrelid
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         join pg_catalog.pg_proc p on p.oid = t.tgfoid
         join pg_catalog.pg_namespace fn_ on fn_.oid = p.pronamespace
         where not t.tgisinternal and {HIDDEN}
         order by 1, 2, 3"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            let reference = RelationRef::new(text(row, 0), text(row, 1));
            let definition: Arc<str> = row.get::<_, Option<String>>(3)?.into();
            let trigger = TriggerDef {
                name: text(row, 2),
                timing: trigger_timing(&definition).into(),
                definition,
                function: text(row, 4),
                enabled: row.get::<_, bool>(5),
            };
            Some((reference, trigger))
        })
        .collect())
}

/// The `BEFORE INSERT OR UPDATE` part of a `CREATE TRIGGER`.
///
/// Read out of the printed definition rather than decoded from `tgtype`'s bit
/// field: the server already renders those bits, and rendering them a second
/// time here is two implementations of the same table that can disagree.
fn trigger_timing(definition: &str) -> String {
    let Some(rest) = definition.split_once(" ON ").map(|(head, _)| head) else {
        return String::new();
    };
    let mut words = rest.split_whitespace().skip_while(|w| *w != "TRIGGER");
    words.next();
    // `CREATE TRIGGER <name> BEFORE INSERT OR UPDATE OF col ON …` — the name is
    // one word (quoted if it needs to be) and everything after it is timing.
    words.next();
    words.collect::<Vec<_>>().join(" ")
}

async fn load_routines(client: &tokio_postgres::Client) -> DbResult<Vec<Routine>> {
    let sql = format!(
        "select n.nspname::text,
                p.proname::text,
                pg_catalog.pg_get_function_arguments(p.oid)::text,
                pg_catalog.pg_get_function_result(p.oid)::text,
                p.prokind = 'p'
         from pg_catalog.pg_proc p
         join pg_catalog.pg_namespace n on n.oid = p.pronamespace
         where {HIDDEN} and p.prokind in ('f', 'p')
         order by 1, 2"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(rows
        .iter()
        .map(|row| Routine {
            schema: text(row, 0),
            name: text(row, 1),
            arguments: text(row, 2),
            returns: text(row, 3),
            is_procedure: row.get::<_, bool>(4),
        })
        .collect())
}

// ---- row helpers ---------------------------------------------------------

fn text(row: &Row, index: usize) -> Arc<str> {
    row.get::<_, String>(index).into()
}

fn strings(row: &Row, index: usize) -> Vec<Arc<str>> {
    row.get::<_, Option<Vec<String>>>(index)
        .unwrap_or_default()
        .into_iter()
        .map(Into::into)
        .collect()
}

fn action(row: &Row, index: usize) -> RefAction {
    RefAction::from_pg(row.get::<_, String>(index).bytes().next().unwrap_or(b'a'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_schemas_are_excluded_by_pattern_not_by_list() {
        // pg_toast_12345 and pg_temp_3 are per-object and per-backend, so they
        // can only be matched by prefix.
        assert!(HIDDEN.contains("pg\\_toast%"));
        assert!(HIDDEN.contains("pg\\_temp%"));
    }

    #[test]
    fn relkinds_map_to_the_kinds_the_tree_draws() {
        assert_eq!(relation_kind("r"), RelationKind::Table);
        assert_eq!(relation_kind("v"), RelationKind::View);
        assert_eq!(relation_kind("m"), RelationKind::MaterializedView);
        assert_eq!(relation_kind("f"), RelationKind::Foreign);
        assert_eq!(relation_kind("p"), RelationKind::Partitioned);
        // Anything the server invents later reads as a table rather than
        // vanishing from the tree.
        assert_eq!(relation_kind("z"), RelationKind::Table);
    }

    #[test]
    fn trigger_timing_is_what_sits_between_the_name_and_the_table() {
        assert_eq!(
            trigger_timing(
                "CREATE TRIGGER touch_updated_at BEFORE UPDATE ON public.users \
                 FOR EACH ROW EXECUTE FUNCTION touch()"
            ),
            "BEFORE UPDATE"
        );
        assert_eq!(
            trigger_timing(
                "CREATE CONSTRAINT TRIGGER audit AFTER INSERT OR DELETE ON public.orders \
                 FOR EACH ROW EXECUTE FUNCTION audit()"
            ),
            "AFTER INSERT OR DELETE"
        );
        // Nothing recognisable is a blank column, not a panic.
        assert_eq!(trigger_timing("nonsense"), "");
    }
}
