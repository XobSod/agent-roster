//! TUI state and key handling. No terminal or tmux calls here: keys become `Op`s
//! that the run loop carries out, so this part is testable on its own.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Text;

mod act;
mod dialog;
mod settings;
mod texts;

pub use dialog::Dialog;
use dialog::Purpose;

use super::form::{self, Form, Outcome};
use super::input;
use super::list::{self, Entry, EntryKey, Group, Shelf};
use super::op::Op;
use super::picker::{Picked, Picker};
use crate::keys::{Action, Keymap, Menu};
use crate::model::{Project, Session, State};
use crate::paths::PROJECT_ENV;
use crate::search::Hit;

const MESSAGE_TTL: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The session list, by shelf or by project.
    Sessions,
    /// Every project, to add, edit, remove, or pick one.
    Projects,
    /// The tmux server and database report.
    Server,
    /// Every setting, to see and change.
    Settings,
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
    /// What the selected session's agent can tell about it.
    pub stats: Option<crate::stats::AgentStats>,
    /// Transcript readers by session, kept so each read only takes what's new.
    pub transcripts: HashMap<String, Box<dyn crate::stats::Transcript>>,
    /// Its size when last drawn; agents drawing full screen get it.
    pub size: Cell<Option<(u16, u16)>>,
    /// Agents fitted to `fitted_size` (see `Roster::fit_screens`).
    pub fitted: HashSet<String>,
    pub fitted_size: Option<(u16, u16)>,
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
    /// The server view's report, its orphan count, and how far it's scrolled.
    pub report: Vec<crate::doctor::Line>,
    pub orphans: usize,
    pub problems: usize,
    pub report_scroll: usize,
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
    pub listing: Option<std::sync::mpsc::Receiver<HashMap<String, Vec<crate::agent::Model>>>>,
    /// The settings view's settings, and the one selected (of all, in order).
    pub settings: Vec<crate::settings::Section>,
    pub setting_row: usize,
    /// Where config.toml is, to say so.
    pub config_file: std::path::PathBuf,
    /// A key setting waiting for its key to be pressed.
    pub capturing: Option<settings::Capture>,
    /// Oldest or newest first, per shelf (config.toml).
    pub order: crate::config::Order,
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
            report_scroll: 0,
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
        }
    }

    /// Whether starting a session of `agent` again resumes its conversation.
    /// The agents that can be started, after a change of settings.
    pub fn set_agents(&mut self, agents: Vec<String>) {
        self.agents = agents;
    }

    /// The agent new sessions get unless their project picks one: the first
    /// of those that can be started (`agent::available`).
    pub fn default_agent(&self) -> &str {
        self.agents.first().map_or("", String::as_str)
    }

    pub fn resumes(&self, agent: &str) -> bool {
        crate::agent::adapter(agent).resumes()
    }

    /// A form in a dialog, with what model choices turned out to be.
    pub(super) fn open_form(&mut self, form: Form) {
        self.dialog = Some(Dialog::Form(form.with_models(&self.models)));
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
        self.rebuild();
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
        let opts = list::Options {
            grouped: self.grouped,
            folded: self.folds(),
            filter: self.filter,
            hits: self.hits.as_ref(),
            order: &self.order,
        };
        self.entries = list::entries(&self.projects, &self.sessions, &opts);
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
            self.mode = mode;
            self.report_scroll = 0;
            if self.select_next.is_none() {
                // Back from another view: where we were.
                self.select_next = keep;
            }
            self.rebuild();
        }
    }

    pub fn set_report(&mut self, diagnosis: crate::doctor::Diagnosis) {
        self.report = diagnosis.lines;
        self.orphans = diagnosis.orphans.len();
        self.problems = diagnosis.problems;
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
        // The projects view is about projects: the project menu's keys work
        // without opening it (top-level keys win).
        let action = if self.mode == Mode::Projects {
            self.keymap.in_view(Menu::Project, &e)?
        } else {
            self.keymap.top(&e)?
        };
        self.act(action)
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
fn wrap(row: usize, len: usize, down: bool) -> usize {
    match (len, down) {
        (0, _) => 0,
        (_, true) => (row + 1) % len,
        (_, false) => (row + len - 1) % len,
    }
}

#[cfg(test)]
mod tests;
