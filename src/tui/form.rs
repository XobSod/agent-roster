//! Forms for creating and editing projects and sessions: labelled fields moved
//! through with Tab or ↑/↓, choices changed with ←/→, submitted with Enter.

use std::fs;
use std::path::Path;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::Op;
use crate::agent::{self, CLAUDE};
use crate::model::{Project, Session};
use crate::paths::PROJECT_ENV;
use crate::util::expand_tilde;

/// Shown for "no value": the agent's own default.
const DEFAULT: &str = "default";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id {
    Directory,
    Name,
    Project,
    Agent,
    Model,
    Effort,
    Prompt,
    Title,
    EnvFile,
    Conversation,
    Copy,
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// Free text; a `path` field completes directories with Tab.
    Text { path: bool, placeholder: String },
    /// One of these, changed with ←/→.
    Choice(Vec<String>),
    /// One of these with ←/→, or any typed value.
    Combo(Vec<String>),
    /// Read-only, worked out from the other fields.
    Info,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub id: Id,
    pub label: &'static str,
    pub kind: Kind,
    pub value: String,
    pub hidden: bool,
    /// Shown after a value: what a model alias turns out to be.
    pub notes: std::collections::HashMap<String, String>,
    /// Changed by the user, so no longer filled in from other fields.
    edited: bool,
}

impl Field {
    fn new(id: Id, label: &'static str, kind: Kind, value: impl Into<String>) -> Field {
        Field {
            id,
            label,
            kind,
            value: value.into(),
            hidden: false,
            notes: Default::default(),
            edited: false,
        }
    }

    fn focusable(&self) -> bool {
        !self.hidden && !matches!(self.kind, Kind::Info)
    }

    /// What ←/→ do here, for the hint next to the focused field.
    pub fn hint(&self) -> &'static str {
        match self.kind {
            Kind::Text { path: true, .. } => "Tab completes",
            Kind::Text { .. } => "",
            Kind::Choice(_) => "←→ change",
            Kind::Combo(_) => "←→ or type",
            Kind::Info => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Purpose {
    NewProject,
    EditProject(i64),
    NewSession,
    EditSession(String),
    /// Adopting a conversation, open in a Claude process or not.
    Adopt {
        conversation: String,
        open_in: Option<i32>,
    },
}

pub enum Outcome {
    Continue,
    Cancel,
    Submit(Op),
}

pub struct Form {
    pub purpose: Purpose,
    pub title: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
    /// A note under the form, e.g. what a directory could complete to.
    pub note: Option<String>,
    projects: Vec<Project>,
    /// The project the session fields were last filled in from.
    filled_from: Option<String>,
    /// What models asked for turned out to be (`Db::model_names`).
    model_names: std::collections::HashMap<String, String>,
}

fn options(first: &str, rest: &[&str]) -> Vec<String> {
    std::iter::once(first)
        .chain(rest.iter().copied())
        .map(String::from)
        .collect()
}

fn or_default(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| DEFAULT.to_string())
}

/// The model field; its choices are the agent's (see `Form::update`).
fn model_field(value: impl Into<String>) -> Field {
    Field::new(Id::Model, "Model", Kind::Combo(Vec::new()), value)
}

fn effort_field(value: impl Into<String>) -> Field {
    Field::new(Id::Effort, "Effort", Kind::Choice(Vec::new()), value)
}

impl Form {
    pub fn new_project(directory: &str, agents: &[String]) -> Form {
        let fields = vec![
            Field::new(
                Id::Directory,
                "Directory",
                Kind::Text {
                    path: true,
                    placeholder: String::new(),
                },
                directory,
            ),
            Field::new(
                Id::Name,
                "Name",
                Kind::Text {
                    path: false,
                    placeholder: "from the directory".into(),
                },
                "",
            ),
            Field::new(
                Id::Agent,
                "Agent",
                Kind::Choice(agents.to_vec()),
                agent::DEFAULT,
            ),
            model_field(DEFAULT),
            effort_field(DEFAULT),
            Field::new(Id::EnvFile, "Env file", Kind::Info, ""),
        ];
        Form::build(
            Purpose::NewProject,
            "New project".into(),
            fields,
            Vec::new(),
        )
    }

    /// A new chat: the new-session form on the chats project, on the prompt.
    pub fn new_chat(projects: &[Project], chats: &Project, agents: &[String]) -> Form {
        let mut form = Form::new_session(projects, Some(chats), agents);
        form.title = "New chat".into();
        if let Some(i) = form.fields.iter().position(|f| f.id == Id::Prompt) {
            form.focus = i;
        }
        form
    }

    /// `builtin`: the chats project, whose directory follows `chats_dir`.
    pub fn edit_project(p: &Project, agents: &[String], builtin: bool) -> Form {
        let mut fields = vec![
            Field::new(
                Id::Name,
                "Name",
                Kind::Text {
                    path: false,
                    placeholder: String::new(),
                },
                &p.name,
            ),
            Field::new(
                Id::Directory,
                "Directory",
                // The chats project's follows `chats_dir` in config.toml.
                if builtin {
                    Kind::Info
                } else {
                    Kind::Text {
                        path: true,
                        placeholder: String::new(),
                    }
                },
                p.origin.display().to_string(),
            ),
            Field::new(
                Id::Agent,
                "Agent",
                Kind::Choice(agents.to_vec()),
                p.agent.as_deref().unwrap_or(agent::DEFAULT),
            ),
            model_field(or_default(&p.model)),
            effort_field(or_default(&p.effort)),
            Field::new(Id::EnvFile, "Env file", Kind::Info, ""),
        ];
        fields[0].edited = true;
        let title = if builtin {
            format!("Edit project {} (built-in, for chats)", p.name)
        } else {
            format!("Edit project {}", p.name)
        };
        Form::build(Purpose::EditProject(p.id), title, fields, Vec::new())
    }

    pub fn new_session(
        projects: &[Project],
        selected: Option<&Project>,
        agents: &[String],
    ) -> Form {
        let names: Vec<String> = projects.iter().map(|p| p.name.clone()).collect();
        let project = selected
            .or(projects.first())
            .map_or(String::new(), |p| p.name.clone());
        let fields = vec![
            Field::new(Id::Project, "Project", Kind::Choice(names), project),
            Field::new(
                Id::Name,
                "Name",
                Kind::Text {
                    path: false,
                    placeholder: "automatic".into(),
                },
                "",
            ),
            Field::new(
                Id::Agent,
                "Agent",
                Kind::Choice(agents.to_vec()),
                agent::DEFAULT,
            ),
            model_field(DEFAULT),
            effort_field(DEFAULT),
            Field::new(
                Id::Prompt,
                "Prompt",
                Kind::Text {
                    path: false,
                    placeholder: "optional: typed in once the agent is ready".into(),
                },
                "",
            ),
        ];
        let mut form = Form::build(
            Purpose::NewSession,
            "New session".into(),
            fields,
            projects.to_vec(),
        );
        // Start on the name when the project is already chosen.
        if selected.is_some() || projects.len() == 1 {
            form.focus = 1;
        }
        form
    }

    pub fn edit_session(s: &Session) -> Form {
        let mut fields = vec![
            Field::new(
                Id::Name,
                "Name",
                Kind::Text {
                    path: false,
                    placeholder: String::new(),
                },
                &s.name,
            ),
            Field::new(
                Id::Title,
                "Title",
                Kind::Text {
                    path: false,
                    placeholder: format!(
                        "{}'s: {}",
                        s.agent,
                        s.title.as_deref().unwrap_or("none yet")
                    ),
                },
                s.title_override.clone().unwrap_or_default(),
            ),
            Field::new(Id::Agent, "Agent", Kind::Info, &s.agent),
            model_field(or_default(&s.model)),
            effort_field(or_default(&s.effort)),
        ];
        fields[0].edited = true;
        Form::build(
            Purpose::EditSession(s.id.clone()),
            format!("Edit session {}", s.label()),
            fields,
            Vec::new(),
        )
    }

    /// Adopting `a`: into the project holding its directory, or a new one.
    pub fn adopt(a: &crate::engine::Adoptable) -> Form {
        let c = &a.conversation;
        let what = c.label();
        let branch = c
            .branch
            .as_ref()
            .map_or(String::new(), |b| format!(" ({b})"));
        let mut directory = format!("{}{branch}", c.cwd.display());
        let project = match &a.project {
            Some(p) => Field::new(Id::Project, "Project", Kind::Info, &p.name),
            None => {
                directory.push_str(" · not in a project");
                Field::new(
                    Id::Project,
                    "Project",
                    Kind::Text {
                        path: false,
                        placeholder: String::new(),
                    },
                    &a.new_project,
                )
            }
        };
        let defaults = a.project.as_ref();
        let moved = match c.open_in {
            Some(pid) => {
                format!("move it; it's open in Claude (pid {pid}), close it there")
            }
            None => "move it; resume it only here from now on".to_string(),
        };
        let mut copy = Field::new(
            Id::Copy,
            "Copy",
            Kind::Choice(options("no", &["yes"])),
            "no",
        );
        copy.notes = [
            ("no".to_string(), moved),
            (
                "yes".to_string(),
                "a copy; the original stays as it is".to_string(),
            ),
        ]
        .into();
        let fields = vec![
            Field::new(Id::Conversation, "Adopting", Kind::Info, what),
            Field::new(Id::Directory, "Directory", Kind::Info, directory),
            project,
            Field::new(
                Id::Name,
                "Name",
                Kind::Text {
                    path: false,
                    placeholder: "automatic".into(),
                },
                "",
            ),
            model_field(or_default(&defaults.and_then(|p| p.model.clone()))),
            effort_field(or_default(&defaults.and_then(|p| p.effort.clone()))),
            copy,
        ];
        let mut form = Form::build(
            Purpose::Adopt {
                conversation: c.id.clone(),
                open_in: c.open_in,
            },
            "Adopt a conversation".into(),
            fields,
            Vec::new(),
        );
        form.focus = form.fields.iter().position(Field::focusable).unwrap_or(0);
        form
    }

    fn build(purpose: Purpose, title: String, fields: Vec<Field>, projects: Vec<Project>) -> Form {
        let mut form = Form {
            purpose,
            title,
            fields,
            focus: 0,
            error: None,
            note: None,
            projects,
            filled_from: None,
            model_names: Default::default(),
        };
        form.update();
        form
    }

    /// What each model choice is, shown next to it: the default or an alias
    /// as it turned out (`names`, learned from sessions; "latest" until known),
    /// a pinned id by its name.
    pub fn with_model_names(mut self, names: &std::collections::HashMap<String, String>) -> Form {
        self.model_names = names.clone();
        self.update();
        self
    }

    /// Enter's verb, for the hint.
    pub fn verb(&self) -> &'static str {
        match self.purpose {
            Purpose::NewProject => "create",
            Purpose::NewSession => "start",
            Purpose::Adopt { .. } => "adopt",
            Purpose::EditProject(_) | Purpose::EditSession(_) => "save",
        }
    }

    fn field(&self, id: Id) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    fn field_mut(&mut self, id: Id) -> Option<&mut Field> {
        self.fields.iter_mut().find(|f| f.id == id)
    }

    fn value(&self, id: Id) -> &str {
        self.field(id).map_or("", |f| f.value.as_str())
    }

    /// The agent the form is about; without an agent field, it adopts a
    /// conversation, which is Claude's.
    fn agent(&self) -> &str {
        self.field(Id::Agent).map_or(CLAUDE, |f| f.value.as_str())
    }

    /// What the agent's model choices are (see `with_model_names`).
    fn model_notes(&self) -> std::collections::HashMap<String, String> {
        let agent = self.agent();
        let learned = |asked: &str| {
            self.model_names
                .get(&agent::learned_key(agent, asked))
                .cloned()
        };
        let mut notes = std::collections::HashMap::new();
        let mut name = agent.chars();
        let name: String = name
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .chain(name)
            .collect();
        notes.insert(
            DEFAULT.to_string(),
            learned(DEFAULT).unwrap_or_else(|| format!("{name}'s default")),
        );
        if let Some(c) = agent::adapter(agent).choices() {
            for alias in c.aliases {
                notes.insert(
                    alias.to_string(),
                    learned(alias).unwrap_or_else(|| "latest".into()),
                );
            }
            for id in c.pinned {
                notes.insert(id.to_string(), crate::util::model_name(id));
            }
        }
        notes
    }

    /// The value, or None for "default" or empty.
    fn setting(&self, id: Id) -> Option<String> {
        let value = self.value(id).trim();
        (!value.is_empty() && value != DEFAULT).then(|| value.to_string())
    }

    /// Fills in fields that follow others.
    fn update(&mut self) {
        // Model and effort choices are the agent's; one that takes neither hides them.
        let choices = agent::adapter(self.agent()).choices();
        let notes = self.model_notes();
        if let Some(f) = self.field_mut(Id::Model) {
            f.hidden = choices.is_none();
            f.kind = Kind::Combo(options(
                DEFAULT,
                &choices.map_or(Vec::new(), |c| c.models()),
            ));
            f.notes = notes;
        }
        if let Some(f) = self.field_mut(Id::Effort) {
            f.hidden = choices.is_none();
            f.kind = Kind::Choice(options(DEFAULT, choices.map_or(&[][..], |c| c.efforts)));
        }
        // A new project is named after its directory until the user names it.
        let directory = expand_tilde(self.value(Id::Directory));
        if matches!(self.purpose, Purpose::NewProject)
            && let Some(name) = self.field_mut(Id::Name).filter(|f| !f.edited)
        {
            name.value = crate::util::directory_name(Path::new(&directory));
        }
        let env = Path::new(&directory).join(PROJECT_ENV);
        let state = if env.exists() {
            "found: sourced before every start"
        } else {
            "none"
        };
        if let Some(f) = self.field_mut(Id::EnvFile) {
            f.value = format!("{PROJECT_ENV} ({state})");
        }
        // A new session takes its project's defaults, unless changed here.
        let project = self.value(Id::Project).to_string();
        if self.purpose == Purpose::NewSession && self.filled_from.as_deref() != Some(&project) {
            if let Some(p) = self.projects.iter().find(|p| p.name == project).cloned() {
                let defaults = [
                    (
                        Id::Agent,
                        p.agent
                            .clone()
                            .unwrap_or_else(|| agent::DEFAULT.to_string()),
                    ),
                    (Id::Model, or_default(&p.model)),
                    (Id::Effort, or_default(&p.effort)),
                ];
                for (id, value) in defaults {
                    if let Some(f) = self.field_mut(id).filter(|f| !f.edited) {
                        f.value = value;
                    }
                }
            }
            self.filled_from = Some(project);
            // The agent may have changed.
            self.update();
        }
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Outcome {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let alt = e.modifiers.contains(KeyModifiers::ALT);
        self.note = None;
        match e.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if ctrl => return Outcome::Cancel,
            KeyCode::Enter => {
                return match self.submit() {
                    Ok(op) => Outcome::Submit(op),
                    Err(message) => {
                        self.error = Some(message);
                        Outcome::Continue
                    }
                };
            }
            KeyCode::Tab => {
                if !self.complete_path() {
                    self.move_focus(1);
                }
            }
            KeyCode::Down => self.move_focus(1),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(-1),
            _ => self.edit(e.code, ctrl, alt),
        }
        Outcome::Continue
    }

    fn move_focus(&mut self, step: isize) {
        let n = self.fields.len() as isize;
        let mut i = self.focus as isize;
        for _ in 0..n {
            i = (i + step).rem_euclid(n);
            if self.fields[i as usize].focusable() {
                self.focus = i as usize;
                return;
            }
        }
    }

    fn edit(&mut self, code: KeyCode, ctrl: bool, alt: bool) {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        let before = field.value.clone();
        match (&field.kind, code) {
            (Kind::Choice(opts) | Kind::Combo(opts), KeyCode::Left | KeyCode::Right) => {
                field.value = cycle(opts, &field.value, code == KeyCode::Right);
            }
            (Kind::Choice(opts), KeyCode::Char('h' | 'l' | ' ')) => {
                field.value = cycle(opts, &field.value, code != KeyCode::Char('h'));
            }
            (Kind::Text { .. } | Kind::Combo(_), KeyCode::Backspace) => {
                field.value.pop();
            }
            (Kind::Text { .. } | Kind::Combo(_), KeyCode::Char('u')) if ctrl => field.value.clear(),
            (Kind::Combo(_), KeyCode::Char(c)) if !ctrl && !alt => {
                // Typing replaces a listed choice rather than appending to it.
                if field.value == DEFAULT {
                    field.value.clear();
                }
                field.value.push(c);
            }
            (Kind::Text { .. }, KeyCode::Char(c)) if !ctrl && !alt => field.value.push(c),
            _ => return,
        }
        if field.value != before {
            field.edited = true;
            self.error = None;
            self.update();
        }
    }

    /// Completes the focused directory field. False if there was nothing to do.
    fn complete_path(&mut self) -> bool {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return false;
        };
        if !matches!(field.kind, Kind::Text { path: true, .. }) {
            return false;
        }
        let (completed, matches) = complete_directory(&field.value);
        if completed != field.value {
            field.value = completed;
            field.edited = true;
            self.update();
            return true;
        }
        if matches.len() > 1 {
            let shown: Vec<&str> = matches.iter().take(6).map(String::as_str).collect();
            let more = if matches.len() > 6 { ", …" } else { "" };
            self.note = Some(format!("{}{more}", shown.join(", ")));
            return true;
        }
        false
    }

    fn submit(&self) -> Result<Op, String> {
        let required = |id: Id, what: &str| -> Result<String, String> {
            let value = self.value(id).trim();
            if value.is_empty() {
                Err(format!("{what} is missing"))
            } else {
                Ok(value.to_string())
            }
        };
        // A project's default agent: None for the default one.
        let agent = || self.setting(Id::Agent).filter(|a| a != agent::DEFAULT);
        // Only for an agent that takes them.
        let takes = agent::adapter(self.agent()).choices().is_some();
        let model = || if takes { self.setting(Id::Model) } else { None };
        let effort = || {
            if takes {
                self.setting(Id::Effort)
            } else {
                None
            }
        };
        Ok(match &self.purpose {
            Purpose::NewProject => Op::AddProject {
                name: required(Id::Name, "The name")?,
                path: expand_tilde(&required(Id::Directory, "The directory")?),
                agent: agent(),
                model: model(),
                effort: effort(),
            },
            Purpose::EditProject(id) => Op::EditProject {
                id: *id,
                name: required(Id::Name, "The name")?,
                path: expand_tilde(&required(Id::Directory, "The directory")?),
                agent: agent(),
                model: model(),
                effort: effort(),
            },
            Purpose::NewSession => Op::NewSession {
                project: required(Id::Project, "The project")?,
                agent: self.value(Id::Agent).to_string(),
                name: self.setting(Id::Name),
                model: model(),
                effort: effort(),
                prompt: self.setting(Id::Prompt),
            },
            Purpose::Adopt {
                conversation,
                open_in,
            } => Op::Adopt {
                conversation: conversation.clone(),
                project: required(Id::Project, "The project")?,
                name: self.setting(Id::Name),
                model: self.setting(Id::Model),
                effort: self.setting(Id::Effort),
                fork: self.value(Id::Copy) == "yes",
                // Asked first (see App).
                force: open_in.is_some(),
            },
            Purpose::EditSession(id) => Op::EditSession {
                id: id.clone(),
                name: required(Id::Name, "The name")?,
                title: self.setting(Id::Title),
                model: model(),
                effort: effort(),
            },
        })
    }
}

/// The next (or previous) option after `value`; a typed value goes to the first.
fn cycle(options: &[String], value: &str, forward: bool) -> String {
    if options.is_empty() {
        return value.to_string();
    }
    let n = options.len();
    let next = match options.iter().position(|o| o == value) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None => 0,
    };
    options[next].clone()
}

/// Completes `input` as far as the directories under it allow; also returns the
/// matching names. A single match gets a trailing '/'.
fn complete_directory(input: &str) -> (String, Vec<String>) {
    let expanded = expand_tilde(input);
    let (dir, prefix) = match expanded.rfind('/') {
        Some(i) => (&expanded[..=i], &expanded[i + 1..]),
        None => ("./", expanded.as_str()),
    };
    let mut matches: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    n.starts_with(prefix) && (prefix.starts_with('.') || !n.starts_with('.'))
                })
                .collect()
        })
        .unwrap_or_default();
    matches.sort();
    let Some(first) = matches.first() else {
        return (input.to_string(), matches);
    };
    let common = matches.iter().fold(first.clone(), |common, m| {
        common
            .chars()
            .zip(m.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a)
            .collect()
    });
    let mut completed = format!(
        "{}{common}",
        if dir == "./" && !expanded.starts_with("./") {
            ""
        } else {
            dir
        }
    );
    if matches.len() == 1 {
        completed.push('/');
    }
    // Keep the user's "~".
    if input.starts_with('~')
        && let Some(home) = dirs::home_dir()
    {
        let home = home.display().to_string();
        if let Some(rest) = completed.strip_prefix(&home) {
            completed = format!("~{rest}");
        }
    }
    if completed.len() < input.len() {
        completed = input.to_string();
    }
    (completed, matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(form: &mut Form, text: &str) {
        for c in text.chars() {
            form.on_key(key(KeyCode::Char(c)));
        }
    }

    fn submit(form: &mut Form) -> Op {
        match form.on_key(key(KeyCode::Enter)) {
            Outcome::Submit(op) => op,
            _ => panic!("not submitted: {:?}", form.error),
        }
    }

    fn agents() -> Vec<String> {
        vec!["claude".into(), "aider".into()]
    }

    fn project(name: &str, model: Option<&str>) -> Project {
        Project {
            id: 7,
            name: name.into(),
            origin: format!("/{name}").into(),
            agent: None,
            model: model.map(String::from),
            effort: Some("high".into()),
        }
    }

    #[test]
    fn new_project_is_named_after_its_directory() {
        let mut form = Form::new_project("/src/my app", &agents());
        assert_eq!(form.value(Id::Name), "my-app");
        // Tab past the directory (nothing to complete), then the name is the user's.
        form.focus = 1;
        form.on_key(key(KeyCode::Char('x')));
        assert_eq!(form.value(Id::Name), "my-appx");
        form.focus = 0;
        type_text(&mut form, "2");
        assert_eq!(form.value(Id::Name), "my-appx");
    }

    #[test]
    fn choices_cycle_and_the_model_takes_any_name() {
        let mut form = Form::new_project("/src/app", &agents());
        form.focus = 3; // model
        form.on_key(key(KeyCode::Right));
        assert_eq!(form.value(Id::Model), "opus");
        form.on_key(key(KeyCode::Left));
        assert_eq!(form.value(Id::Model), "default");
        type_text(&mut form, "claude-opus-5-5");
        assert_eq!(form.value(Id::Model), "claude-opus-5-5");
        form.on_key(key(KeyCode::Down)); // effort
        form.on_key(key(KeyCode::Char('l')));
        form.on_key(key(KeyCode::Char('l')));
        assert_eq!(form.value(Id::Effort), "medium");
        let op = submit(&mut form);
        assert_eq!(
            op,
            Op::AddProject {
                name: "app".into(),
                path: "/src/app".into(),
                agent: None,
                model: Some("claude-opus-5-5".into()),
                effort: Some("medium".into()),
            }
        );
    }

    #[test]
    fn other_agents_hide_model_and_effort() {
        let mut form = Form::new_project("/src/app", &agents());
        form.focus = 2; // agent
        form.on_key(key(KeyCode::Right));
        assert_eq!(form.value(Id::Agent), "aider");
        assert!(form.field(Id::Model).unwrap().hidden && form.field(Id::Effort).unwrap().hidden);
        // ↓ skips the hidden fields and the info line, back to the top.
        form.on_key(key(KeyCode::Down));
        assert_eq!(form.fields[form.focus].id, Id::Directory);
        let Op::AddProject { agent, model, .. } = submit(&mut form) else {
            panic!()
        };
        assert_eq!((agent.as_deref(), model), (Some("aider"), None));
    }

    #[test]
    fn new_session_takes_the_projects_defaults() {
        let projects = vec![project("alpha", Some("opus")), project("beta", None)];
        let mut form = Form::new_session(&projects, Some(&projects[0]), &agents());
        assert_eq!(form.fields[form.focus].id, Id::Name);
        assert_eq!(
            (form.value(Id::Model), form.value(Id::Effort)),
            ("opus", "high")
        );
        // Choosing another project refills what wasn't changed here.
        form.focus = 0;
        form.on_key(key(KeyCode::Right));
        assert_eq!(form.value(Id::Project), "beta");
        assert_eq!(form.value(Id::Model), "default");
        form.focus = 5;
        type_text(&mut form, "fix the tests");
        assert_eq!(
            submit(&mut form),
            Op::NewSession {
                project: "beta".into(),
                agent: "claude".into(),
                name: None,
                model: None,
                effort: Some("high".into()),
                prompt: Some("fix the tests".into()),
            }
        );
    }

    #[test]
    fn missing_values_are_errors_not_submissions() {
        let mut form = Form::new_project("", &agents());
        assert!(matches!(
            form.on_key(key(KeyCode::Enter)),
            Outcome::Continue
        ));
        assert!(form.error.is_some());
        assert!(matches!(form.on_key(key(KeyCode::Esc)), Outcome::Cancel));
    }

    #[test]
    fn completes_directories() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["alpha", "alpine", "beta", ".hidden"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("alfile"), "").unwrap();
        let base = dir.path().display().to_string();
        assert_eq!(
            complete_directory(&format!("{base}/b")).0,
            format!("{base}/beta/")
        );
        let (completed, matches) = complete_directory(&format!("{base}/a"));
        assert_eq!(completed, format!("{base}/alp"));
        assert_eq!(matches, ["alpha", "alpine"]);
        assert_eq!(
            complete_directory(&format!("{base}/")).1,
            ["alpha", "alpine", "beta"]
        );
        assert_eq!(
            complete_directory(&format!("{base}/zzz")).0,
            format!("{base}/zzz")
        );
    }

    #[test]
    fn adopting_a_copy_with_the_projects_defaults() {
        use crate::tui::picker::tests::adoptable;
        let mut a = adoptable("c1", "Fix the parser", Some("alpha"));
        a.project.as_mut().unwrap().model = Some("opus".into());
        let mut form = Form::adopt(&a);
        // The project is known: the first thing to fill in is the name.
        assert_eq!(form.fields[form.focus].id, Id::Name);
        assert!(matches!(form.field(Id::Project).unwrap().kind, Kind::Info));
        assert_eq!(form.value(Id::Model), "opus");
        assert!(!form.field(Id::Model).unwrap().hidden);
        type_text(&mut form, "parser");
        form.focus = form.fields.iter().position(|f| f.id == Id::Copy).unwrap();
        form.on_key(key(KeyCode::Right));
        assert_eq!(
            submit(&mut form),
            Op::Adopt {
                conversation: "c1".into(),
                project: "alpha".into(),
                name: Some("parser".into()),
                model: Some("opus".into()),
                effort: None,
                fork: true,
                force: false,
            }
        );
        // Outside a project: the new project's name comes first, and can't be empty.
        let mut form = Form::adopt(&adoptable("c2", "Elsewhere", None));
        assert_eq!(form.fields[form.focus].id, Id::Project);
        assert_eq!(form.value(Id::Project), "elsewhere");
        form.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(matches!(
            form.on_key(key(KeyCode::Enter)),
            Outcome::Continue
        ));
        assert!(form.error.is_some());
    }

    #[test]
    fn edit_session_keeps_the_agent_fixed() {
        let mut s = crate::model::tests::session("s1");
        s.name = "api".into();
        s.model = Some("opus".into());
        let mut form = Form::edit_session(&s);
        form.on_key(key(KeyCode::Backspace));
        type_text(&mut form, "2");
        assert_eq!(
            submit(&mut form),
            Op::EditSession {
                id: "s1".into(),
                name: "ap2".into(),
                title: None,
                model: Some("opus".into()),
                effort: None
            }
        );
    }
}
