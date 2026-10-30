//! TUI state and key handling. No terminal or tmux calls here: keys become `Op`s
//! that the run loop carries out, so this part is testable on its own.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Text;

mod act;
mod dialog;
mod settings;
mod texts;

use dialog::Purpose;
pub use dialog::{Dialog, completions};
pub use texts::screen_tail;

use super::form::{self, Form, Outcome};
use super::input;
use super::list::{self, Entry, EntryKey, Group, Shelf};
use super::note::{self, NoteEditor, OpenNotes, Side};
use super::op::{self, Op};
use super::picker::{Picked, Picker};
use crate::keys::{Action, Keymap, Menu};
use crate::model::{Project, Session, State};
use crate::note::Whose;
use crate::paths::PROJECT_ENV;
use crate::search::Hit;

const MESSAGE_TTL: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The session list, by shelf or by project.
    Sessions,
    /// Every project, to add, edit, remove, or pick one.
    Projects,
    /// The tmux server, database and processes report.
    Server,
    /// Every setting, to see and change.
    Settings,
    /// What the agents' accounts have used of their limits.
    Usage,
}

pub struct Message {
    pub text: String,
    pub error: bool,
    at: Instant,
}

/// The preview: the selected session's screen and statistics (or what's
/// shown for other selections), and how agents are sized to it.
#[derive(Default)]
pub struct Preview {
    /// The screen, or information about the selection.
    pub text: Text<'static>,
    /// Whether `text` is the selected session's screen.
    pub screen: bool,
    /// What the selected session's agent can tell about it.
    pub stats: Option<crate::stats::AgentStats>,
    /// Transcript readers by session, kept so each read only takes what's new.
    pub transcripts: HashMap<String, Box<dyn crate::stats::Transcript>>,
    /// Those read the first time on a thread (a heading's sessions').
    pub reading: Reading,
    /// Its size when last drawn; agents drawing full screen get it.
    pub size: Cell<Option<(u16, u16)>>,
    /// Agents fitted to `fitted_size` (see `Roster::fit_screens`).
    pub fitted: HashSet<String>,
    pub fitted_size: Option<(u16, u16)>,
}

/// Transcripts read for the first time on a thread, as a big one would hold
/// the TUI up (75 ms for 111 MB): which sessions' are being read, and the
/// channel each comes back by (None if it couldn't be read).
pub struct Reading {
    pub ids: HashSet<String>,
    pub send: Sender<(String, Option<Box<dyn crate::stats::Transcript>>)>,
    pub came: Receiver<(String, Option<Box<dyn crate::stats::Transcript>>)>,
}

impl Default for Reading {
    fn default() -> Self {
        let (send, came) = std::sync::mpsc::channel();
        Reading {
            ids: HashSet::new(),
            send,
            came,
        }
    }
}

/// A title being made for session `id` (`s t`): its agent's answer to come,
/// and what the session was when asked.
pub struct Titling {
    pub id: String,
    pub title: std::sync::mpsc::Receiver<anyhow::Result<String>>,
    pub asked_of: crate::title::AskedOf,
}

/// What the session list shows beyond its sessions, handed to the read-only
/// list beside an attached agent, so it's the list the TUI showed.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct View {
    grouped: bool,
    filter: Option<i64>,
    search: String,
    folded: Vec<Group>,
    search_folded: Vec<Group>,
    unfolded: Vec<String>,
    scroll: usize,
    memory_marked: Vec<String>,
    /// What the accounts last said, if the list shows it.
    usage: Option<Vec<super::limits::Said>>,
}

pub struct App {
    pub keymap: Keymap,
    pub mode: Mode,
    /// Grouped by project instead of shelves.
    pub grouped: bool,
    /// Showing only this project's sessions.
    pub filter: Option<i64>,
    /// The text filter (`/`), and the sessions matching it by id (None: none).
    pub search: String,
    pub hits: Option<HashMap<String, Hit>>,
    /// Typing the filter, so keys go to it: what it and the selection were
    /// before, for Esc.
    pub typing: Option<(String, Option<EntryKey>)>,
    folded: HashSet<Group>,
    /// Folded while filtering by text; every group starts open, so what
    /// matches shows, archived too.
    search_folded: HashSet<Group>,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// First visible list row; the drawing code keeps the selection in view.
    pub scroll: Cell<usize>,
    /// The menu whose popup is open.
    pub menu: Option<Menu>,
    pub help: bool,
    pub dialog: Option<Dialog>,
    /// What the right of the list shows of the selection.
    pub preview: Preview,
    /// The server view's report, its orphan count, and how far it's scrolled
    /// (the drawing keeps the cursor's line in view).
    pub report: Vec<crate::doctor::Line>,
    pub orphans: usize,
    pub problems: usize,
    pub report_scroll: Cell<usize>,
    /// Its sessions unfolded to their processes, and the processes folded.
    pub report_open: HashSet<String>,
    pub report_folded: HashSet<crate::doctor::What>,
    /// The cursor's line, of those shown, and the session or process it's
    /// on, or else its text and how far from the end it is (followed when
    /// the report is read again).
    pub report_cursor: usize,
    pub report_selected: Option<crate::doctor::What>,
    report_anchor: Option<(String, usize)>,
    /// The selected row of the projects view.
    pub project_row: usize,
    /// A project to select there once it shows up (after adding or renaming it).
    select_project: Option<String>,
    /// The built-in project chats run in, once it exists.
    pub chat_project: Option<i64>,
    message: Option<Message>,
    pub projects: Vec<Project>,
    /// Live and archived.
    pub sessions: Vec<Session>,
    select_next: Option<EntryKey>,
    /// The cursor's row when attaching, while attached.
    attached_row: Option<usize>,
    /// Back from giving a session a task: keep this row (see `rebuild`).
    keep_row: Option<usize>,
    /// Suggested origin for a new project.
    default_path: String,
    /// Agents that can be started, for the forms.
    agents: Vec<String>,
    /// The agents' models, for the forms: which to offer, what each is.
    pub models: form::Models,
    /// The agents' models being listed in the background (see `tui::run`).
    pub listing: Option<std::sync::mpsc::Receiver<(String, Vec<crate::agent::Model>)>>,
    /// The settings view's settings, and the one selected (of all, in order).
    pub settings: Vec<crate::settings::Section>,
    pub setting_row: usize,
    /// Where config.toml is, to say so.
    pub config_file: std::path::PathBuf,
    /// A key setting waiting for its key to be pressed.
    pub capturing: Option<settings::Capture>,
    /// Oldest or newest first, per shelf (config.toml).
    pub order: crate::config::Order,
    /// A session's note and its project's, open to read and write; keys go
    /// to the one shown.
    pub note: Option<OpenNotes>,
    /// Titles being made (`s t`), by session: each its agent's answer to
    /// come, on a thread of its own.
    pub titling: Vec<Titling>,
    /// Where notes open, and the editor's keys (config.toml).
    pub notes: crate::config::Notes,
    /// What the agents' accounts have used of their limits, and whether the
    /// sidebar shows it (config.toml).
    pub limits: super::limits::Limits,
    /// Whether the accounts are asked at all (`account_limits`).
    pub account_limits: bool,
    pub sidebar_usage: bool,
    /// What cards say of their sessions' subagents and shells at work
    /// (config.toml), the cards unfolded to show them (`l`), by session, and
    /// what's read of each subagent, by its agent's id for it.
    pub card_subagents: crate::config::CardSubagents,
    /// Above how much memory a session's processes are said to use too
    /// much; None: not said.
    pub memory_limit: Option<u64>,
    /// What each session's processes use, by its id, read every few seconds
    /// while that's said (and when), and those said to use too much.
    pub resources: HashMap<String, crate::procs::Totals>,
    pub resources_read: Option<Instant>,
    memory_marked: HashSet<String>,
    pub unfolded: HashSet<String>,
    pub subagent_info: HashMap<String, crate::agent::SubagentInfo>,
    /// What each of those is doing now (its latest step), by its id.
    pub subagent_doing: HashMap<String, String>,
    /// What the agents spent, and the period the usage view shows of it
    /// (None: the limits).
    pub tokens: super::tokens::Tokens,
    pub usage_period: Option<crate::tokens::Period>,
    /// How far the usage view is scrolled, and how far it can be (as last
    /// drawn).
    pub usage_scroll: usize,
    pub usage_scroll_max: Cell<usize>,
}

impl App {
    pub fn new(keymap: Keymap, default_path: String, agents: Vec<String>) -> App {
        App {
            keymap,
            mode: Mode::Sessions,
            grouped: false,
            filter: None,
            search: String::new(),
            hits: None,
            typing: None,
            folded: HashSet::from([Group::Shelf(Shelf::Archived)]),
            search_folded: HashSet::new(),
            entries: Vec::new(),
            selected: 0,
            scroll: Cell::new(0),
            menu: None,
            help: false,
            dialog: None,
            preview: Preview::default(),
            report: Vec::new(),
            orphans: 0,
            problems: 0,
            report_scroll: Cell::new(0),
            report_open: HashSet::new(),
            report_folded: HashSet::new(),
            report_cursor: 0,
            report_selected: None,
            report_anchor: None,
            project_row: 0,
            select_project: None,
            chat_project: None,
            message: None,
            projects: Vec::new(),
            sessions: Vec::new(),
            select_next: None,
            attached_row: None,
            keep_row: None,
            default_path,
            agents,
            models: form::Models::known(),
            listing: None,
            settings: Vec::new(),
            setting_row: 0,
            config_file: std::path::PathBuf::new(),
            capturing: None,
            order: Default::default(),
            note: None,
            titling: Vec::new(),
            notes: Default::default(),
            limits: Default::default(),
            account_limits: true,
            sidebar_usage: true,
            card_subagents: Default::default(),
            memory_limit: None,
            resources: HashMap::new(),
            resources_read: None,
            memory_marked: HashSet::new(),
            unfolded: HashSet::new(),
            subagent_info: HashMap::new(),
            subagent_doing: HashMap::new(),
            tokens: Default::default(),
            usage_period: None,
            usage_scroll: 0,
            usage_scroll_max: Cell::new(0),
        }
    }

    /// The agents that can be started, after a change of settings.
    pub fn set_agents(&mut self, agents: Vec<String>) {
        self.agents = agents;
    }

    /// The agent new sessions get unless their project picks one: the first
    /// of those that can be started (`agent::available`).
    pub fn default_agent(&self) -> &str {
        self.agents.first().map_or("", String::as_str)
    }

    /// Whether starting a session of `agent` again resumes its conversation.
    pub fn resumes(&self, agent: &str) -> bool {
        crate::agent::adapter(agent).resumes()
    }

    /// A form in a dialog, with what model choices turned out to be.
    pub(super) fn open_form(&mut self, form: Form) {
        self.dialog = Some(Dialog::Form(Box::new(form.with_models(&self.models))));
    }

    /// Whether `s`'s card shows a line for each of its subagents and shells
    /// at work (as of `now`): unfolded, or always.
    pub fn at_work_shown(&self, s: &Session, now: i64) -> bool {
        use crate::config::CardSubagents;
        s.at_work(now) > 0
            && match self.card_subagents {
                CardSubagents::Count => false,
                CardSubagents::Folded => self.unfolded.contains(&s.id),
                CardSubagents::Unfolded => true,
            }
    }

    /// What the list shows beyond its sessions (`View`).
    pub fn view(&self) -> View {
        View {
            grouped: self.grouped,
            filter: self.filter,
            search: self.search.clone(),
            folded: self.folded.iter().copied().collect(),
            search_folded: self.search_folded.iter().copied().collect(),
            unfolded: self.unfolded.iter().cloned().collect(),
            scroll: self.scroll.get(),
            memory_marked: self.memory_marked.iter().cloned().collect(),
            usage: self.sidebar_usage.then(|| self.limits.agents.clone()),
        }
    }

    /// The list as `view` says, before its sessions are set.
    pub fn take_view(&mut self, view: View) {
        self.grouped = view.grouped;
        self.filter = view.filter;
        self.search = view.search;
        self.folded = view.folded.into_iter().collect();
        self.search_folded = view.search_folded.into_iter().collect();
        self.unfolded = view.unfolded.into_iter().collect();
        self.scroll.set(view.scroll);
        self.memory_marked = view.memory_marked.into_iter().collect();
        self.sidebar_usage = view.usage.is_some();
        self.limits.agents = view.usage.unwrap_or_default();
    }

    /// Grouped by project, or on shelves (`group_by` in config.toml).
    pub fn set_grouped(&mut self, grouped: bool) {
        if self.grouped != grouped {
            self.grouped = grouped;
            self.rebuild();
        }
    }

    /// Replaces the data, keeping the selection on the same entry.
    pub fn set_data(&mut self, projects: Vec<Project>, sessions: Vec<Session>) {
        self.projects = projects;
        self.sessions = sessions;
        if let Some(name) = self.select_project.take()
            && let Some(row) = self.projects.iter().position(|p| p.name == name)
        {
            self.project_row = row;
        }
        self.project_row = self.project_row.min(self.projects.len().saturating_sub(1));
        if self
            .filter
            .is_some_and(|f| !self.projects.iter().any(|p| p.id == f))
        {
            self.filter = None;
        }
        self.note_changes();
        self.rebuild();
    }

    /// Takes each open note saved by another since it was opened or saved
    /// as it is now if it's untouched, else marks it so.
    fn note_changes(&mut self) {
        let Some(open) = self.note.as_mut() else {
            return;
        };
        for editor in open.editors_mut() {
            let current = match &editor.whose {
                Whose::Session(id) => self.sessions.iter().find(|s| s.id == *id).map(|s| &s.note),
                Whose::Project(id) => self.projects.iter().find(|p| p.id == *id).map(|p| &p.note),
            };
            match current {
                Some(current) if current.as_ref() == editor.base.as_ref() => {
                    editor.external = false
                }
                Some(current) if editor.untouched() => editor.reload(current.as_ref()),
                Some(_) => editor.external = true,
                None => {}
            }
        }
    }

    fn rebuild(&mut self) {
        let keep = self.select_next.take().or_else(|| self.selected_key());
        self.hits = crate::search::hits(
            &self.search,
            self.sessions
                .iter()
                .filter(|s| self.filter.is_none_or(|p| s.project_id == p)),
        );
        // In the server view the list isn't shown, but kept for coming back.
        self.entries = list::entries(&self.projects, &self.sessions, &self.list_options());
        let position = |key: &EntryKey| self.entries.iter().position(|e| e.key() == *key);
        // A selected session that moved into a folded group: select that group.
        let found = keep.as_ref().and_then(|k| {
            position(k).or_else(|| match k {
                EntryKey::Session(id) => {
                    let s = self.sessions.iter().find(|s| s.id == *id)?;
                    position(&EntryKey::Group(list::group_of(s, self.grouped)))
                }
                _ => None,
            })
        });
        // At first, the first session rather than a heading.
        let first_session = || self.entries.iter().position(|e| e.session().is_some());
        let fallback = if keep.is_none() {
            first_session().unwrap_or(0)
        } else {
            self.selected
        };
        self.selected = found
            .unwrap_or(fallback)
            .min(self.entries.len().saturating_sub(1));
        // Back from giving a session a task: the cursor keeps its row, so it's
        // on whatever took the session's place, the next one waiting. Otherwise
        // (only looked, or read its turn) it follows the session, as always.
        if let Some(row) = self.keep_row.take()
            && self.mode == Mode::Sessions
            && !self.entries.is_empty()
        {
            let n = self.entries.len();
            let row = row.min(n - 1);
            // A heading there: the session below it (or above, at the end).
            self.selected = (row..n)
                .chain((0..row).rev())
                .find(|&i| self.entries[i].session().is_some())
                .unwrap_or(row);
        }
    }

    /// Notes where the cursor is before attaching to session `id`, the selected
    /// one.
    pub fn attaching(&mut self, id: &str) {
        self.attached_row = self
            .selected_session()
            .is_some_and(|s| s.id == id)
            .then_some(self.selected);
    }

    /// Back from attaching; `prompted`: a prompt was submitted meanwhile, so the
    /// next rebuild keeps the cursor's row (see `rebuild`).
    pub fn detached(&mut self, prompted: bool) {
        self.keep_row = self.attached_row.take().filter(|_| prompted);
    }

    /// The sessions in `group` as the list shows them (`list::members`),
    /// folded or not.
    pub fn group_members(&self, group: Group) -> Vec<&Session> {
        list::members(&self.sessions, group, &self.list_options())
    }

    /// How the list is arranged and filtered now.
    fn list_options(&self) -> list::Options<'_> {
        list::Options {
            grouped: self.grouped,
            folded: self.folds(),
            filter: self.filter,
            hits: self.hits.as_ref(),
            order: &self.order,
        }
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    pub fn selected_key(&self) -> Option<EntryKey> {
        self.selected_entry().map(Entry::key)
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.selected_entry().and_then(Entry::session)
    }

    pub fn project(&self, id: i64) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// `project/name` of a session, for messages.
    pub fn session_label(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| id.to_string(), Session::label)
    }

    /// Selects `key` once it shows up (after the next reload).
    pub fn select(&mut self, key: EntryKey) {
        self.select_next = Some(key);
    }

    /// Selects project `name` in the projects view once it shows up.
    pub fn select_project(&mut self, name: &str) {
        self.select_project = Some(name.to_string());
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            let keep = self.selected_key();
            // The projects view starts on the selection's project.
            if mode == Mode::Projects
                && let Some(p) = self.selected_project()
            {
                self.project_row = self.projects.iter().position(|q| q.id == p.id).unwrap_or(0);
            }
            // The usage view asks again, unless it just did.
            if mode == Mode::Usage {
                self.limits.want(false);
                self.usage_scroll = 0;
            }
            self.mode = mode;
            self.report_scroll.set(0);
            (self.report_cursor, self.report_selected, self.report_anchor) = (0, None, None);
            if self.select_next.is_none() {
                // Back from another view: where we were.
                self.select_next = keep;
            }
            self.rebuild();
        }
    }

    pub fn set_report(&mut self, diagnosis: crate::doctor::Diagnosis) {
        use crate::doctor::What;
        self.report = diagnosis.lines;
        self.orphans = diagnosis.orphans.len();
        self.problems = diagnosis.problems;
        let there: HashSet<What> = (self.report.iter())
            .filter_map(|l| Some(l.item.as_ref()?.what.clone()))
            .collect();
        self.report_open
            .retain(|id| there.contains(&What::Session(id.clone())));
        self.report_folded.retain(|what| there.contains(what));
        // A process that ended: its session selected instead.
        self.report_selected = match self.report_selected.take() {
            Some(what) if there.contains(&what) => Some(what),
            Some(What::Process { session, .. }) => {
                Some(What::Session(session)).filter(|s| there.contains(s))
            }
            _ => None,
        };
        self.place_report_cursor();
    }

    /// The cursor on the selected session or process, wherever its line is
    /// now; on another line, on one with its text, else as far from the end
    /// as it was (the lines above change as processes come and go).
    pub(super) fn place_report_cursor(&mut self) {
        let lines = self.report_lines();
        let last = lines.len().saturating_sub(1);
        let near = |i: &usize| i.abs_diff(self.report_cursor);
        let at = match (&self.report_selected, &self.report_anchor) {
            (Some(what), _) => {
                (lines.iter()).position(|l| l.item.as_ref().is_some_and(|i| i.what == *what))
            }
            // Its text where it's nearest; gone, as far from the end.
            (None, Some((text, from_end))) => ((0..lines.len())
                .filter(|&i| lines[i].text == *text))
            .min_by_key(near)
            .or(Some(last.saturating_sub(*from_end))),
            (None, None) => None,
        };
        let mut at = at.unwrap_or(self.report_cursor).min(last);
        // Never on a blank line: the next with something on it, else the
        // one before.
        if lines.get(at).is_some_and(|l| l.text.is_empty()) {
            let below = (at..lines.len()).find(|&i| !lines[i].text.is_empty());
            at = below
                .or((0..at).rev().find(|&i| !lines[i].text.is_empty()))
                .unwrap_or(at);
        }
        let what = lines
            .get(at)
            .and_then(|l| Some(l.item.as_ref()?.what.clone()));
        let anchor = match what {
            Some(_) => None,
            None => lines.get(at).map(|l| (l.text.clone(), last - at)),
        };
        (self.report_cursor, self.report_selected, self.report_anchor) = (at, what, anchor);
    }

    /// Above how much memory a session's processes use too much; changed,
    /// none marked under the old limit, and read again at once.
    pub fn set_memory_limit(&mut self, limit: Option<u64>) {
        if limit != self.memory_limit {
            self.memory_marked.clear();
            self.resources_read = None;
        }
        self.memory_limit = limit;
    }

    /// What the sessions' processes use now; those over the limit marked,
    /// and those marked before kept so till well under it (a build hovering
    /// at it doesn't flap).
    pub fn set_resources(&mut self, resources: HashMap<String, crate::procs::Totals>) {
        let limit = self.memory_limit.unwrap_or(u64::MAX);
        let marked = std::mem::take(&mut self.memory_marked);
        self.memory_marked = (resources.iter())
            .filter(|(id, t)| {
                t.memory > limit || (marked.contains(*id) && t.memory > limit / 10 * 9)
            })
            .map(|(id, _)| id.clone())
            .collect();
        self.resources = resources;
    }

    /// How much memory `s`'s processes use, if marked as more than they
    /// should.
    pub fn over_memory(&self, s: &Session) -> Option<u64> {
        self.memory_limit?;
        let memory = self.resources.get(&s.id)?.memory;
        self.memory_marked.contains(&s.id).then_some(memory)
    }

    /// The report's lines as the server view shows them: a session's
    /// processes only while it's unfolded, a process's below it only while
    /// it isn't folded.
    pub fn report_lines(&self) -> Vec<&crate::doctor::Line> {
        use crate::doctor::What;
        let mut shown = Vec::new();
        // Lines of items deeper than this are folded away.
        let mut folded_below = None;
        for line in &self.report {
            let Some(item) = &line.item else {
                folded_below = None;
                shown.push(line);
                continue;
            };
            if folded_below.is_some_and(|depth| item.depth > depth) {
                continue;
            }
            let folded = match &item.what {
                What::Session(id) => !self.report_open.contains(id),
                process => self.report_folded.contains(process),
            };
            folded_below = (item.children && folded).then_some(item.depth);
            shown.push(line);
        }
        shown
    }

    /// `fail`, after an error you haven't seen yet if one is showing, rather
    /// than over it: for errors that come on their own (a title not made).
    pub fn fail_too(&mut self, text: impl Into<String>) {
        let text = text.into();
        match self.message.as_ref().filter(|m| m.error) {
            Some(m) => {
                let both = format!("{} · {text}", m.text);
                self.fail(both);
            }
            None => self.fail(text),
        }
    }

    /// `flash`, but after an error you haven't seen yet if one is showing,
    /// rather than over it: for news that comes on its own (a title made),
    /// not in answer to a key.
    pub fn notice(&mut self, text: impl Into<String>) {
        match self.message.as_ref().is_some_and(|m| m.error) {
            true => self.fail_too(text),
            false => self.flash(text),
        }
    }

    pub fn flash(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
            at: Instant::now(),
        });
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
            at: Instant::now(),
        });
    }

    /// The current message: an error until the next key press, anything else
    /// while it's recent.
    pub fn message(&self) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|m| m.error || m.at.elapsed() < MESSAGE_TTL)
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Option<Op> {
        if self.message.as_ref().is_some_and(|m| m.error) {
            self.message = None;
        }
        if let Some(capture) = self.capturing.take() {
            return self.on_captured_key(capture, &e);
        }
        if self.dialog.is_some() {
            return self.on_dialog_key(e);
        }
        if self.note.is_some() {
            return self.on_note_key(e);
        }
        if self.typing.is_some() {
            self.on_search_key(e);
            return None;
        }
        if self.help {
            self.help = false;
            return None;
        }
        if let Some(menu) = self.menu.take() {
            // Anything that isn't one of the menu's keys just closes it.
            return self.keymap.menu(menu, &e).and_then(|a| self.act(a));
        }
        if self.mode == Mode::Settings
            && let Some(op) = self.on_settings_key(&e)
        {
            return op;
        }
        if self.mode != Mode::Sessions && e.code == KeyCode::Esc {
            self.set_mode(Mode::Sessions);
            return None;
        }
        if e.code == KeyCode::Esc && !self.search.is_empty() {
            self.set_search(String::new());
            return None;
        }
        // The projects view is about projects, the server view about
        // sessions: their menu's keys work there without opening it
        // (top-level keys win).
        let action = match self.mode {
            Mode::Projects => self.keymap.in_view(Menu::Project, &e)?,
            Mode::Server => self.keymap.in_view(Menu::Session, &e)?,
            _ => self.keymap.top(&e)?,
        };
        self.act(action)
    }

    /// The note editor's keys; what it asks for.
    fn on_note_key(&mut self, e: KeyEvent) -> Option<Op> {
        let open = self.note.as_mut()?;
        let save = |editor: &NoteEditor, force| op::NoteSave {
            whose: editor.whose.clone(),
            label: editor.label.clone(),
            text: editor.text(),
            pin: editor.pin,
            base: editor.base.clone(),
            force,
        };
        // As vim, closing leaves no changes in a note not shown.
        let unsaved = |open: &OpenNotes| {
            let (key, whose) = match open.side() {
                Side::Session => ("[", "project's"),
                Side::Project => ("]", "session's"),
            };
            match open.other().is_some_and(NoteEditor::on_version) {
                true => format!(
                    "The {whose} note shows an older version: {key} shows it, :wqa brings it back \
                     and saves this one, :qa! leaves both as saved"
                ),
                false => format!(
                    "The {whose} note has unsaved changes: {key} shows it, :wqa saves both, :qa! \
                     leaves them"
                ),
            }
        };
        let other_dirty = open.other().is_some_and(|e| e.dirty());
        // Markers left in are saved only on purpose.
        let conflicts = |editors: &[&NoteEditor]| {
            editors.iter().find(|e| e.conflicts() > 0).map(|e| {
                let n = e.conflicts();
                let s = if n == 1 { "" } else { "s" };
                format!(
                    "The note of {} has {n} conflict{s}: remove the markers, or :w! saves it as it is",
                    e.label
                )
            })
        };
        match open.shown_mut().on_key(e) {
            note::Outcome::None => None,
            note::Outcome::Save { all, force, .. }
                if !force
                    && let Some(why) = conflicts(&match all {
                        false => vec![open.shown()],
                        true => open.dirty(),
                    }) =>
            {
                self.fail(why);
                None
            }
            note::Outcome::Save {
                all: false,
                close,
                force,
            } => {
                // The shown note is saved; the editor stays open while the
                // other has changes.
                let saves = vec![save(open.shown(), force)];
                let unsaved = (close && other_dirty).then(|| unsaved(open));
                Some(Op::SaveNotes {
                    saves,
                    close: close && !other_dirty,
                    unsaved,
                })
            }
            note::Outcome::Save {
                all: true,
                close,
                force,
            } => {
                let saves: Vec<op::NoteSave> =
                    open.dirty().into_iter().map(|e| save(e, force)).collect();
                match (saves.is_empty(), close) {
                    (false, _) => {
                        return Some(Op::SaveNotes {
                            saves,
                            close,
                            unsaved: None,
                        });
                    }
                    (true, true) => self.note = None,
                    (true, false) => self.flash("Nothing new to save"),
                }
                None
            }
            note::Outcome::Close { all } => {
                if !all && other_dirty {
                    let why = unsaved(open);
                    self.fail(why);
                } else {
                    self.note = None;
                }
                None
            }
            note::Outcome::Show(side) => {
                self.show_note(side);
                None
            }
            note::Outcome::External => Some(Op::EditNote(open.shown().whose.clone())),
            note::Outcome::Reload => Some(Op::ReloadNote(open.shown().whose.clone())),
            note::Outcome::Step(step) => Some(Op::NoteVersion {
                whose: open.shown().whose.clone(),
                step,
            }),
            note::Outcome::Fail(why) => {
                self.fail(why);
                None
            }
            note::Outcome::Notice(what) => {
                self.flash(what);
                None
            }
        }
    }

    /// `[` or `]` in the editor: the project's note or the session's, opened
    /// from what roster last read if it isn't open yet.
    fn show_note(&mut self, side: Side) {
        let Some(open) = &self.note else {
            return;
        };
        let noted = match side {
            _ if open.get(side).is_some() => None,
            Side::Session => match &open.session {
                Some(id) => self
                    .sessions
                    .iter()
                    .find(|s| s.id == *id)
                    .map(Session::noted),
                None => {
                    self.fail("Opened on a project: there's no session's note here");
                    return;
                }
            },
            Side::Project => self.project(open.project).map(Project::noted),
        };
        let escape = self.notes.escape();
        let Some(open) = self.note.as_mut() else {
            return;
        };
        match noted {
            Some(noted) => open.show(side, || NoteEditor::new(&noted, escape)),
            None if open.get(side).is_some() => open.show(side, || unreachable!()),
            None => self.fail("It's gone meanwhile"),
        }
    }

    /// The folded groups: while filtering by text, its own.
    fn folds(&self) -> &HashSet<Group> {
        if self.hits.is_some() {
            &self.search_folded
        } else {
            &self.folded
        }
    }

    /// Typing the text filter: Enter keeps it, Esc puts back what it was.
    fn on_search_key(&mut self, e: KeyEvent) {
        if let Some(down) = input::vertical(e) {
            self.selected = wrap(self.selected, self.entries.len(), down);
            return;
        }
        if input::cancels(e) {
            return self.cancel_search();
        }
        let mut text = self.search.clone();
        if e.code == KeyCode::Enter {
            self.typing = None;
            if text.trim().is_empty() {
                text.clear();
            }
        } else {
            input::edit_line(&mut text, e);
        }
        if text != self.search {
            self.set_search(text);
        }
    }

    fn cancel_search(&mut self) {
        if let Some((text, selected)) = self.typing.take() {
            self.search = text;
            self.select_next = selected;
            self.rebuild();
        }
    }

    /// Filters the list by `text`, every group open, the cursor on the best
    /// match.
    fn set_search(&mut self, text: String) {
        self.search = text;
        self.search_folded.clear();
        self.rebuild();
        let Some(hits) = &self.hits else { return };
        let mut best: Option<(u32, usize)> = None;
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(hit) = entry.session().and_then(|s| hits.get(&s.id))
                && best.is_none_or(|(score, _)| hit.score > score)
            {
                best = Some((hit.score, i));
            }
        }
        if let Some((_, i)) = best {
            self.selected = i;
        }
    }
}

/// The next (or previous) of `len` rows after `row`, past the end back to the
/// start and the other way round.
pub(super) fn wrap(row: usize, len: usize, down: bool) -> usize {
    match (len, down) {
        (0, _) => 0,
        (_, true) => (row + 1) % len,
        (_, false) => (row + len - 1) % len,
    }
}

#[cfg(test)]
mod tests;
