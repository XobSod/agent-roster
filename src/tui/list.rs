//! What the session list shows: sessions on shelves by attention (or grouped by
//! project), in which order, and which groups are folded.

use std::cmp::Reverse;
use std::collections::HashSet;

use crate::model::{Project, Session, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shelf {
    NeedsYou,
    Idle,
    Working,
    Stopped,
}

impl Shelf {
    pub const ALL: [Shelf; 4] = [Shelf::NeedsYou, Shelf::Idle, Shelf::Working, Shelf::Stopped];

    pub fn of(s: &Session) -> Shelf {
        match s.state {
            State::NeedsInput => Shelf::NeedsYou,
            State::Idle if s.unread => Shelf::NeedsYou,
            State::Idle => Shelf::Idle,
            State::Working => Shelf::Working,
            State::Stopped => Shelf::Stopped,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Shelf::NeedsYou => "Needs you",
            Shelf::Idle => "Idle",
            Shelf::Working => "Working",
            Shelf::Stopped => "Stopped",
        }
    }
}

/// A foldable group: a shelf, or a project when grouped by project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Shelf(Shelf),
    Project(i64),
}

#[derive(Clone, Debug)]
pub enum Entry {
    Heading {
        group: Group,
        /// The shelf's title or the project's name.
        label: String,
        count: usize,
        open: bool,
    },
    /// A two-line card.
    Card(Session),
    /// A one-line row: stopped and deleted sessions.
    Row(Session),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKey {
    Group(Group),
    Session(String),
}

impl Entry {
    pub fn key(&self) -> EntryKey {
        match self {
            Entry::Heading { group, .. } => EntryKey::Group(*group),
            Entry::Card(s) | Entry::Row(s) => EntryKey::Session(s.id.clone()),
        }
    }

    pub fn session(&self) -> Option<&Session> {
        match self {
            Entry::Card(s) | Entry::Row(s) => Some(s),
            Entry::Heading { .. } => None,
        }
    }
}

pub struct Options<'a> {
    pub grouped: bool,
    pub folded: &'a HashSet<Group>,
    /// Show only this project's sessions.
    pub filter: Option<i64>,
}

/// Attention order: blocked on the user, then unread, idle, working, stopped.
fn rank(s: &Session) -> u8 {
    match (s.state, s.unread) {
        (State::NeedsInput, _) => 0,
        (State::Idle, true) => 1,
        (State::Idle, false) => 2,
        (State::Working, _) => 3,
        (State::Stopped, _) => 4,
    }
}

/// Most attention first, then the most recent state change first. A working
/// session's state changes only when its turn starts, so cards don't jump around.
fn sort(sessions: &mut [&Session]) {
    sessions.sort_by_key(|s| (rank(s), Reverse(s.state_since)));
}

fn entry(s: &Session) -> Entry {
    if s.state == State::Stopped {
        Entry::Row(s.clone())
    } else {
        Entry::Card(s.clone())
    }
}

/// The live sessions, on shelves or grouped by project.
pub fn live(projects: &[Project], sessions: &[Session], opts: &Options) -> Vec<Entry> {
    let shown: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.deleted_at.is_none() && opts.filter.is_none_or(|p| s.project_id == p))
        .collect();
    let mut entries = Vec::new();
    if opts.grouped {
        for p in projects
            .iter()
            .filter(|p| opts.filter.is_none_or(|f| p.id == f))
        {
            let mut mine: Vec<&Session> = shown
                .iter()
                .copied()
                .filter(|s| s.project_id == p.id)
                .collect();
            sort(&mut mine);
            let group = Group::Project(p.id);
            let open = !opts.folded.contains(&group);
            entries.push(Entry::Heading {
                group,
                label: p.name.clone(),
                count: mine.len(),
                open,
            });
            if open {
                entries.extend(mine.into_iter().map(entry));
            }
        }
    } else {
        for shelf in Shelf::ALL {
            let mut mine: Vec<&Session> = shown
                .iter()
                .copied()
                .filter(|s| Shelf::of(s) == shelf)
                .collect();
            if mine.is_empty() {
                continue;
            }
            sort(&mut mine);
            let group = Group::Shelf(shelf);
            let open = !opts.folded.contains(&group);
            entries.push(Entry::Heading {
                group,
                label: shelf.title().into(),
                count: mine.len(),
                open,
            });
            if open {
                entries.extend(mine.into_iter().map(entry));
            }
        }
    }
    entries
}

/// Deleted sessions, most recently deleted first.
pub fn deleted(sessions: &[Session]) -> Vec<Entry> {
    let mut gone: Vec<&Session> = sessions.iter().filter(|s| s.deleted_at.is_some()).collect();
    gone.sort_by_key(|s| Reverse(s.deleted_at));
    gone.into_iter().map(|s| Entry::Row(s.clone())).collect()
}

/// The group a session sits in, given how the list is arranged.
pub fn group_of(s: &Session, grouped: bool) -> Group {
    if grouped {
        Group::Project(s.project_id)
    } else {
        Group::Shelf(Shelf::of(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::session;

    fn project(id: i64, name: &str) -> Project {
        Project {
            id,
            name: name.into(),
            origin: "/p".into(),
            agent: None,
            model: None,
            effort: None,
        }
    }

    fn with(id: &str, project: i64, state: State, unread: bool, since: i64) -> Session {
        let mut s = session(id);
        s.project_id = project;
        s.state = state;
        s.unread = unread;
        s.state_since = Some(since);
        s
    }

    fn keys(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Heading {
                    label, count, open, ..
                } => format!("[{label} {count}{}]", if *open { "" } else { " folded" }),
                Entry::Card(s) => s.id.clone(),
                Entry::Row(s) => format!("row:{}", s.id),
            })
            .collect()
    }

    fn sessions() -> Vec<Session> {
        vec![
            with("working-old", 1, State::Working, false, 10),
            with("idle", 1, State::Idle, false, 20),
            with("unread", 2, State::Idle, true, 30),
            with("input", 2, State::NeedsInput, true, 5),
            with("working-new", 2, State::Working, false, 40),
            with("stopped", 1, State::Stopped, false, 50),
        ]
    }

    #[test]
    fn shelves_by_attention() {
        let folded = HashSet::new();
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
        };
        let entries = live(&[project(1, "a"), project(2, "b")], &sessions(), &opts);
        assert_eq!(
            keys(&entries),
            [
                "[Needs you 2]",
                "input",
                "unread",
                "[Idle 1]",
                "idle",
                "[Working 2]",
                "working-new",
                "working-old",
                "[Stopped 1]",
                "row:stopped",
            ]
        );
    }

    #[test]
    fn folded_groups_keep_their_heading_and_count() {
        let folded = HashSet::from([Group::Shelf(Shelf::Stopped), Group::Shelf(Shelf::Working)]);
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
        };
        let entries = live(&[project(1, "a"), project(2, "b")], &sessions(), &opts);
        let shown = keys(&entries);
        assert!(shown.contains(&"[Working 2 folded]".to_string()));
        assert!(!shown.contains(&"working-new".to_string()));
        assert_eq!(shown.last().unwrap(), "[Stopped 1 folded]");
    }

    #[test]
    fn grouped_by_project_in_attention_order() {
        let folded = HashSet::new();
        let opts = Options {
            grouped: true,
            folded: &folded,
            filter: None,
        };
        let entries = live(
            &[project(1, "a"), project(2, "b"), project(3, "empty")],
            &sessions(),
            &opts,
        );
        assert_eq!(
            keys(&entries),
            [
                "[a 3]",
                "idle",
                "working-old",
                "row:stopped",
                "[b 3]",
                "input",
                "unread",
                "working-new",
                "[empty 0]",
            ]
        );
    }

    #[test]
    fn filter_shows_one_project() {
        let folded = HashSet::new();
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: Some(2),
        };
        let entries = live(&[project(1, "a"), project(2, "b")], &sessions(), &opts);
        assert_eq!(
            keys(&entries),
            [
                "[Needs you 2]",
                "input",
                "unread",
                "[Working 1]",
                "working-new"
            ]
        );
    }

    #[test]
    fn deleted_sessions_newest_first() {
        let mut a = session("a");
        a.deleted_at = Some(1);
        let mut b = session("b");
        b.deleted_at = Some(2);
        assert_eq!(keys(&deleted(&[a, b, session("live")])), ["row:b", "row:a"]);
    }
}
