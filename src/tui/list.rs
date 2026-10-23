//! What the session list shows: sessions on shelves by attention (or grouped by
//! project), archived ones last, in which order, and which groups are folded.

use std::collections::{HashMap, HashSet};

use crate::model::{Project, Session, State};
use crate::search::Hit;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shelf {
    NeedsInput,
    Idle,
    Stopped,
    Working,
    Archived,
}

impl Shelf {
    /// Waiting for you first (a question or a finished turn, a task, a start),
    /// then what's handed off (working) and what's finished (archived).
    pub const ALL: [Shelf; 5] = [
        Shelf::NeedsInput,
        Shelf::Idle,
        Shelf::Stopped,
        Shelf::Working,
        Shelf::Archived,
    ];

    pub fn of(s: &Session) -> Shelf {
        if s.deleted_at.is_some() {
            return Shelf::Archived;
        }
        match s.state {
            State::NeedsInput => Shelf::NeedsInput,
            State::Idle if s.unread => Shelf::NeedsInput,
            State::Idle => Shelf::Idle,
            State::Stopped => Shelf::Stopped,
            State::Working => Shelf::Working,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Shelf::NeedsInput => "Needs input",
            Shelf::Idle => "Idle",
            Shelf::Stopped => "Stopped",
            Shelf::Working => "Working",
            Shelf::Archived => "Archived",
        }
    }

    /// Kept at the bottom of the list while everything fits.
    pub fn at_bottom(self) -> bool {
        matches!(self, Shelf::Working | Shelf::Archived)
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
    /// A one-line row: stopped and archived sessions.
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

    /// The heading of a shelf kept at the bottom.
    pub fn at_bottom(&self) -> bool {
        matches!(self, Entry::Heading { group: Group::Shelf(s), .. } if s.at_bottom())
    }
}

pub struct Options<'a> {
    pub grouped: bool,
    pub folded: &'a HashSet<Group>,
    /// Show only this project's sessions.
    pub filter: Option<i64>,
    /// Show only these sessions (matching the text filter), by id.
    pub hits: Option<&'a HashMap<String, Hit>>,
    /// Oldest or newest first, per shelf.
    pub order: &'a crate::config::Order,
}

/// Most attention first, then by when each got there, as `order` says
/// (default: first come, first served; archived: the latest first). A working
/// session's state changes only when its turn starts, so cards don't jump.
fn sort(sessions: &mut [&Session], order: &crate::config::Order) {
    sessions.sort_by(|a, b| a.list_order(b, order));
}

fn entry(s: &Session) -> Entry {
    if s.state == State::Stopped || s.deleted_at.is_some() {
        Entry::Row(s.clone())
    } else {
        Entry::Card(s.clone())
    }
}

/// The sessions on shelves, or grouped by project with the archived ones on
/// their shelf after the projects.
pub fn entries(projects: &[Project], sessions: &[Session], opts: &Options) -> Vec<Entry> {
    let mut entries = Vec::new();
    // A group's heading and, unless it's folded, its sessions; one with
    // none isn't shown, but for projects when not filtering by text.
    let mut push = |group: Group, label: String| {
        let mine = members(sessions, group, opts);
        let keep = match group {
            Group::Project(_) => opts.hits.is_none() || !mine.is_empty(),
            Group::Shelf(_) => !mine.is_empty(),
        };
        if !keep {
            return;
        }
        let open = !opts.folded.contains(&group);
        entries.push(Entry::Heading {
            group,
            label,
            count: mine.len(),
            open,
        });
        if open {
            entries.extend(mine.into_iter().map(entry));
        }
    };
    if opts.grouped {
        for p in projects
            .iter()
            .filter(|p| opts.filter.is_none_or(|f| p.id == f))
        {
            push(Group::Project(p.id), p.name.clone());
        }
        push(
            Group::Shelf(Shelf::Archived),
            Shelf::Archived.title().into(),
        );
    } else {
        for shelf in Shelf::ALL {
            push(Group::Shelf(shelf), shelf.title().into());
        }
    }
    entries
}

/// The sessions in `group` as the list shows them, filtered and in order,
/// whether it's folded or not. Grouped by project, the shelves but Archived
/// have none.
pub fn members<'a>(sessions: &'a [Session], group: Group, opts: &Options) -> Vec<&'a Session> {
    let mut mine: Vec<&Session> = sessions
        .iter()
        .filter(|s| opts.filter.is_none_or(|p| s.project_id == p))
        .filter(|s| opts.hits.is_none_or(|h| h.contains_key(&s.id)))
        .filter(|s| match group {
            Group::Project(id) => s.deleted_at.is_none() && s.project_id == id,
            Group::Shelf(shelf) => {
                Shelf::of(s) == shelf && (!opts.grouped || shelf == Shelf::Archived)
            }
        })
        .collect();
    sort(&mut mine, opts.order);
    mine
}

/// The group a session sits in, given how the list is arranged.
pub fn group_of(s: &Session, grouped: bool) -> Group {
    if grouped && s.deleted_at.is_none() {
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
            archived("archived", 1, 60),
        ]
    }

    fn archived(id: &str, project: i64, at: i64) -> Session {
        let mut s = with(id, project, State::Stopped, false, at);
        s.deleted_at = Some(at);
        s
    }

    #[test]
    fn shelves_by_attention() {
        let folded = HashSet::new();
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
            hits: None,
            order: &crate::config::Order::default(),
        };
        let entries = entries(&[project(1, "a"), project(2, "b")], &sessions(), &opts);
        assert_eq!(
            keys(&entries),
            [
                "[Needs input 2]",
                "input",
                "unread",
                "[Idle 1]",
                "idle",
                "[Stopped 1]",
                "row:stopped",
                "[Working 2]",
                "working-old",
                "working-new",
                "[Archived 1]",
                "row:archived",
            ]
        );
    }

    #[test]
    fn a_groups_members_are_its_sessions_as_listed() {
        let folded = HashSet::new();
        let order = crate::config::Order::default();
        let sessions = sessions();
        let ids = |group, opts: &Options| -> Vec<String> {
            members(&sessions, group, opts)
                .iter()
                .map(|s| s.id.clone())
                .collect()
        };
        let mut opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
            hits: None,
            order: &order,
        };
        assert_eq!(
            ids(Group::Shelf(Shelf::NeedsInput), &opts),
            ["input", "unread"]
        );
        assert_eq!(ids(Group::Shelf(Shelf::Archived), &opts), ["archived"]);
        // A project's are its live ones; grouped by project, only the
        // Archived shelf has any.
        opts.grouped = true;
        assert_eq!(
            ids(Group::Project(1), &opts),
            ["idle", "stopped", "working-old"]
        );
        assert!(ids(Group::Shelf(Shelf::NeedsInput), &opts).is_empty());
        assert_eq!(ids(Group::Shelf(Shelf::Archived), &opts), ["archived"]);
        // Filtered as the list is.
        opts.grouped = false;
        opts.filter = Some(2);
        assert_eq!(ids(Group::Shelf(Shelf::Working), &opts), ["working-new"]);
        let hits = HashMap::from([("unread".to_string(), Hit::default())]);
        opts.filter = None;
        opts.hits = Some(&hits);
        assert_eq!(ids(Group::Shelf(Shelf::NeedsInput), &opts), ["unread"]);
    }

    #[test]
    fn each_shelf_can_be_newest_first() {
        let folded = HashSet::new();
        let order = crate::config::Order {
            working: crate::config::First::Newest,
            ..Default::default()
        };
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
            hits: None,
            order: &order,
        };
        let sessions = [
            with("first", 1, State::Working, false, 10),
            with("second", 1, State::Working, false, 20),
            with("idle-1", 1, State::Idle, false, 10),
            with("idle-2", 1, State::Idle, false, 20),
        ];
        assert_eq!(
            keys(&entries(&[project(1, "a")], &sessions, &opts)),
            [
                "[Idle 2]",
                "idle-1",
                "idle-2",
                "[Working 2]",
                "second",
                "first"
            ]
        );
    }

    #[test]
    fn folded_groups_keep_their_heading_and_count() {
        let folded = HashSet::from([Group::Shelf(Shelf::Archived), Group::Shelf(Shelf::Working)]);
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
            hits: None,
            order: &crate::config::Order::default(),
        };
        let entries = entries(&[project(1, "a"), project(2, "b")], &sessions(), &opts);
        let shown = keys(&entries);
        assert!(shown.contains(&"[Working 2 folded]".to_string()));
        assert!(!shown.contains(&"working-new".to_string()));
        assert_eq!(shown.last().unwrap(), "[Archived 1 folded]");
        // The bottom group starts at Working.
        let first = entries.iter().position(Entry::at_bottom).unwrap();
        assert_eq!(shown[first], "[Working 2 folded]");
    }

    #[test]
    fn grouped_by_project_in_attention_order() {
        let folded = HashSet::new();
        let opts = Options {
            grouped: true,
            folded: &folded,
            filter: None,
            hits: None,
            order: &crate::config::Order::default(),
        };
        let entries = entries(
            &[project(1, "a"), project(2, "b"), project(3, "empty")],
            &sessions(),
            &opts,
        );
        assert_eq!(
            keys(&entries),
            [
                "[a 3]",
                "idle",
                "row:stopped",
                "working-old",
                "[b 3]",
                "input",
                "unread",
                "working-new",
                "[empty 0]",
                "[Archived 1]",
                "row:archived",
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
            hits: None,
            order: &crate::config::Order::default(),
        };
        let entries = entries(&[project(1, "a"), project(2, "b")], &sessions(), &opts);
        assert_eq!(
            keys(&entries),
            [
                "[Needs input 2]",
                "input",
                "unread",
                "[Working 1]",
                "working-new"
            ]
        );
    }

    #[test]
    fn text_filter_shows_matches_only() {
        let folded = HashSet::new();
        let hits: HashMap<String, Hit> = ["unread", "working-new", "archived"]
            .map(|id| (id.to_string(), Hit::default()))
            .into();
        let mut opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
            hits: Some(&hits),
            order: &crate::config::Order::default(),
        };
        let projects = [project(1, "a"), project(2, "b"), project(3, "empty")];
        assert_eq!(
            keys(&entries(&projects, &sessions(), &opts)),
            [
                "[Needs input 1]",
                "unread",
                "[Working 1]",
                "working-new",
                "[Archived 1]",
                "row:archived",
            ]
        );
        // Grouped: projects without a match are left out.
        opts.grouped = true;
        assert_eq!(
            keys(&entries(&projects, &sessions(), &opts)),
            [
                "[b 2]",
                "unread",
                "working-new",
                "[Archived 1]",
                "row:archived"
            ]
        );
    }

    #[test]
    fn archived_sessions_newest_first() {
        let folded = HashSet::new();
        let opts = Options {
            grouped: false,
            folded: &folded,
            filter: None,
            hits: None,
            order: &crate::config::Order::default(),
        };
        let sessions = [archived("a", 1, 1), archived("b", 1, 2)];
        assert_eq!(
            keys(&entries(&[project(1, "a")], &sessions, &opts)),
            ["[Archived 2]", "row:b", "row:a"]
        );
    }
}
