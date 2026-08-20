//! Who may do what.
//!
//! Two questions that look like one and are not. "Which roles exist" is a
//! property of the server and is asked once per connection; "who may read this
//! table" is a property of one object and is asked when somebody opens it. The
//! first is a list to browse, the second is the answer to a question the grid
//! asks constantly and used to answer by guessing — a table opened read-only
//! looked exactly like a table with no primary key, and neither said why.
//!
//! Nothing here is Postgres-shaped on purpose. A role is a name with a set of
//! things it is allowed to do and a set of roles it inherits from, which is
//! true of every server that has the concept, and the engine-specific spelling
//! stays in the driver.

use std::fmt;
use std::sync::Arc;

/// One role on the server.
///
/// Users and groups are the same thing wearing different clothes — Postgres
/// stopped distinguishing them in 8.1 and the only difference left is whether
/// the role can log in — so there is one type and [`Role::is_login`] rather
/// than two lists that would have to be kept in step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    pub name: Arc<str>,
    pub superuser: bool,
    pub can_login: bool,
    pub create_db: bool,
    pub create_role: bool,
    /// Whether membership grants this role's privileges automatically. A role
    /// with `noinherit` has to `set role` to use what it is a member of, which
    /// changes the answer to "why can I not read this" completely.
    pub inherit: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    /// `-1` for no limit, which is what the server stores and what the UI is
    /// expected to read as "unlimited" rather than as minus one.
    pub connection_limit: i32,
    /// When the password stops working. Not when the role stops existing —
    /// an expired role can still be granted things and still owns its tables.
    pub valid_until: Option<Arc<str>>,
    /// Roles this one is a member of, directly. Not transitively: a chain that
    /// has been flattened cannot be drawn as the chain it was, and the reason
    /// anybody opens this list is to find out where a privilege came from.
    pub member_of: Vec<Arc<str>>,
    pub comment: Option<Arc<str>>,
}

impl Role {
    /// Whether this is something a person logs in as.
    pub fn is_login(&self) -> bool {
        self.can_login
    }

    /// The attributes worth showing, in the order they are worth showing them.
    ///
    /// Only the ones that are on: a role's interesting facts are what it can
    /// do that an ordinary role cannot, and a row of eight "no"s says nothing
    /// while hiding the one "yes".
    pub fn attributes(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.superuser {
            out.push("Superuser");
        }
        if self.create_db {
            out.push("Create DB");
        }
        if self.create_role {
            out.push("Create role");
        }
        if self.replication {
            out.push("Replication");
        }
        if self.bypass_rls {
            out.push("Bypass RLS");
        }
        if !self.inherit {
            out.push("No inherit");
        }
        if !self.can_login {
            out.push("Group");
        }
        out
    }

    /// One line for the sidebar's right-hand column.
    pub fn summary(&self) -> String {
        match self.attributes().as_slice() {
            [] => match self.member_of.len() {
                0 => String::new(),
                n => format!("member of {n}"),
            },
            attributes => attributes.join(" · "),
        }
    }
}

/// Every role on the server, and which one this connection is.
#[derive(Clone, Debug, Default)]
pub struct RoleSet {
    /// Sorted: login roles first, then groups, each alphabetically. Sorted by
    /// the driver rather than by the view, because the same order should hold
    /// wherever the list is shown.
    pub roles: Vec<Role>,
    /// The role the connection authenticated as. Shown, and used to decide
    /// which row is *you* — the single most useful fact in the list.
    pub current: Arc<str>,
}

impl RoleSet {
    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.iter().find(|role| &*role.name == name)
    }

    pub fn logins(&self) -> impl Iterator<Item = &Role> {
        self.roles.iter().filter(|role| role.can_login)
    }

    pub fn groups(&self) -> impl Iterator<Item = &Role> {
        self.roles.iter().filter(|role| !role.can_login)
    }
}

/// One thing that can be done to a relation.
///
/// The SQL-standard set plus `Maintain`, which Postgres 17 added and older
/// servers simply never report. An engine with a privilege not on this list
/// reports it as [`Privilege::Other`] rather than dropping it: a privileges
/// view that quietly omits a grant is worse than one that shows a name it does
/// not have an icon for.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
    Maintain,
    Other(Arc<str>),
}

impl Privilege {
    /// The ones a table has, in the order a matrix should column them: read,
    /// then the three writes, then the rest.
    pub const TABLE: [Privilege; 8] = [
        Privilege::Select,
        Privilege::Insert,
        Privilege::Update,
        Privilege::Delete,
        Privilege::Truncate,
        Privilege::References,
        Privilege::Trigger,
        Privilege::Maintain,
    ];

    /// The SQL keyword, which is also what every server calls it back.
    pub fn keyword(&self) -> &str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::References => "REFERENCES",
            Self::Trigger => "TRIGGER",
            Self::Maintain => "MAINTAIN",
            Self::Other(name) => name,
        }
    }

    pub fn parse(keyword: &str) -> Self {
        match keyword.trim().to_ascii_uppercase().as_str() {
            "SELECT" => Self::Select,
            "INSERT" => Self::Insert,
            "UPDATE" => Self::Update,
            "DELETE" => Self::Delete,
            "TRUNCATE" => Self::Truncate,
            "REFERENCES" => Self::References,
            "TRIGGER" => Self::Trigger,
            "MAINTAIN" => Self::Maintain,
            other => Self::Other(other.into()),
        }
    }
}

impl fmt::Display for Privilege {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

/// One grant: somebody may do something, and may or may not pass it on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The role the privilege is held by. [`PUBLIC`] means everyone, and is a
    /// string rather than a variant because that is how every server spells it
    /// and inventing a second spelling only creates a place to forget one.
    pub grantee: Arc<str>,
    pub privilege: Privilege,
    /// `with grant option`.
    pub grantable: bool,
    /// The column this is limited to, when it is. Column privileges are rare
    /// and are exactly the thing that makes an otherwise readable table refuse
    /// half its columns, so they are worth carrying rather than folding into
    /// the table row.
    pub column: Option<Arc<str>>,
}

/// The grantee that means everyone.
pub const PUBLIC: &str = "PUBLIC";

/// Who may do what to one relation.
#[derive(Clone, Debug, Default)]
pub struct Grants {
    pub owner: Arc<str>,
    /// Sorted by grantee and then by privilege, so the matrix is stable
    /// between refreshes.
    pub grants: Vec<Grant>,
    /// What the connected role may actually do here, inheritance and `PUBLIC`
    /// already taken into account. Asked of the server rather than worked out
    /// from `grants`, because working it out means re-implementing role
    /// inheritance and getting it subtly wrong.
    pub mine: Vec<Privilege>,
}

impl Grants {
    pub fn may(&self, privilege: &Privilege) -> bool {
        self.mine.contains(privilege)
    }

    /// Whether the connected role can change rows here. What decides between a
    /// grid that edits and a grid that explains why it does not.
    pub fn may_write(&self) -> bool {
        self.may(&Privilege::Update) || self.may(&Privilege::Delete)
    }

    /// The grantees in the order the matrix should row them: the owner first,
    /// then `PUBLIC`, then everybody else alphabetically.
    pub fn grantees(&self) -> Vec<Arc<str>> {
        let mut names: Vec<Arc<str>> = Vec::new();
        for grant in &self.grants {
            if !names.contains(&grant.grantee) {
                names.push(grant.grantee.clone());
            }
        }
        names.sort_by_key(|name| {
            (
                match &**name {
                    owner if owner == &*self.owner => 0,
                    PUBLIC => 1,
                    _ => 2,
                },
                name.to_lowercase(),
            )
        });
        names
    }

    /// What one grantee holds on the relation as a whole, ignoring the
    /// column-scoped grants, which the matrix shows separately.
    pub fn of(&self, grantee: &str) -> Vec<&Grant> {
        self.grants
            .iter()
            .filter(|grant| &*grant.grantee == grantee && grant.column.is_none())
            .collect()
    }

    /// The column-scoped grants, grouped by the column they name.
    pub fn columns(&self) -> Vec<(Arc<str>, Vec<&Grant>)> {
        let mut out: Vec<(Arc<str>, Vec<&Grant>)> = Vec::new();
        for grant in self.grants.iter().filter(|g| g.column.is_some()) {
            let column = grant.column.clone().expect("filtered on some");
            match out.iter_mut().find(|(name, _)| *name == column) {
                Some((_, grants)) => grants.push(grant),
                None => out.push((column, vec![grant])),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(name: &str, login: bool) -> Role {
        Role {
            name: name.into(),
            superuser: false,
            can_login: login,
            create_db: false,
            create_role: false,
            inherit: true,
            replication: false,
            bypass_rls: false,
            connection_limit: -1,
            valid_until: None,
            member_of: Vec::new(),
            comment: None,
        }
    }

    fn grant(grantee: &str, privilege: Privilege) -> Grant {
        Grant {
            grantee: grantee.into(),
            privilege,
            grantable: false,
            column: None,
        }
    }

    #[test]
    fn an_ordinary_role_has_nothing_worth_saying_about_it() {
        assert!(role("app", true).attributes().is_empty());
    }

    #[test]
    fn a_role_that_cannot_log_in_is_a_group() {
        assert_eq!(role("readers", false).attributes(), ["Group"]);
    }

    #[test]
    fn only_the_attributes_that_are_on_are_listed() {
        let mut admin = role("admin", true);
        admin.superuser = true;
        admin.create_db = true;
        assert_eq!(admin.attributes(), ["Superuser", "Create DB"]);
    }

    #[test]
    fn the_owner_leads_the_matrix_and_public_follows_it() {
        let grants = Grants {
            owner: "app".into(),
            grants: vec![
                grant("zoe", Privilege::Select),
                grant(PUBLIC, Privilege::Select),
                grant("app", Privilege::Select),
                grant("alice", Privilege::Select),
            ],
            mine: Vec::new(),
        };
        let grantees = grants.grantees();
        let order: Vec<&str> = grantees.iter().map(|n| &**n).collect();
        assert_eq!(order, ["app", PUBLIC, "alice", "zoe"]);
    }

    #[test]
    fn a_column_grant_is_not_counted_as_a_table_grant() {
        let mut column = grant("alice", Privilege::Update);
        column.column = Some("email".into());
        let grants = Grants {
            owner: "app".into(),
            grants: vec![grant("alice", Privilege::Select), column],
            mine: Vec::new(),
        };
        assert_eq!(grants.of("alice").len(), 1);
        assert_eq!(grants.columns().len(), 1);
    }

    #[test]
    fn a_privilege_the_engine_invented_survives_the_round_trip() {
        let odd = Privilege::parse("vacuum");
        assert_eq!(odd.keyword(), "VACUUM");
        assert!(!Privilege::TABLE.contains(&odd));
    }

    #[test]
    fn writing_means_update_or_delete_and_not_merely_reading() {
        let mut grants = Grants {
            owner: "app".into(),
            grants: Vec::new(),
            mine: vec![Privilege::Select],
        };
        assert!(!grants.may_write());
        grants.mine.push(Privilege::Delete);
        assert!(grants.may_write());
    }
}
