//! Reading `pg_authid` and the ACLs.
//!
//! Two queries for the role list and three for one table's privileges, all of
//! them against `pg_catalog` rather than `information_schema` for the same
//! reason as [`crate::introspect`]: the standard views apply a permission
//! filter of their own, so a role that cannot see a grant is shown a table
//! with the grant simply missing rather than a table it is not allowed to
//! read. A privileges view that quietly omits privileges is worse than none.

use std::sync::Arc;

use db::{DbResult, ErrorClass, Grant, Grants, Privilege, RelationRef, Role, RoleSet};
use tokio_postgres::Row;

use crate::client::{classify, PgConnection};

/// The roles Postgres ships with itself — `pg_read_all_data`, `pg_monitor` and
/// the rest. Left out of the list because nobody administers them and there
/// are a dozen, but deliberately still visible in a role's `member_of`: being
/// a member of `pg_read_all_data` is exactly the sort of thing somebody opens
/// this view to discover.
const PREDEFINED: &str = "r.rolname not like 'pg\\_%'";

pub async fn roles(connection: &PgConnection) -> DbResult<RoleSet> {
    let client = connection.client();
    let current = connection.scalar("select current_user").await?;

    // `pg_roles` rather than `pg_authid`: the two differ only in that
    // `rolpassword` is redacted, and `pg_authid` is superuser-only, so reading
    // it would make this feature disappear for exactly the ordinary users who
    // most want to know what they are allowed to do.
    let sql = format!(
        "select r.rolname::text,
                r.rolsuper, r.rolcanlogin, r.rolcreatedb, r.rolcreaterole,
                r.rolinherit, r.rolreplication, r.rolbypassrls,
                r.rolconnlimit,
                to_char(r.rolvaliduntil, 'YYYY-MM-DD HH24:MI')::text,
                pg_catalog.shobj_description(r.oid, 'pg_authid')::text,
                coalesce(
                    array_agg(g.rolname::text order by g.rolname)
                        filter (where g.rolname is not null),
                    '{{}}'
                )
         from pg_catalog.pg_roles r
         left join pg_catalog.pg_auth_members m on m.member = r.oid
         left join pg_catalog.pg_roles g on g.oid = m.roleid
         where {PREDEFINED}
         group by r.oid, r.rolname, r.rolsuper, r.rolcanlogin, r.rolcreatedb,
                  r.rolcreaterole, r.rolinherit, r.rolreplication, r.rolbypassrls,
                  r.rolconnlimit, r.rolvaliduntil
         order by r.rolcanlogin desc, lower(r.rolname)"
    );
    let rows = client
        .query(&sql, &[])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    Ok(RoleSet {
        roles: rows.iter().map(role).collect(),
        current: current.into(),
    })
}

fn role(row: &Row) -> Role {
    Role {
        name: text(row, 0),
        superuser: row.get(1),
        can_login: row.get(2),
        create_db: row.get(3),
        create_role: row.get(4),
        inherit: row.get(5),
        replication: row.get(6),
        bypass_rls: row.get(7),
        connection_limit: row.get(8),
        valid_until: maybe(row, 9),
        comment: maybe(row, 10),
        member_of: row
            .get::<_, Option<Vec<String>>>(11)
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect(),
    }
}

/// Who may do what to one relation.
pub async fn grants(connection: &PgConnection, relation: &RelationRef) -> DbResult<Grants> {
    let client = connection.client();
    let schema = relation.schema.to_string();
    let name = relation.name.to_string();

    // `aclexplode` turns the packed `aclitem[]` into one row per privilege,
    // which is the shape the matrix wants and the only way to read an ACL
    // without parsing `=arwdDxt/postgres` by hand. Grantee 0 is `PUBLIC`; the
    // server stores it as an OID that belongs to nobody.
    let table = client
        .query(
            "select pg_catalog.pg_get_userbyid(c.relowner)::text,
                    case when acl.grantee = 0
                         then 'PUBLIC'
                         else pg_catalog.pg_get_userbyid(acl.grantee)::text end,
                    acl.privilege_type::text,
                    acl.is_grantable
             from pg_catalog.pg_class c
             join pg_catalog.pg_namespace n on n.oid = c.relnamespace
             left join lateral pg_catalog.aclexplode(c.relacl) acl on true
             where n.nspname = $1 and c.relname = $2",
            &[&schema, &name],
        )
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;

    let Some(first) = table.first() else {
        return Err(db::DbError::new(
            ErrorClass::Server,
            format!("there is no relation called {}", relation.qualified()),
        ));
    };
    let owner: Arc<str> = text(first, 0);

    let mut list: Vec<Grant> = table
        .iter()
        .filter_map(|row| {
            Some(Grant {
                grantee: maybe(row, 1)?,
                privilege: Privilege::parse(&row.get::<_, Option<String>>(2)?),
                grantable: row.get::<_, Option<bool>>(3).unwrap_or(false),
                column: None,
            })
        })
        .collect();

    // A null `relacl` is not "nobody may do anything": it is the default, which
    // is the owner holding everything and nobody else holding anything. The
    // server writes it that way to save space and every client has to know.
    // Reconstructed here rather than with `acldefault()` in SQL because that
    // function is not callable on servers old enough to still be in use.
    if list.is_empty() {
        list = Privilege::TABLE
            .iter()
            .map(|privilege| Grant {
                grantee: owner.clone(),
                privilege: privilege.clone(),
                grantable: true,
                column: None,
            })
            .collect();
    }

    let columns = client
        .query(
            "select a.attname::text,
                    case when acl.grantee = 0
                         then 'PUBLIC'
                         else pg_catalog.pg_get_userbyid(acl.grantee)::text end,
                    acl.privilege_type::text,
                    acl.is_grantable
             from pg_catalog.pg_attribute a
             join pg_catalog.pg_class c on c.oid = a.attrelid
             join pg_catalog.pg_namespace n on n.oid = c.relnamespace
             cross join lateral pg_catalog.aclexplode(a.attacl) acl
             where n.nspname = $1 and c.relname = $2 and a.attacl is not null
             order by a.attnum",
            &[&schema, &name],
        )
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;
    list.extend(columns.iter().map(|row| Grant {
        grantee: text(row, 1),
        privilege: Privilege::parse(&row.get::<_, String>(2)),
        grantable: row.get(3),
        column: Some(text(row, 0)),
    }));

    list.sort_by(|a, b| {
        a.column
            .cmp(&b.column)
            .then_with(|| a.grantee.cmp(&b.grantee))
            .then_with(|| a.privilege.cmp(&b.privilege))
    });

    Ok(Grants {
        owner,
        grants: list,
        mine: mine(connection, relation).await?,
    })
}

/// What the connected role may actually do here.
///
/// `has_table_privilege` and not a walk of the ACL: the answer depends on
/// every role this one inherits from, on `PUBLIC`, and on whether the role is
/// a superuser, and re-deriving that in the client is how a client ends up
/// confidently telling somebody they cannot do something they can.
async fn mine(connection: &PgConnection, relation: &RelationRef) -> DbResult<Vec<Privilege>> {
    let qualified = format!(
        "{}.{}",
        db::schema::quote_ident(&relation.schema),
        db::schema::quote_ident(&relation.name)
    );
    let rows = connection
        .client()
        .query(MINE, &[&qualified])
        .await
        .map_err(|e| classify(e, ErrorClass::Server))?;
    Ok(rows
        .iter()
        .map(|row| Privilege::parse(&row.get::<_, String>(0)))
        .collect())
}

/// `MAINTAIN` arrived in Postgres 17, and asking an older server about a
/// privilege it has never heard of is an error rather than a `false` — so the
/// list is chosen by the server, inside the statement, rather than by a
/// version check the client would have to make first.
const MINE: &str = "select p from unnest(
         case when current_setting('server_version_num')::int >= 170000
              then array['SELECT','INSERT','UPDATE','DELETE','TRUNCATE',
                         'REFERENCES','TRIGGER','MAINTAIN']
              else array['SELECT','INSERT','UPDATE','DELETE','TRUNCATE',
                         'REFERENCES','TRIGGER'] end
     ) p
     where pg_catalog.has_table_privilege($1, p)";

fn text(row: &Row, index: usize) -> Arc<str> {
    row.get::<_, String>(index).into()
}

fn maybe(row: &Row, index: usize) -> Option<Arc<str>> {
    row.get::<_, Option<String>>(index)
        .filter(|value| !value.is_empty())
        .map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roles_postgres_owns_are_hidden_by_prefix_and_not_by_list() {
        // There are a dozen of them and the set grows every release, so naming
        // them would mean shipping a new build to hide the next one.
        assert!(PREDEFINED.contains("pg\\_%"));
    }

    #[test]
    fn every_table_privilege_is_asked_about() {
        // The statement names them as strings, so a privilege added to
        // `Privilege::TABLE` and forgotten here would silently never be true.
        for privilege in Privilege::TABLE {
            assert!(
                MINE.contains(privilege.keyword()),
                "{} is not asked for",
                privilege.keyword()
            );
        }
    }

    #[test]
    fn maintain_is_asked_for_only_where_it_exists() {
        assert!(MINE.contains("server_version_num"));
        assert_eq!(MINE.matches("MAINTAIN").count(), 1);
    }
}
