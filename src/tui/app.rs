//! TUI state and key handling. No terminal or tmux calls here: keys become `Op`s
//! that the run loop carries out, so this part is testable on its own.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Text};

use crate::keys::{Action, Keymap};
use crate::model::{Project, Session, State};
use crate::paths::PROJECT_ENV;

const MESSAGE_TTL: Duration = Duration::from_secs(6);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKey {
    Project(i64),
    Session(String),
}

// A few dozen rows at most, so the size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Row {
    Project(Project),
    Session(Session),
}

impl Row {
    pub fn key(&self) -> RowKey {
        match self {
            Row::Project(p) => RowKey::Project(p.id),
            Row::Session(s) => RowKey::Session(s.id.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Live,
    Deleted,
}

#[derive(Debug)]
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
}

#[derive(Clone, Debug)]
pub enum Purpose {
    ChooseAgent { project: String },
    NewSession { project: String, agent: String },
    ProjectName,
    ProjectPath { name: String },
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
    },
    AddProject {
        name: String,
        path: String,
    },
    RemoveProject(String),
    Quit,
}

pub struct Message {
    pub text: String,
    pub error: bool,
    at: Instant,
}

pub struct App {
    pub keymap: Keymap,
    pub view: View,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub dialog: Option<Dialog>,
    pub preview_title: Line<'static>,
    pub preview: Text<'static>,
    message: Option<Message>,
    projects: Vec<Project>,
    /// Live and deleted.
    sessions: Vec<Session>,
    select_next: Option<RowKey>,
    /// Suggested origin for a new project.
    default_path: String,
    /// Agents that can be started, and whether each resumes its conversations.
    agents: Vec<(String, bool)>,
}

impl App {
    pub fn new(keymap: Keymap, default_path: String, agents: Vec<(String, bool)>) -> App {
        App {
            keymap,
            view: View::Live,
            rows: Vec::new(),
            selected: 0,
            dialog: None,
            preview_title: Line::default(),
            preview: Text::default(),
            message: None,
            projects: Vec::new(),
            sessions: Vec::new(),
            select_next: None,
            default_path,
            agents,
        }
    }

    /// Whether starting a session of `agent` again resumes its conversation.
    pub fn resumes(&self, agent: &str) -> bool {
        self.agents
            .iter()
            .any(|(name, resumes)| name == agent && *resumes)
    }

    /// Replaces the data, keeping the selection on the same project or session.
    pub fn set_data(&mut self, projects: Vec<Project>, sessions: Vec<Session>) {
        self.projects = projects;
        self.sessions = sessions;
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        let keep = self.select_next.take().or_else(|| self.selected_key());
        self.rows = match self.view {
            View::Live => {
                let mut rows = Vec::new();
                for p in &self.projects {
                    rows.push(Row::Project(p.clone()));
                    rows.extend(
                        self.sessions
                            .iter()
                            .filter(|s| s.project_id == p.id && s.deleted_at.is_none())
                            .map(|s| Row::Session(s.clone())),
                    );
                }
                rows
            }
            View::Deleted => {
                let mut deleted: Vec<&Session> = self
                    .sessions
                    .iter()
                    .filter(|s| s.deleted_at.is_some())
                    .collect();
                deleted.sort_by_key(|s| std::cmp::Reverse(s.deleted_at));
                deleted
                    .into_iter()
                    .map(|s| Row::Session(s.clone()))
                    .collect()
            }
        };
        let found = keep.and_then(|k| self.rows.iter().position(|r| r.key() == k));
        self.selected = found
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    /// `project/name` of a session, for messages.
    pub fn session_label(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| id.to_string(), Session::label)
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn selected_key(&self) -> Option<RowKey> {
        self.selected_row().map(Row::key)
    }

    /// Selects `key` once it shows up (after the next reload).
    pub fn select(&mut self, key: RowKey) {
        self.select_next = Some(key);
    }

    pub fn set_view(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.selected = 0;
            self.rebuild_rows();
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

    /// The current message, if it's recent.
    pub fn message(&self) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|m| m.at.elapsed() < MESSAGE_TTL)
    }

    /// Footer hints: the first key of each action available in this view.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        let actions: &[(Action, &str)] = match self.view {
            View::Live => &[
                (Action::Attach, "attach"),
                (Action::New, "new"),
                (Action::AddProject, "project"),
                (Action::Start, "start"),
                (Action::Stop, "stop"),
                (Action::Restart, "restart"),
                (Action::Delete, "delete"),
                (Action::ToggleRead, "read"),
                (Action::Deleted, "deleted"),
                (Action::Quit, "quit"),
            ],
            View::Deleted => &[
                (Action::Attach, "restore"),
                (Action::Deleted, "back"),
                (Action::Quit, "quit"),
            ],
        };
        actions
            .iter()
            .filter_map(|(a, what)| Some((self.keymap.label(*a)?.to_string(), *what)))
            .collect()
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Option<Op> {
        if self.dialog.is_some() {
            return self.on_dialog_key(e);
        }
        if self.view == View::Deleted && e.code == KeyCode::Esc {
            self.set_view(View::Live);
            return None;
        }
        let action = self.keymap.action(&e)?;
        match action {
            Action::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            Action::Down => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                None
            }
            Action::Quit => Some(Op::Quit),
            Action::Deleted => {
                self.set_view(if self.view == View::Live {
                    View::Deleted
                } else {
                    View::Live
                });
                None
            }
            _ if self.view == View::Deleted => self.on_deleted_action(action),
            _ => self.on_live_action(action),
        }
    }

    fn on_live_action(&mut self, action: Action) -> Option<Op> {
        match (action, self.selected_row().cloned()) {
            (Action::AddProject, _) => {
                self.input("New project's name:", String::new(), Purpose::ProjectName);
                None
            }
            (Action::New, row) | (Action::Attach, row @ Some(Row::Project(_))) => {
                match row {
                    Some(
                        Row::Project(Project { name, .. })
                        | Row::Session(Session { project: name, .. }),
                    ) => self.ask_new_session(name),
                    None => self.fail("Add a project first"),
                }
                None
            }
            (_, None) => None,
            // Starting an agent that can't resume loses its conversation: ask.
            (Action::Attach, Some(Row::Session(s)))
                if s.state == State::Stopped && !self.resumes(&s.agent) =>
            {
                self.confirm(self.new_conversation_question(&s), Op::Attach(s.id));
                None
            }
            (Action::Attach, Some(Row::Session(s))) => Some(Op::Attach(s.id)),
            (Action::Start | Action::Restart, Some(Row::Session(s))) if !self.resumes(&s.agent) => {
                let op = if action == Action::Start {
                    Op::Start(s.id.clone())
                } else {
                    Op::Restart(s.id.clone())
                };
                self.confirm(self.new_conversation_question(&s), op);
                None
            }
            (Action::Start, Some(Row::Session(s))) => Some(Op::Start(s.id)),
            (Action::Stop, Some(Row::Session(s))) => Some(Op::Stop(s.id)),
            (Action::Restart, Some(Row::Session(s))) if s.state == State::Working => {
                self.confirm(
                    format!("{} is working. Restart it?", s.label()),
                    Op::Restart(s.id),
                );
                None
            }
            (Action::Restart, Some(Row::Session(s))) => Some(Op::Restart(s.id)),
            (Action::Delete, Some(Row::Session(s))) => {
                let question = format!(
                    "Delete {}? It can be restored from the deleted view.",
                    s.label()
                );
                self.confirm(question, Op::Delete(s.id));
                None
            }
            (Action::Delete, Some(Row::Project(p))) => {
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
                None
            }
            (Action::ToggleRead, Some(Row::Session(s))) => Some(Op::SetUnread(s.id, !s.unread)),
            (_, Some(Row::Project(_))) => {
                self.fail("Select a session for that");
                None
            }
            _ => None,
        }
    }

    /// Asks for the agent if there's a choice, then for the name.
    fn ask_new_session(&mut self, project: String) {
        if self.agents.len() > 1 {
            let names: Vec<&str> = self.agents.iter().map(|(n, _)| n.as_str()).collect();
            let prompt = format!(
                "Agent for the new session in {project} ({}):",
                names.join(", ")
            );
            self.input(
                &prompt,
                names[0].to_string(),
                Purpose::ChooseAgent { project },
            );
        } else {
            let agent = self
                .agents
                .first()
                .map_or("claude".to_string(), |(n, _)| n.clone());
            self.ask_session_name(project, agent);
        }
    }

    fn ask_session_name(&mut self, project: String, agent: String) {
        let prompt = format!("New {agent} session in {project}. Name (empty for automatic):");
        self.input(
            &prompt,
            String::new(),
            Purpose::NewSession { project, agent },
        );
    }

    fn new_conversation_question(&self, s: &Session) -> String {
        format!(
            "{} ({}) can't resume its conversation. Start a new one?",
            s.label(),
            s.agent
        )
    }

    fn on_deleted_action(&mut self, action: Action) -> Option<Op> {
        let Some(Row::Session(s)) = self.selected_row().cloned() else {
            return None;
        };
        if action != Action::Attach {
            self.fail("This is the deleted view: restore a session first");
            return None;
        }
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

    fn on_dialog_key(&mut self, e: KeyEvent) -> Option<Op> {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let alt = e.modifiers.contains(KeyModifiers::ALT);
        match self.dialog.take()? {
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
            Purpose::ChooseAgent { project } => {
                if self.agents.iter().any(|(n, _)| *n == value) {
                    self.ask_session_name(project, value);
                } else {
                    self.fail(format!(
                        "No agent {value:?}; configure it under [agents] in config.toml"
                    ));
                }
                None
            }
            Purpose::NewSession { project, agent } => Some(Op::NewSession {
                project,
                agent,
                name: (!value.is_empty()).then_some(value),
            }),
            Purpose::ProjectName if value.is_empty() => None,
            Purpose::ProjectName => {
                let prompt = format!("Origin directory of {value}:");
                self.input(
                    &prompt,
                    self.default_path.clone(),
                    Purpose::ProjectPath { name: value },
                );
                None
            }
            Purpose::ProjectPath { name } => Some(Op::AddProject {
                name,
                path: expand_tilde(&value),
            }),
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

    /// Preview for a project row.
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
        let new_key = self.keymap.label(Action::New).unwrap_or("?");
        Text::from(vec![
            format!("origin    {}", p.origin.display()).into(),
            format!("sessions  {live} (deleted: {deleted})").into(),
            format!("env file  {} ({env_state})", env.display()).into(),
            "".into(),
            format!("Press {new_key} to start a session here.").into(),
        ])
    }

    /// Preview for a deleted session.
    pub fn deleted_info(&self, s: &Session) -> Text<'static> {
        let restore = self.keymap.label(Action::Attach).unwrap_or("?");
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
        match self.view {
            View::Live => {
                let key = self.keymap.label(Action::AddProject).unwrap_or("?");
                Text::from(format!(
                    "No projects yet. Press {key} to add one: a name and its directory."
                ))
            }
            View::Deleted => Text::from("No deleted sessions."),
        }
    }
}

fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.display())
        }
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Keys;
    use crate::model::tests::session;

    fn app() -> App {
        let agents = vec![("claude".to_string(), true)];
        let mut app = App::new(
            Keymap::new(&Keys::default()).unwrap(),
            "/work".into(),
            agents,
        );
        let projects = vec![
            Project {
                id: 1,
                name: "alpha".into(),
                origin: "/a".into(),
            },
            Project {
                id: 2,
                name: "beta".into(),
                origin: "/b".into(),
            },
        ];
        let mut one = session("s1");
        one.state = State::Idle;
        let mut two = session("s2");
        two.project_id = 2;
        two.project = "beta".into();
        let mut gone = session("s3");
        gone.deleted_at = Some(5);
        app.set_data(projects, vec![one, two, gone]);
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

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            assert_eq!(app.on_key(chr(c)), None);
        }
    }

    #[test]
    fn lists_projects_with_their_live_sessions() {
        let app = app();
        let keys: Vec<RowKey> = app.rows.iter().map(Row::key).collect();
        assert_eq!(
            keys,
            [
                RowKey::Project(1),
                RowKey::Session("s1".into()),
                RowKey::Project(2),
                RowKey::Session("s2".into())
            ]
        );
    }

    #[test]
    fn selection_follows_its_row_across_reloads() {
        let mut app = app();
        app.on_key(chr('j'));
        app.on_key(chr('j'));
        assert_eq!(app.selected_key(), Some(RowKey::Project(2)));
        // A new session in alpha shifts beta down; the selection stays on beta.
        let mut extra = session("s0");
        extra.name = "extra".into();
        let mut sessions = app.sessions.clone();
        sessions.insert(0, extra);
        app.set_data(app.projects.clone(), sessions);
        assert_eq!(app.selected_key(), Some(RowKey::Project(2)));
    }

    #[test]
    fn enter_attaches_sessions_and_opens_new_session_on_projects() {
        let mut app = app();
        app.on_key(chr('j'));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::Attach("s1".into()))
        );
        app.on_key(chr('k'));
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        type_text(&mut app, "api");
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::NewSession {
                project: "alpha".into(),
                agent: "claude".into(),
                name: Some("api".into())
            })
        );
    }

    #[test]
    fn delete_and_working_restart_ask_first() {
        let mut app = app();
        app.on_key(chr('j'));
        assert_eq!(app.on_key(chr('D')), None);
        assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
        assert_eq!(app.on_key(chr('n')), None);
        assert!(app.dialog.is_none());
        app.on_key(chr('D'));
        assert_eq!(app.on_key(chr('y')), Some(Op::Delete("s1".into())));

        // s1 is idle: restart right away. s2 is working: confirm.
        assert_eq!(app.on_key(chr('R')), Some(Op::Restart("s1".into())));
        app.on_key(chr('j'));
        app.on_key(chr('j'));
        assert_eq!(app.on_key(chr('R')), None);
        assert_eq!(app.on_key(chr('y')), Some(Op::Restart("s2".into())));
    }

    #[test]
    fn projects_with_sessions_cant_be_removed() {
        let mut app = app();
        assert_eq!(app.on_key(chr('D')), None);
        assert!(app.dialog.is_none());
        assert!(app.message().unwrap().error);
    }

    #[test]
    fn adding_a_project_asks_name_then_path() {
        let mut app = app();
        app.on_key(chr('P'));
        type_text(&mut app, "gamma");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        // The path is prefilled with the current directory.
        for _ in 0.."/work".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "~/src/gamma");
        let op = app.on_key(key(KeyCode::Enter)).unwrap();
        let Op::AddProject { name, path } = op else {
            panic!("{op:?}")
        };
        assert_eq!(name, "gamma");
        assert_eq!(
            path,
            format!("{}/src/gamma", dirs::home_dir().unwrap().display())
        );
    }

    #[test]
    fn escape_cancels_dialogs() {
        let mut app = app();
        app.on_key(chr('n'));
        type_text(&mut app, "x");
        assert_eq!(app.on_key(key(KeyCode::Esc)), None);
        assert!(app.dialog.is_none());
    }

    #[test]
    fn deleted_view_restores_and_asks_for_a_name_on_conflict() {
        let mut app = app();
        app.on_key(chr('X'));
        assert_eq!(app.view, View::Deleted);
        assert_eq!(app.selected_key(), Some(RowKey::Session("s3".into())));
        // s3 has the default name "s-s3", which is free.
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::Restore {
                id: "s3".into(),
                name: None
            })
        );
        assert_eq!(app.on_key(chr('d')), None);
        assert!(app.message().unwrap().error);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Live);
    }

    #[test]
    fn restoring_onto_a_taken_name_prompts() {
        let mut app = app();
        let mut gone = session("s4");
        gone.name = "s-s1".into();
        gone.deleted_at = Some(9);
        let mut sessions = app.sessions.clone();
        sessions.push(gone);
        app.set_data(app.projects.clone(), sessions);
        app.on_key(chr('X'));
        assert_eq!(app.selected_key(), Some(RowKey::Session("s4".into())));
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        type_text(&mut app, "again");
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::Restore {
                id: "s4".into(),
                name: Some("again".into())
            })
        );
    }

    #[test]
    fn with_several_agents_new_asks_which_first() {
        let mut app = app();
        app.agents.push(("aider".into(), false));
        app.on_key(chr('n'));
        // Prefilled with the first agent (claude); replace it.
        for _ in 0.."claude".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "aider");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert!(
            matches!(&app.dialog, Some(Dialog::Input { prompt, .. }) if prompt.starts_with("New aider session"))
        );
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Op::NewSession {
                project: "alpha".into(),
                agent: "aider".into(),
                name: None
            })
        );
        // An unknown agent is an error, not a session.
        app.on_key(chr('n'));
        type_text(&mut app, "x");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert!(app.dialog.is_none() && app.message().unwrap().error);
    }

    #[test]
    fn agents_that_cant_resume_ask_before_a_new_conversation() {
        let mut app = app();
        app.agents.push(("aider".into(), false));
        let mut generic = session("g1");
        generic.agent = "aider".into();
        generic.name = "g".into();
        generic.state = State::Stopped;
        let mut sessions = app.sessions.clone();
        sessions.push(generic);
        app.set_data(app.projects.clone(), sessions);
        app.select(RowKey::Session("g1".into()));
        app.rebuild_rows();
        for (press, op) in [
            ('s', Op::Start("g1".into())),
            ('R', Op::Restart("g1".into())),
        ] {
            assert_eq!(app.on_key(chr(press)), None);
            let Some(Dialog::Confirm { question, .. }) = &app.dialog else {
                panic!("no confirmation")
            };
            assert!(question.contains("can't resume"), "{question}");
            assert_eq!(app.on_key(chr('y')), Some(op));
        }
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(app.on_key(chr('y')), Some(Op::Attach("g1".into())));
    }

    #[test]
    fn hints_follow_the_keymap() {
        let app = app();
        let hints = app.hints();
        assert_eq!(hints[0], ("Enter".to_string(), "attach"));
        assert!(hints.contains(&("X".to_string(), "deleted")));
    }
}
