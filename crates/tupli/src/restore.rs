//! What the window was showing when you last quit.
//!
//! Layout is where the panels were; settings are what you chose. This is the
//! third thing an app has to remember: which tabs were open, what was typed in
//! them, and which connection they were pointed at. Losing a half-written
//! statement to a restart is the one that actually costs somebody something.
//!
//! Stored under its own key, in the same shape as [`crate::layout`] — one JSON
//! value, every field optional on the way in — so a file written by an older
//! build loses nothing but the fields it never had.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pane::{Layout, Member, PaneGroup, PaneId};
use crate::workspace::{CenterKind, CenterTab};

pub const KEY: &str = "session";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The connection the window was looking at, by id. A connection that has
    /// since been deleted simply does not reopen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<Uuid>,
    /// Which database on that connection. A Postgres session belongs to one
    /// database for its whole life, so the connection's own default is only
    /// where you *started*; switching is a reconnect, and without this the
    /// window comes back on the wrong one and every restored table tab asks
    /// for a relation that is not there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// The active pane's tabs. Written even when the window was split, so a
    /// build that predates splits still reopens the editor you were in rather
    /// than nothing at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<Tab>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
    /// Every pane, in the order the ids are handed back out at launch. Empty
    /// in a file written before splits existed, which is what `tabs` is for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PaneState>,
    /// How those panes were arranged. `None` means one pane, or a file from
    /// before the tree existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<Split>,
    /// Which pane was active, as an index into `panes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pane: Option<usize>,
}

/// One pane's worth of the window: what was open in it and which of those was
/// showing. The result set is deliberately not here — rows are the server's,
/// and a restart is exactly the moment to go and ask again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<Tab>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
}

/// The pane tree as it goes to disk. Panes are referred to by their index in
/// [`State::panes`] rather than by their live id, because ids are per-window
/// and are not reused — writing them down would be recording a number that
/// means nothing on the next launch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Pane(usize),
    Group {
        /// `"rows"` or `"columns"`, spelled out for the same reason tab kinds
        /// are.
        layout: String,
        members: Vec<Split>,
        flexes: Vec<f32>,
    },
}

/// What the workspace hands over about one pane. A borrow rather than a copy:
/// this is built on the way to quitting, and the tabs are already sitting
/// there.
pub struct PaneSnapshot<'a> {
    pub id: PaneId,
    pub tabs: &'a [CenterTab],
    pub active: usize,
    /// Where each tab was connected, in the same order as `tabs`. Handed over
    /// separately because a tab holds a live [`Session`] entity and reading one
    /// needs the app context, which this module deliberately does not have —
    /// the workspace, which does, resolves them on the way in.
    ///
    /// [`Session`]: crate::session::Session
    pub sources: Vec<Option<(Uuid, String)>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    /// `"query"`, `"table"` or `"structure"`. A word rather than a number so
    /// the file stays readable and a fourth kind needs no migration.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sql: String,
    /// The relation a browsing tab was on, split into its two halves so the
    /// file does not have to be re-parsed for quoting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_query: Option<Uuid>,
    /// The filter above the rows: the chips, or the clause someone typed. Kept
    /// because it is the part of a browse that took thought — the table is one
    /// click away, the `where` that made it interesting is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<crate::filter::Filter>,
    /// The connection this tab was on, which is not necessarily the window's:
    /// two tabs open on two databases is the whole point of a tab owning its
    /// session, and a restore that put them both back on the last one you
    /// clicked would be undoing that every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Pinned. Worth a launch's memory: a pin is somebody saying this tab is
    /// the one they keep, and forgetting it every morning would make the pin a
    /// gesture for the next ten minutes rather than for the work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

impl State {
    pub fn load(store: Option<&store::Store>) -> Self {
        let Some(store) = store else {
            return Self::default();
        };
        match store.setting(KEY) {
            Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_else(|error| {
                log::warn!("ignoring an unreadable saved session: {error}");
                Self::default()
            }),
            Ok(None) => Self::default(),
            Err(error) => {
                log::warn!("could not read the saved session: {error:#}");
                Self::default()
            }
        }
    }

    pub fn save(&self, store: Option<&store::Store>) {
        let Some(store) = store else { return };
        match serde_json::to_string(self) {
            Ok(text) => {
                if let Err(error) = store.set_setting(KEY, &text) {
                    log::warn!("could not save the session: {error:#}");
                }
            }
            Err(error) => log::warn!("could not encode the session: {error}"),
        }
    }

    /// The tabs to open, or nothing if this is a first launch. `None` rather
    /// than an empty list, because a window with no tab is not a state the
    /// workspace has: the caller falls back to its own defaults.
    pub fn tabs(&self) -> Option<Vec<CenterTab>> {
        if self.tabs.is_empty() {
            return None;
        }
        Some(self.tabs.iter().map(Tab::to_center_tab).collect())
    }

    /// Which of those tabs was showing, clamped — a file that names tab 7 of
    /// a list of three is wrong about something, and the first tab is a better
    /// answer than a panic.
    pub fn active(&self) -> usize {
        self.active
            .unwrap_or(0)
            .min(self.tabs.len().saturating_sub(1))
    }

    /// Every pane's tabs, oldest field first: a file written before splits
    /// existed has one pane's worth of tabs in `tabs`, and it should reopen as
    /// one pane rather than as nothing.
    ///
    /// `None` on a first launch, for the same reason [`Self::tabs`] is.
    pub fn panes(&self) -> Option<Vec<(Vec<CenterTab>, usize)>> {
        if !self.panes.is_empty() {
            let panes: Vec<_> = self
                .panes
                .iter()
                .filter(|pane| !pane.tabs.is_empty())
                .map(|pane| {
                    let tabs: Vec<CenterTab> = pane.tabs.iter().map(Tab::to_center_tab).collect();
                    let active = pane.active.unwrap_or(0).min(tabs.len() - 1);
                    (tabs, active)
                })
                .collect();
            if !panes.is_empty() {
                return Some(panes);
            }
        }
        Some(vec![(self.tabs()?, self.active())])
    }

    /// The tree those panes were in, or `None` for one pane and for anything
    /// that does not describe the panes actually being restored. A layout that
    /// names a pane that is not there is not a layout to be repaired: the
    /// panes are the thing someone typed into, and a row of them is a correct
    /// window even if it is not the window they left.
    pub fn tree(&self, count: usize) -> Option<PaneGroup> {
        let split = self.split.as_ref()?;
        let group = match split.to_group() {
            Some(group) => group,
            None => {
                log::warn!("ignoring a saved layout whose root is a single pane");
                return None;
            }
        };
        let mut seen = group.panes();
        let panes = seen.len();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != panes || panes != count || seen.last() != count.checked_sub(1).as_ref() {
            log::warn!("ignoring a saved layout that does not name the {count} panes exactly once");
            return None;
        }
        Some(group)
    }

    /// Which pane was active, as an id — which is its index, because that is
    /// the order the workspace hands ids out in when it restores.
    pub fn active_pane(&self, count: usize) -> PaneId {
        self.active_pane.unwrap_or(0).min(count.saturating_sub(1))
    }

    pub fn from_workspace(
        connection: Option<Uuid>,
        database: Option<String>,
        panes: &[PaneSnapshot<'_>],
        layout: &PaneGroup,
        active: PaneId,
    ) -> Self {
        let index_of = |id: PaneId| panes.iter().position(|pane| pane.id == id);
        let showing = panes.iter().position(|pane| pane.id == active).unwrap_or(0);
        Self {
            connection,
            database,
            // The active pane's tabs, twice: once here where an older build
            // will find them, and once in `panes` with everybody else's.
            tabs: panes
                .get(showing)
                .map(|pane| pane.tabs_to_save())
                .unwrap_or_default(),
            active: panes.get(showing).map(|pane| pane.active),
            panes: panes
                .iter()
                .map(|pane| PaneState {
                    tabs: pane.tabs_to_save(),
                    active: Some(pane.active),
                })
                .collect(),
            // One pane is not a layout. Writing `null` there keeps the file
            // the same as it was before splits for the window most people have.
            split: (panes.len() > 1).then(|| Split::of_group(layout, &index_of)),
            active_pane: Some(showing),
        }
    }
}

impl PaneSnapshot<'_> {
    /// This pane's tabs as they go to disk, each paired with its own source.
    /// A missing entry is a tab nobody has connected — it saves without one and
    /// comes back the same way.
    fn tabs_to_save(&self) -> Vec<Tab> {
        self.tabs
            .iter()
            .enumerate()
            // A key tab is not put back. A key can expire between quitting and
            // launching — that is what a TTL is — and a tab reopening onto
            // nothing would be the app remembering something that stopped
            // being true. A table is still there tomorrow; a key is a guess.
            .filter(|(_, tab)| tab.kind != CenterKind::Key)
            .map(|(ix, tab)| Tab::from_center_tab(tab, self.sources.get(ix).cloned().flatten()))
            .collect()
    }
}

impl Split {
    fn of_group(group: &PaneGroup, index_of: &impl Fn(PaneId) -> Option<usize>) -> Self {
        Split::Group {
            layout: match group.layout {
                Layout::Columns => "columns",
                Layout::Rows => "rows",
            }
            .to_string(),
            members: group
                .members
                .iter()
                .map(|member| match member {
                    // A pane the workspace does not have is not a thing that
                    // happens; `usize::MAX` makes it fail the check on the way
                    // back in rather than silently naming pane zero twice.
                    Member::Pane(id) => Split::Pane(index_of(*id).unwrap_or(usize::MAX)),
                    Member::Group(group) => Split::of_group(group, index_of),
                })
                .collect(),
            flexes: group.flexes.clone(),
        }
    }

    fn to_group(&self) -> Option<PaneGroup> {
        let Split::Group {
            layout,
            members,
            flexes,
        } = self
        else {
            return None;
        };
        if members.is_empty() || members.len() != flexes.len() {
            return None;
        }
        let members: Option<Vec<Member>> = members
            .iter()
            .map(|member| match member {
                Split::Pane(index) => Some(Member::Pane(*index)),
                group => group.to_group().map(Member::Group),
            })
            .collect();
        // Fractions are normalised rather than trusted: they were written by
        // a drag, and a rounding error that leaves them summing to 0.999 should
        // not cost anyone their layout.
        let total: f32 = flexes.iter().sum();
        if !total.is_finite() || total <= 0. {
            return None;
        }
        Some(PaneGroup {
            layout: match layout.as_str() {
                "rows" => Layout::Rows,
                _ => Layout::Columns,
            },
            members: members?,
            flexes: flexes.iter().map(|flex| flex / total).collect(),
        })
    }
}

impl Tab {
    fn from_center_tab(tab: &CenterTab, source: Option<(Uuid, String)>) -> Self {
        let (connection, database) = match source {
            Some((connection, database)) => (Some(connection), Some(database)),
            None => (None, None),
        };
        Self {
            kind: match tab.kind {
                CenterKind::Query => "query",
                CenterKind::Table => "table",
                CenterKind::Structure => "structure",
                // Filtered out before this by `tabs_to_save`; a query tab is
                // the harmless answer if one ever gets here.
                CenterKind::Key => "query",
            }
            .to_string(),
            title: tab.title.to_string(),
            detail: tab.detail.as_ref().map(|d| d.to_string()),
            sql: tab.sql.clone(),
            schema: tab.relation.as_ref().map(|r| r.schema.to_string()),
            relation: tab.relation.as_ref().map(|r| r.name.to_string()),
            saved_query: tab.saved_query,
            // `None` rather than an empty filter, so a tab nobody filtered
            // adds nothing to the file.
            filter: Some(tab.filter.clone()).filter(|f| f != &crate::filter::Filter::default()),
            connection,
            database,
            pinned: tab.pinned,
        }
    }

    fn to_center_tab(&self) -> CenterTab {
        let relation = match (self.schema.as_deref(), self.relation.as_deref()) {
            (Some(schema), Some(name)) => Some(db::RelationRef::new(schema, name)),
            _ => None,
        };
        CenterTab {
            key: None,
            // An unknown word is a tab kind from a build this one is older
            // than. It becomes a query tab, which is the kind that needs
            // nothing but its text to be useful.
            kind: match self.kind.as_str() {
                "table" => CenterKind::Table,
                // A design tab comes back as the table it was designing, and
                // gets its editor when the catalog arrives. One that named no
                // table was a `New Table` nobody had saved: there is nothing
                // to put back, so it comes back as the empty tab it is.
                "structure" if relation.is_some() => CenterKind::Structure,
                _ => CenterKind::Query,
            },
            title: match self.title.is_empty() {
                true => "Untitled".into(),
                false => self.title.clone().into(),
            },
            detail: self.detail.clone().map(Into::into),
            // Nothing is unsaved at launch: the text is right there in the
            // file, so a dot claiming otherwise would be a lie.
            dirty: false,
            pinned: self.pinned,
            relation,
            saved_query: self.saved_query,
            sql: self.sql.clone(),
            filter: self.filter.clone().unwrap_or_default(),
            page: None,
            structure: None,
            // Bound when the tab is first shown: a window that comes back
            // with six tabs on three databases opens three connections the
            // moment it launches if they are all bound here, and five of the
            // six are not being looked at.
            session: None,
            // What the tab was connected to last time, kept until the tab is
            // actually looked at.
            reconnect: match (self.connection, self.database.clone()) {
                (Some(connection), Some(database)) => Some((connection, database)),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one-pane window every test but the split ones is about.
    fn one_pane(tabs: &[CenterTab]) -> State {
        let snapshot = PaneSnapshot {
            id: 0,
            tabs,
            active: 0,
            sources: Vec::new(),
        };
        State::from_workspace(None, None, &[snapshot], &PaneGroup::new(0), 0)
    }

    fn round_trip(state: &State) -> State {
        serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap()
    }

    fn query(title: &str, sql: &str) -> CenterTab {
        CenterTab {
            kind: CenterKind::Query,
            title: title.into(),
            detail: None,
            dirty: true,
            pinned: false,
            relation: None,
            key: None,
            saved_query: None,
            sql: sql.to_string(),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: None,
            // Bound when the tab is first shown: a window that comes back
            // with six tabs on three databases opens three connections the
            // moment it launches if they are all bound here, and five of the
            // six are not being looked at.
            session: None,
            reconnect: None,
        }
    }

    #[test]
    fn a_half_written_statement_survives_a_restart() {
        let tabs = vec![query("Untitled", "select * from users where")];
        let restored = round_trip(&one_pane(&tabs)).tabs().expect("one tab");
        assert_eq!(restored[0].sql, "select * from users where");
    }

    #[test]
    fn a_browsed_table_comes_back_pointing_at_the_same_relation() {
        let tabs = vec![CenterTab {
            kind: CenterKind::Table,
            title: "users".into(),
            detail: Some("public".into()),
            dirty: false,
            pinned: false,
            relation: Some(db::RelationRef::new("public", "users")),
            key: None,
            saved_query: None,
            sql: String::new(),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: None,
            // Bound when the tab is first shown: a window that comes back
            // with six tabs on three databases opens three connections the
            // moment it launches if they are all bound here, and five of the
            // six are not being looked at.
            session: None,
            reconnect: None,
        }];
        let restored = round_trip(&one_pane(&tabs)).tabs().expect("one tab");
        assert_eq!(restored[0].kind, CenterKind::Table);
        assert_eq!(
            restored[0].relation,
            Some(db::RelationRef::new("public", "users"))
        );
    }

    #[test]
    fn each_tab_comes_back_on_its_own_database() {
        // Two tabs, two databases, one window. Restoring both onto whichever
        // one was last clicked is the bug this field exists to prevent.
        let tabs = vec![query("left", "select 1"), query("right", "select 2")];
        let one = Uuid::new_v4();
        let two = Uuid::new_v4();
        let state = State::from_workspace(
            Some(one),
            Some("oracle".to_string()),
            &[PaneSnapshot {
                id: 0,
                tabs: &tabs,
                active: 0,
                sources: vec![
                    Some((one, "oracle".to_string())),
                    Some((two, "kuber".to_string())),
                ],
            }],
            &PaneGroup::new(0),
            0,
        );

        let back = round_trip(&state).tabs().expect("two tabs");
        assert_eq!(back[0].reconnect, Some((one, "oracle".to_string())));
        assert_eq!(back[1].reconnect, Some((two, "kuber".to_string())));
    }

    /// A tab nobody has connected saves without a source and comes back the
    /// same way, rather than borrowing the window's.
    #[test]
    fn a_tab_with_no_connection_names_none() {
        let tabs = vec![query("q", "select 1")];
        let back = round_trip(&one_pane(&tabs)).tabs().expect("one tab");
        assert_eq!(back[0].reconnect, None);
    }

    #[test]
    fn nothing_is_dirty_at_launch() {
        let tabs = vec![query("Untitled", "select 1")];
        assert!(!one_pane(&tabs).tabs().unwrap()[0].dirty);
    }

    #[test]
    fn a_first_launch_has_no_tabs_to_restore() {
        assert!(State::default().tabs().is_none());
    }

    #[test]
    fn an_active_index_past_the_end_falls_back_to_the_first_tab() {
        let state = State {
            tabs: vec![Tab::default()],
            active: Some(7),
            ..State::default()
        };
        assert_eq!(state.active(), 0);
    }

    #[test]
    fn the_database_you_switched_to_is_the_one_that_comes_back() {
        let tabs = vec![query("q", "select 1")];
        let id = Uuid::new_v4();
        let state = State::from_workspace(
            Some(id),
            Some("kuber".to_string()),
            &[PaneSnapshot {
                id: 0,
                tabs: &tabs,
                active: 0,
                sources: Vec::new(),
            }],
            &PaneGroup::new(0),
            0,
        );

        let back = round_trip(&state);
        assert_eq!(back.connection, Some(id));
        assert_eq!(back.database.as_deref(), Some("kuber"));
    }

    /// A file written before the field existed says nothing about the
    /// database, and nothing is the right answer: the connection's own default
    /// is where that build would have put you.
    #[test]
    fn an_older_file_names_no_database() {
        let back: State = serde_json::from_str(r#"{"tabs":[]}"#).unwrap();
        assert_eq!(back.database, None);
    }

    #[test]
    fn a_split_window_comes_back_split() {
        let left = vec![query("left", "select 1")];
        let right = vec![query("right", "select 2")];
        let mut layout = PaneGroup::new(0);
        layout.split(0, 7, Layout::Rows);
        layout.resize_to(&[], 0, 0.7);
        let state = State::from_workspace(
            None,
            None,
            &[
                PaneSnapshot {
                    id: 0,
                    tabs: &left,
                    active: 0,
                    sources: Vec::new(),
                },
                PaneSnapshot {
                    id: 7,
                    tabs: &right,
                    active: 0,
                    sources: Vec::new(),
                },
            ],
            &layout,
            7,
        );

        let back = round_trip(&state);
        let panes = back.panes().expect("two panes");
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].0[0].sql, "select 1");
        assert_eq!(panes[1].0[0].sql, "select 2");
        // The id the pane had is gone; the pane it was is the second one.
        assert_eq!(back.active_pane(2), 1);

        let tree = back.tree(2).expect("a tree");
        assert_eq!(tree.layout, Layout::Rows);
        assert_eq!(tree.panes(), vec![0, 1]);
        assert!((tree.flexes[0] - 0.7).abs() < 0.001);
    }

    #[test]
    fn the_pane_you_were_typing_in_is_the_one_an_older_build_reopens() {
        let left = vec![query("left", "select 1")];
        let right = vec![query("right", "select 2")];
        let mut layout = PaneGroup::new(0);
        layout.split(0, 7, Layout::Columns);
        let state = State::from_workspace(
            None,
            None,
            &[
                PaneSnapshot {
                    id: 0,
                    tabs: &left,
                    active: 0,
                    sources: Vec::new(),
                },
                PaneSnapshot {
                    id: 7,
                    tabs: &right,
                    active: 0,
                    sources: Vec::new(),
                },
            ],
            &layout,
            7,
        );
        assert_eq!(state.tabs().unwrap()[0].sql, "select 2");
    }

    #[test]
    fn a_layout_that_does_not_match_the_panes_is_dropped_rather_than_repaired() {
        let tabs = vec![query("only", "select 1")];
        let mut state = one_pane(&tabs);
        state.split = Some(Split::Group {
            layout: "columns".into(),
            members: vec![Split::Pane(0), Split::Pane(4)],
            flexes: vec![0.5, 0.5],
        });
        assert!(state.tree(1).is_none());
        // …and the panes themselves still come back.
        assert_eq!(state.panes().expect("one pane").len(), 1);
    }

    #[test]
    fn one_pane_writes_no_layout_at_all() {
        let tabs = vec![query("only", "select 1")];
        let text = serde_json::to_string(&one_pane(&tabs)).unwrap();
        assert!(!text.contains("split"), "{text}");
    }

    #[test]
    fn a_session_from_before_splits_reopens_as_one_pane() {
        let state: State =
            serde_json::from_str(r#"{"tabs":[{"title":"scratch","sql":"select 1"}]}"#).unwrap();
        let panes = state.panes().expect("one pane");
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].0[0].sql, "select 1");
        assert!(state.tree(1).is_none());
    }

    #[test]
    fn a_session_from_an_older_build_still_loads() {
        let state: State = serde_json::from_str(r#"{"tabs":[{"title":"scratch"}]}"#).unwrap();
        let tabs = state.tabs().expect("one tab");
        assert_eq!(tabs[0].kind, CenterKind::Query);
        assert_eq!(tabs[0].title, "scratch");
    }
}
