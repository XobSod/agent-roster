//! TUI state and key handling. No terminal or tmux calls here: keys become `Op`s
//! that the run loop carries out, so this part is testable on its own.

use std::cell::Cell;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Text;

use super::form::{Form, Outcome};
use super::list::{self, Entry, EntryKey, Group, Shelf};
use crate::keys::{Action, Keymap, Menu};
use crate::model::{Project, Session, State};
use crate::paths::PROJECT_ENV;

const MESSAGE_TTL: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    Deleted,
    /// The tmux server and database report.
    Server,
}

pub enum Dialog {
    Confirm {
        question: String,
        op: Op,
    },
    Input {
        prompt: String,
        value: String,
        purpose: Purpose,
    },
    Form(Form),
}

#[derive(Clone, Debug)]
pub enum Purpose {
    RestoreName { id: String },
}

/// Work for the run loop, which owns the terminal and the roster.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Attach(String),
    Start(String),
    Stop(String),
    Restart(String),
    Delete(String),
    Restore {
        id: String,
        name: Option<String>,
    },
    SetUnread(String, bool),
    NewSession {
        project: String,
        agent: String,
        name: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        prompt: Option<String>,
    },
    EditSession {
        id: String,
        name: String,
        model: Option<String>,
        effort: Option<String>,
    },
    AddProject {
        name: String,
        path: String,
        agent: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    EditProject {
        id: i64,
        name: String,
        path: String,
        agent: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    RemoveProject(String),
    KillOrphans,
    Quit,
}

pub struct Message {
    pub text: String,
    pub error: bool,
    at: Instant,
}

pub struct App {
    pub keymap: Keymap,
    pub mode: Mode,
    /// Grouped by project instead of shelves.
    pub grouped: bool,
    /// Showing only this project's sessions.
    pub filter: Option<i64>,
    folded: HashSet<Group>,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// First visible list row; the drawing code keeps the selection in view.
    pub scroll: Cell<usize>,
    /// The menu whose popup is open.
    pub menu: Option<Menu>,
    pub help: bool,
    pub dialog: Option<Dialog>,
    /// The selected session's screen, or information about the selection.
    pub preview: Text<'static>,
    /// The server view's report, its orphan count, and how far it's scrolled.
    pub report: Vec<crate::doctor::Line>,
    pub orphans: usize,
    pub problems: usize,
    pub report_scroll: usize,
    message: Option<Message>,
    pub projects: Vec<Project>,
    /// Live and deleted.
    pub sessions: Vec<Session>,
    select_next: Option<EntryKey>,
    /// Suggested origin for a new project.
    default_path: String,
    /// Agents that can be started, and whether each resumes its conversations.
    agents: Vec<(String, bool)>,
    /// What each model alias last turned out to be, for the forms.
    pub model_names: std::collections::HashMap<String, String>,
}

impl App {
    pub fn new(keymap: Keymap, default_path: String, agents: Vec<(String, bool)>) -> App {
        App {
            keymap,
            mode: Mode::Live,
            grouped: false,
            filter: None,
            folded: HashSet::from([Group::Shelf(Shelf::Stopped)]),
            entries: Vec::new(),
            selected: 0,
            scroll: Cell::new(0),
            menu: None,
            help: false,
            dialog: None,
            preview: Text::default(),
            report: Vec::new(),
            orphans: 0,
            problems: 0,
            report_scroll: 0,
            message: None,
            projects: Vec::new(),
            sessions: Vec::new(),
            select_next: None,
            default_path,
            agents,
            model_names: Default::default(),
        }
    }

    /// Whether starting a session of `agent` again resumes its conversation.
    pub fn resumes(&self, agent: &str) -> bool {
        self.agents
            .iter()
            .any(|(name, resumes)| name == agent && *resumes)
    }

    /// Replaces the data, keeping the selection on the same entry.
    pub fn set_data(&mut self, projects: Vec<Project>, sessions: Vec<Session>) {
        self.projects = projects;
        self.sessions = sessions;
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
        self.entries = match self.mode {
            Mode::Live => {
                let opts = list::Options {
                    grouped: self.grouped,
                    folded: &self.folded,
                    filter: self.filter,
                };
                list::live(&self.projects, &self.sessions, &opts)
            }
            Mode::Deleted => list::deleted(&self.sessions),
            // The list isn't shown; keep the live one for coming back.
            Mode::Server => {
                let opts = list::Options {
                    grouped: self.grouped,
                    folded: &self.folded,
                    filter: self.filter,
                };
                list::live(&self.projects, &self.sessions, &opts)
            }
        };
        let position = |key: &EntryKey| self.entries.iter().position(|e| e.key() == *key);
        // A selected session that moved into a folded group: select that group.
        let found = keep.as_ref().and_then(|k| {
            position(k).or_else(|| match k {
                EntryKey::Session(id) if self.mode == Mode::Live => {
                    let s = self
                        .sessions
                        .iter()
                        .find(|s| s.id == *id && s.deleted_at.is_none())?;
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

    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            let keep = self.selected_key();
            self.mode = mode;
            self.report_scroll = 0;
            if mode == Mode::Deleted {
                self.selected = 0;
            } else if self.select_next.is_none() {
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

    /// The current message, if it's recent.
    pub fn message(&self) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|m| m.at.elapsed() < MESSAGE_TTL)
    }

    /// Footer hints: keys that act here and now.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        let actions: Vec<(Action, &'static str)> = match self.mode {
            Mode::Live => {
                let mut actions = vec![(Action::Confirm, "attach")];
                actions.extend(Menu::ALL.map(|m| (Action::Open(m), m.name())));
                actions
            }
            Mode::Deleted => vec![(Action::Confirm, "restore")],
            Mode::Server if self.orphans > 0 => {
                vec![(Action::Confirm, "kill orphans"), (Action::Down, "scroll")]
            }
            Mode::Server => vec![(Action::Down, "scroll")],
        };
        let mut hints: Vec<(String, &'static str)> = actions
            .into_iter()
            .filter_map(|(a, what)| Some((self.keymap.label(a)?.to_string(), what)))
            .collect();
        if self.mode != Mode::Live {
            hints.push(("Esc".into(), "back"));
        }
        for (a, what) in [(Action::Help, "help"), (Action::Quit, "quit")] {
            if let Some(key) = self.keymap.label(a) {
                hints.push((key.to_string(), what));
            }
        }
        hints
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Option<Op> {
        if self.dialog.is_some() {
            return self.on_dialog_key(e);
        }
        if self.help {
            self.help = false;
            return None;
        }
        if let Some(menu) = self.menu.take() {
            // Anything that isn't one of the menu's keys just closes it.
            return self.keymap.menu(menu, &e).and_then(|a| self.act(a));
        }
        if self.mode != Mode::Live && e.code == KeyCode::Esc {
            self.set_mode(Mode::Live);
            return None;
        }
        let action = self.keymap.top(&e)?;
        self.act(action)
    }

    fn act(&mut self, action: Action) -> Option<Op> {
        if self.mode == Mode::Server {
            return self.act_in_server_view(action);
        }
        match action {
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down => {
                if self.selected + 1 < self.entries.len() {
                    self.selected += 1;
                }
            }
            Action::Collapse => self.fold(true),
            Action::Expand => self.fold(false),
            Action::Confirm => return self.confirm_selected(),
            Action::Quit => return Some(Op::Quit),
            Action::Help => self.help = true,
            Action::Open(menu) => self.menu = Some(menu),
            Action::ToggleGroup => {
                self.grouped = !self.grouped;
                self.set_mode(Mode::Live);
                self.rebuild();
            }
            Action::ShowDeleted => {
                self.set_mode(if self.mode == Mode::Live {
                    Mode::Deleted
                } else {
                    Mode::Live
                });
            }
            Action::ShowServer => self.set_mode(Mode::Server),
            _ if self.mode == Mode::Deleted => {
                let restore = self.keymap.label(Action::Confirm).unwrap_or("?");
                self.fail(format!(
                    "These are deleted sessions: {restore} restores one, Esc goes back"
                ));
            }
            Action::NewProject => {
                let agents = self.agent_names();
                self.dialog = Some(Dialog::Form(
                    Form::new_project(&self.default_path, &agents)
                        .with_model_names(&self.model_names),
                ));
            }
            Action::EditProject => match self.selected_project() {
                Some(p) => {
                    self.dialog = Some(Dialog::Form(
                        Form::edit_project(&p, &self.agent_names())
                            .with_model_names(&self.model_names),
                    ))
                }
                None => {
                    self.fail("Select a project (group by project) or one of its sessions first")
                }
            },
            Action::EditSession => match self.selected_session().cloned() {
                Some(s) => {
                    self.dialog = Some(Dialog::Form(
                        Form::edit_session(&s).with_model_names(&self.model_names),
                    ))
                }
                None => self.fail("Select a session first"),
            },
            Action::FilterProject => self.toggle_filter(),
            Action::RemoveProject => self.remove_project(),
            Action::NewSession => self.new_session(),
            Action::Start
            | Action::Stop
            | Action::Restart
            | Action::Delete
            | Action::ToggleRead => {
                return self.session_action(action);
            }
        }
        None
    }

    /// In the server view: scroll, kill orphans, or leave.
    fn act_in_server_view(&mut self, action: Action) -> Option<Op> {
        match action {
            Action::Up => self.report_scroll = self.report_scroll.saturating_sub(1),
            Action::Down => {
                self.report_scroll =
                    (self.report_scroll + 1).min(self.report.len().saturating_sub(1))
            }
            Action::Confirm if self.orphans > 0 => {
                let question = format!(
                    "Kill {} orphaned tmux session(s)? Their programs end.",
                    self.orphans
                );
                self.confirm(question, Op::KillOrphans);
            }
            Action::Confirm => self.flash("No orphaned tmux sessions to remove"),
            Action::Quit => return Some(Op::Quit),
            Action::Help => self.help = true,
            Action::Open(menu) => self.menu = Some(menu),
            Action::ShowServer => self.set_mode(Mode::Live),
            Action::ShowDeleted => self.set_mode(Mode::Deleted),
            Action::ToggleGroup => {
                self.grouped = !self.grouped;
                self.set_mode(Mode::Live);
            }
            _ => self.fail("This is the server view: Esc goes back to the sessions"),
        }
        None
    }

    /// Folds (or unfolds) the selected group; from a session, its group.
    fn fold(&mut self, fold: bool) {
        let group = match self.selected_entry() {
            Some(Entry::Heading { group, .. }) => *group,
            Some(Entry::Card(s) | Entry::Row(s)) if fold && self.mode == Mode::Live => {
                list::group_of(s, self.grouped)
            }
            _ => return,
        };
        let changed = if fold {
            self.folded.insert(group)
        } else {
            self.folded.remove(&group)
        };
        if changed {
            self.select_next = Some(EntryKey::Group(group));
            self.rebuild();
        }
    }

    fn confirm_selected(&mut self) -> Option<Op> {
        match self.selected_entry().cloned()? {
            Entry::Heading { open, .. } => {
                self.fold(open);
                None
            }
            Entry::Card(s) | Entry::Row(s) if self.mode == Mode::Deleted => self.restore(s),
            Entry::Card(s) | Entry::Row(s) => {
                // Starting an agent that can't resume loses its conversation: ask.
                if s.state == State::Stopped && !self.resumes(&s.agent) {
                    self.confirm(self.new_conversation_question(&s), Op::Attach(s.id));
                    return None;
                }
                Some(Op::Attach(s.id))
            }
        }
    }

    fn session_action(&mut self, action: Action) -> Option<Op> {
        let Some(s) = self.selected_session().cloned() else {
            self.fail("Select a session first");
            return None;
        };
        match action {
            Action::Start | Action::Restart if !self.resumes(&s.agent) => {
                let op = if action == Action::Start {
                    Op::Start(s.id.clone())
                } else {
                    Op::Restart(s.id.clone())
                };
                self.confirm(self.new_conversation_question(&s), op);
                None
            }
            Action::Start => Some(Op::Start(s.id)),
            Action::Stop => Some(Op::Stop(s.id)),
            Action::Restart if s.state == State::Working => {
                self.confirm(
                    format!("{} is working. Restart it?", s.label()),
                    Op::Restart(s.id),
                );
                None
            }
            Action::Restart => Some(Op::Restart(s.id)),
            Action::Delete => {
                let question = format!(
                    "Delete {}? It can be restored from the deleted sessions.",
                    s.label()
                );
                self.confirm(question, Op::Delete(s.id));
                None
            }
            Action::ToggleRead => Some(Op::SetUnread(s.id, !s.unread)),
            _ => None,
        }
    }

    /// The project the selection belongs to; with only one project, that one.
    fn selected_project(&self) -> Option<Project> {
        let id = match self.selected_entry() {
            Some(Entry::Heading {
                group: Group::Project(id),
                ..
            }) => Some(*id),
            Some(Entry::Card(s) | Entry::Row(s)) => Some(s.project_id),
            _ => self.filter,
        };
        match id {
            Some(id) => self.project(id).cloned(),
            None if self.projects.len() == 1 => self.projects.first().cloned(),
            None => None,
        }
    }

    fn new_session(&mut self) {
        if self.projects.is_empty() {
            let key = self.chord(Menu::Project, Action::NewProject);
            self.fail(format!("Add a project first ({key})"));
            return;
        }
        let selected = self.selected_project();
        let form = Form::new_session(&self.projects, selected.as_ref(), &self.agent_names())
            .with_model_names(&self.model_names);
        self.dialog = Some(Dialog::Form(form));
    }

    fn agent_names(&self) -> Vec<String> {
        self.agents.iter().map(|(name, _)| name.clone()).collect()
    }

    fn toggle_filter(&mut self) {
        if self.filter.is_some() {
            self.filter = None;
        } else if let Some(p) = self.selected_project() {
            self.filter = Some(p.id);
        } else {
            self.fail("Select a session or a project first");
            return;
        }
        self.rebuild();
    }

    fn remove_project(&mut self) {
        let Some(p) = self.selected_project() else {
            self.fail("Select a project (group by project) or one of its sessions first");
            return;
        };
        if self
            .sessions
            .iter()
            .any(|s| s.project_id == p.id && s.deleted_at.is_none())
        {
            self.fail(format!("{} still has sessions; delete them first", p.name));
        } else {
            self.confirm(
                format!("Remove project {}?", p.name),
                Op::RemoveProject(p.name),
            );
        }
    }

    fn restore(&mut self, s: Session) -> Option<Op> {
        let taken = self
            .sessions
            .iter()
            .any(|o| o.deleted_at.is_none() && o.project_id == s.project_id && o.name == s.name);
        if taken {
            let prompt = format!(
                "{} already has a session named {}. Restore as:",
                s.project, s.name
            );
            self.input(&prompt, String::new(), Purpose::RestoreName { id: s.id });
            return None;
        }
        Some(Op::Restore {
            id: s.id,
            name: None,
        })
    }

    /// "s n"-style text for a menu key, for hints in messages.
    pub fn chord(&self, menu: Menu, action: Action) -> String {
        let open = self.keymap.label(Action::Open(menu)).unwrap_or("?");
        let key = self.keymap.label(action).unwrap_or("?");
        format!("{open} {key}")
    }

    fn new_conversation_question(&self, s: &Session) -> String {
        format!(
            "{} ({}) can't resume its conversation. Start a new one?",
            s.label(),
            s.agent
        )
    }

    fn on_dialog_key(&mut self, e: KeyEvent) -> Option<Op> {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let alt = e.modifiers.contains(KeyModifiers::ALT);
        match self.dialog.take()? {
            Dialog::Form(mut form) => match form.on_key(e) {
                Outcome::Continue => {
                    self.dialog = Some(Dialog::Form(form));
                    None
                }
                Outcome::Cancel => None,
                Outcome::Submit(op) => Some(op),
            },
            Dialog::Confirm { question, op } => match e.code {
                KeyCode::Char('y' | 'Y') => Some(op),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                KeyCode::Char('c') if ctrl => None,
                _ => {
                    self.dialog = Some(Dialog::Confirm { question, op });
                    None
                }
            },
            Dialog::Input {
                prompt,
                mut value,
                purpose,
            } => {
                match e.code {
                    KeyCode::Esc => return None,
                    KeyCode::Char('c') if ctrl => return None,
                    KeyCode::Enter => return self.submit(value, purpose),
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char('u') if ctrl => value.clear(),
                    KeyCode::Char(c) if !ctrl && !alt => value.push(c),
                    _ => {}
                }
                self.dialog = Some(Dialog::Input {
                    prompt,
                    value,
                    purpose,
                });
                None
            }
        }
    }

    fn submit(&mut self, value: String, purpose: Purpose) -> Option<Op> {
        let value = value.trim().to_string();
        match purpose {
            Purpose::RestoreName { .. } if value.is_empty() => None,
            Purpose::RestoreName { id } => Some(Op::Restore {
                id,
                name: Some(value),
            }),
        }
    }

    fn input(&mut self, prompt: &str, value: String, purpose: Purpose) {
        self.dialog = Some(Dialog::Input {
            prompt: prompt.to_string(),
            value,
            purpose,
        });
    }

    fn confirm(&mut self, question: String, op: Op) {
        self.dialog = Some(Dialog::Confirm { question, op });
    }

    /// Preview for a project heading.
    pub fn project_info(&self, p: &Project) -> Text<'static> {
        let live = self
            .sessions
            .iter()
            .filter(|s| s.project_id == p.id && s.deleted_at.is_none())
            .count();
        let deleted = self
            .sessions
            .iter()
            .filter(|s| s.project_id == p.id && s.deleted_at.is_some())
            .count();
        let env = p.origin.join(PROJECT_ENV);
        let env_state = if env.exists() {
            "sourced before every start"
        } else {
            "none"
        };
        Text::from(vec![
            format!("origin    {}", p.origin.display()).into(),
            format!("sessions  {live} (deleted: {deleted})").into(),
            format!("env file  {} ({env_state})", env.display()).into(),
            "".into(),
            format!(
                "Press {} to start a session here.",
                self.chord(Menu::Session, Action::NewSession)
            )
            .into(),
        ])
    }

    /// Preview for a shelf heading.
    pub fn shelf_info(&self, shelf: Shelf, count: usize) -> Text<'static> {
        let what = match shelf {
            Shelf::NeedsYou => {
                "waiting for you: a question, a permission, or a finished turn you haven't seen"
            }
            Shelf::Idle => "idle, waiting for your next prompt",
            Shelf::Working => "working; nothing to do until they finish",
            Shelf::Stopped => "stopped; starting one resumes its conversation where the agent can",
        };
        let fold = self.keymap.label(Action::Collapse).unwrap_or("?");
        let unfold = self.keymap.label(Action::Expand).unwrap_or("?");
        Text::from(vec![
            format!(
                "{count} session{} {what}.",
                if count == 1 { "" } else { "s" }
            )
            .into(),
            "".into(),
            format!("{fold} folds the shelf, {unfold} unfolds it.").into(),
        ])
    }

    /// Preview for a deleted session.
    pub fn deleted_info(&self, s: &Session) -> Text<'static> {
        let restore = self.keymap.label(Action::Confirm).unwrap_or("?");
        Text::from(vec![
            format!(
                "conversation  {}",
                s.conversation_id.as_deref().unwrap_or("none")
            )
            .into(),
            format!("directory     {}", s.workdir.display()).into(),
            "".into(),
            format!("Press {restore} to restore it; starting it then resumes the conversation.")
                .into(),
        ])
    }

    /// Preview when there's nothing to select.
    pub fn empty_info(&self) -> Text<'static> {
        match self.mode {
            Mode::Live if self.projects.is_empty() => Text::from(format!(
                "No projects yet. Press {} to add one: a name and its directory.",
                self.chord(Menu::Project, Action::NewProject)
            )),
            Mode::Live => Text::from(format!(
                "No sessions yet. Press {} to start one.",
                self.chord(Menu::Session, Action::NewSession)
            )),
            Mode::Deleted => Text::from("No deleted sessions."),
            Mode::Server => Text::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Keys;
    use crate::model::tests::session;

    fn project(id: i64, name: &str) -> Project {
        Project {
            id,
            name: name.into(),
            origin: format!("/{name}").into(),
            agent: None,
            model: None,
            effort: None,
        }
    }

    fn with(id: &str, project: (i64, &str), state: State, unread: bool, since: i64) -> Session {
        let mut s = session(id);
        s.project_id = project.0;
        s.project = project.1.into();
        s.state = state;
        s.unread = unread;
        s.state_since = Some(since);
        s
    }

    /// alpha: an idle session; beta: an unread one and a working one; one deleted.
    fn app() -> App {
        let agents = vec![("claude".to_string(), true)];
        let mut app = App::new(
            Keymap::new(&Keys::default()).unwrap(),
            "/work".into(),
            agents,
        );
        let mut gone = with("s3", (1, "alpha"), State::Stopped, false, 1);
        gone.deleted_at = Some(5);
        app.set_data(
            vec![project(1, "alpha"), project(2, "beta")],
            vec![
                with("s1", (1, "alpha"), State::Idle, false, 10),
                with("s2", (2, "beta"), State::Idle, true, 20),
                with("s4", (2, "beta"), State::Working, false, 30),
                gone,
            ],
        );
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn chr(c: char) -> KeyEvent {
        let shift = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        KeyEvent::new(KeyCode::Char(c), shift)
    }

    fn keys(app: &mut App, keys: &str) -> Option<Op> {
        let mut op = None;
        for c in keys.chars() {
            op = app.on_key(chr(c));
        }
        op
    }

    fn select(app: &mut App, id: &str) {
        app.selected = app
            .entries
            .iter()
            .position(|e| e.key() == EntryKey::Session(id.into()))
            .unwrap();
    }

    fn shown(app: &App) -> Vec<EntryKey> {
        app.entries.iter().map(Entry::key).collect()
    }

    #[test]
    fn starts_on_shelves_with_stopped_folded() {
        let app = app();
        assert_eq!(
            shown(&app),
            [
                EntryKey::Group(Group::Shelf(Shelf::NeedsYou)),
                EntryKey::Session("s2".into()),
                EntryKey::Group(Group::Shelf(Shelf::Idle)),
                EntryKey::Session("s1".into()),
                EntryKey::Group(Group::Shelf(Shelf::Working)),
                EntryKey::Session("s4".into()),
            ]
        );
    }

    #[test]
    fn enter_attaches_and_h_l_fold() {
        let mut app = app();
        // The first session is selected at first, not the heading above it.
        assert_eq!(app.selected_session().unwrap().id, "s2");
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::Attach("s2".into()))
        );
        // h on a session folds its shelf and selects the heading; l unfolds.
        keys(&mut app, "h");
        assert_eq!(
            app.selected_key(),
            Some(EntryKey::Group(Group::Shelf(Shelf::NeedsYou)))
        );
        assert!(!shown(&app).contains(&EntryKey::Session("s2".into())));
        keys(&mut app, "l");
        assert!(shown(&app).contains(&EntryKey::Session("s2".into())));
        // Enter on a heading folds or unfolds it too.
        app.on_key(key(KeyCode::Enter));
        assert!(!shown(&app).contains(&EntryKey::Session("s2".into())));
    }

    #[test]
    fn selection_follows_a_session_to_its_new_shelf() {
        let mut app = app();
        select(&mut app, "s4");
        // s4 finishes its turn: it moves to "Needs you" and stays selected.
        let mut sessions = app.sessions.clone();
        let s4 = sessions.iter_mut().find(|s| s.id == "s4").unwrap();
        s4.state = State::Idle;
        s4.unread = true;
        s4.state_since = Some(40);
        app.set_data(app.projects.clone(), sessions);
        assert_eq!(app.selected_session().unwrap().id, "s4");
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn a_session_moving_into_a_folded_shelf_selects_that_shelf() {
        let mut app = app();
        select(&mut app, "s1");
        let mut sessions = app.sessions.clone();
        sessions.iter_mut().find(|s| s.id == "s1").unwrap().state = State::Stopped;
        app.set_data(app.projects.clone(), sessions);
        assert_eq!(
            app.selected_key(),
            Some(EntryKey::Group(Group::Shelf(Shelf::Stopped)))
        );
        keys(&mut app, "lj");
        assert_eq!(app.selected_session().unwrap().id, "s1");
    }

    #[test]
    fn menus_open_with_their_key_and_reuse_letters() {
        let mut app = app();
        select(&mut app, "s1");
        assert_eq!(keys(&mut app, "s"), None);
        assert_eq!(app.menu, Some(Menu::Session));
        assert_eq!(keys(&mut app, "x"), Some(Op::Stop("s1".into())));
        assert_eq!(app.menu, None);
        assert_eq!(keys(&mut app, "su"), Some(Op::SetUnread("s1".into(), true)));
        // An unbound key closes the menu without doing anything.
        assert_eq!(keys(&mut app, "sz"), None);
        assert_eq!((app.menu, app.dialog.is_none()), (None, true));
        // s n opens the new-session form on the selected session's project.
        keys(&mut app, "sn");
        let Some(Dialog::Form(form)) = &app.dialog else {
            panic!("no form")
        };
        assert_eq!(form.title, "New session");
        assert!(form.fields.iter().any(|f| f.value == "alpha"));
        app.on_key(key(KeyCode::Esc));
        assert!(app.dialog.is_none());
        // p n opens the new-project form; s e and p e the edit forms.
        keys(&mut app, "pn");
        assert!(matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "New project"));
        app.on_key(key(KeyCode::Esc));
        keys(&mut app, "se");
        assert!(
            matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "Edit session alpha/s-s1")
        );
        app.on_key(key(KeyCode::Esc));
        keys(&mut app, "pe");
        assert!(matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "Edit project alpha"));
    }

    #[test]
    fn delete_and_working_restart_ask_first() {
        let mut app = app();
        select(&mut app, "s1");
        assert_eq!(keys(&mut app, "sd"), None);
        assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
        assert_eq!(keys(&mut app, "y"), Some(Op::Delete("s1".into())));
        assert_eq!(keys(&mut app, "sr"), Some(Op::Restart("s1".into())));
        select(&mut app, "s4");
        assert_eq!(keys(&mut app, "sr"), None);
        assert_eq!(keys(&mut app, "y"), Some(Op::Restart("s4".into())));
    }

    #[test]
    fn view_menu_groups_by_project_and_shows_deleted() {
        let mut app = app();
        keys(&mut app, "vp");
        assert!(app.grouped);
        assert_eq!(shown(&app)[0], EntryKey::Group(Group::Project(1)));
        keys(&mut app, "vd");
        assert_eq!(app.mode, Mode::Deleted);
        assert_eq!(shown(&app), [EntryKey::Session("s3".into())]);
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::Restore {
                id: "s3".into(),
                name: None
            })
        );
        keys(&mut app, "sx");
        assert!(app.message().unwrap().error);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Live);
    }

    #[test]
    fn project_filter_and_removal() {
        let mut app = app();
        select(&mut app, "s4");
        keys(&mut app, "pf");
        assert_eq!(app.filter, Some(2));
        assert!(!shown(&app).contains(&EntryKey::Session("s1".into())));
        keys(&mut app, "pf");
        assert_eq!(app.filter, None);
        // beta still has sessions.
        keys(&mut app, "pd");
        assert!(app.dialog.is_none() && app.message().unwrap().error);
    }

    #[test]
    fn agents_that_cant_resume_ask_before_a_new_conversation() {
        let mut app = app();
        app.agents.push(("aider".into(), false));
        let mut generic = with("g1", (1, "alpha"), State::Stopped, false, 1);
        generic.agent = "aider".into();
        let mut sessions = app.sessions.clone();
        sessions.push(generic);
        app.folded.clear();
        app.set_data(app.projects.clone(), sessions);
        select(&mut app, "g1");
        for (chord, op) in [
            ("ss", Op::Start("g1".into())),
            ("sr", Op::Restart("g1".into())),
        ] {
            assert_eq!(keys(&mut app, chord), None);
            let Some(Dialog::Confirm { question, .. }) = &app.dialog else {
                panic!("no confirmation")
            };
            assert!(question.contains("can't resume"), "{question}");
            assert_eq!(keys(&mut app, "y"), Some(op));
        }
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(keys(&mut app, "y"), Some(Op::Attach("g1".into())));
    }

    #[test]
    fn server_view_scrolls_kills_orphans_and_goes_back() {
        use crate::doctor::{Diagnosis, Level, Line};
        let mut app = app();
        select(&mut app, "s1");
        keys(&mut app, "vt");
        assert_eq!(app.mode, Mode::Server);
        let line = |text: &str| Line {
            level: Level::Info,
            text: text.into(),
        };
        app.set_report(Diagnosis {
            lines: vec![line("a"), line("b")],
            orphans: vec!["r-x".into()],
            problems: 1,
        });
        keys(&mut app, "jjj");
        assert_eq!(app.report_scroll, 1);
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
        assert_eq!(keys(&mut app, "y"), Some(Op::KillOrphans));
        keys(&mut app, "sx");
        assert!(app.message().unwrap().error);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Live);
        // The selection is where it was.
        assert_eq!(app.selected_session().unwrap().id, "s1");
    }

    #[test]
    fn help_opens_and_any_key_closes_it() {
        let mut app = app();
        keys(&mut app, "?");
        assert!(app.help);
        assert_eq!(keys(&mut app, "q"), None);
        assert!(!app.help);
        assert_eq!(keys(&mut app, "q"), Some(Op::Quit));
    }

    #[test]
    fn hints_follow_the_keymap() {
        let app = app();
        let hints = app.hints();
        assert_eq!(hints[0], ("Enter".to_string(), "attach"));
        assert!(hints.contains(&("s".to_string(), "session")));
        assert!(hints.contains(&("?".to_string(), "help")));
    }
}
