//! Forms for creating and editing projects and sessions: labelled fields moved
//! through with Tab or ↑/↓, choices changed with ←/→, submitted with Enter.

use std::fs;
use std::path::Path;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::input;
use super::op::Op;
use crate::adopt::RunOn;
use crate::agent;
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
    /// What an adopted conversation goes on with.
    RunOn,
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
    /// Shown after a value: what a model alias turns out to be, what a choice
    /// means.
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
        /// The model and effort it last ran on, as far as its agent recorded
        /// them, and its project's for its agent.
        ran_on: (Option<String>, Option<String>),
        defaults: (Option<String>, Option<String>),
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
    /// The project, and the agent, the session fields were last filled in
    /// for.
    filled_from: Option<(String, String)>,
    models: Models,
}

/// What forms know of agents' models.
#[derive(Clone, Debug, Default)]
pub struct Models {
    /// Each agent's to offer (`Adapter::models`), by agent.
    pub offered: std::collections::HashMap<String, Vec<agent::Model>>,
    /// What models asked for turned out to be (`Db::model_names`).
    pub names: std::collections::HashMap<String, String>,
    /// The model and effort config.toml gives each agent's new sessions,
    /// by agent: what "default" is.
    pub chosen: std::collections::HashMap<String, (Option<String>, Option<String>)>,
}

impl Models {
    /// What's known without asking the agents: the lists roster has itself
    /// (Claude's).
    pub fn known() -> Models {
        let unasked: &agent::Ask<'_> = &|_| None;
        let config = crate::config::Config::default();
        Models {
            offered: agent::BUILT_IN
                .iter()
                .map(|a| (a.to_string(), agent::adapter(a).models(&config, unasked)))
                .collect(),
            names: Default::default(),
            chosen: Default::default(),
        }
    }

    /// What config.toml gives the agents' new sessions.
    pub fn chosen_in(
        config: &crate::config::Config,
    ) -> std::collections::HashMap<String, (Option<String>, Option<String>)> {
        config
            .agents
            .iter()
            .map(|(name, a)| (name.clone(), (a.model.clone(), a.effort.clone())))
            .collect()
    }
}

fn options(first: &str, rest: &[&str]) -> Vec<String> {
    std::iter::once(first)
        .chain(rest.iter().copied())
        .map(String::from)
        .collect()
}

/// The first of `agents`: the default one, as `agent::available` lists them.
fn first(agents: &[String]) -> &str {
    agents.first().map_or("", String::as_str)
}

/// What an adopted conversation can go on with (`Id::RunOn`).
const CONVERSATION: &str = "conversation";
const DEFAULTS: &str = "defaults";
const OVERRIDES: &str = "overrides";

fn or_default(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| DEFAULT.to_string())
}

/// Free text, `placeholder` shown while it's empty.
fn text(placeholder: &str) -> Kind {
    Kind::Text {
        path: false,
        placeholder: placeholder.to_string(),
    }
}

/// A directory; Tab completes it.
fn path() -> Kind {
    Kind::Text {
        path: true,
        placeholder: String::new(),
    }
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
            Field::new(Id::Directory, "Directory", path(), directory),
            Field::new(Id::Name, "Name", text("from the directory"), ""),
            Field::new(
                Id::Agent,
                "Agent",
                Kind::Choice(agents.to_vec()),
                first(agents),
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
        let fields = vec![
            Field::new(Id::Name, "Name", text(""), &p.name),
            Field::new(
                Id::Directory,
                "Directory",
                // The chats project's follows `chats_dir` in config.toml.
                if builtin { Kind::Info } else { path() },
                p.origin.display().to_string(),
            ),
            Field::new(
                Id::Agent,
                "Agent",
                Kind::Choice(agents.to_vec()),
                p.agent.as_deref().unwrap_or(first(agents)),
            ),
            model_field(or_default(&p.model)),
            effort_field(or_default(&p.effort)),
            Field::new(Id::EnvFile, "Env file", Kind::Info, ""),
        ];
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
                Id::Agent,
                "Agent",
                Kind::Choice(agents.to_vec()),
                first(agents),
            ),
            // The agent's name in the project (a role, say); its title is
            // another thing, the task it's on.
            Field::new(Id::Name, "Agent name", text("automatic"), ""),
            model_field(DEFAULT),
            effort_field(DEFAULT),
            Field::new(
                Id::Prompt,
                "Prompt",
                text("optional: typed in once the agent is ready"),
                "",
            ),
        ];
        let mut form = Form::build(
            Purpose::NewSession,
            "New session".into(),
            fields,
            projects.to_vec(),
        );
        // Start on the agent when the project is already chosen.
        if selected.is_some() || projects.len() == 1 {
            form.focus = 1;
        }
        form
    }

    pub fn edit_session(s: &Session) -> Form {
        let fields = vec![
            Field::new(Id::Agent, "Agent", Kind::Info, &s.agent),
            Field::new(Id::Name, "Agent name", text(""), &s.name),
            Field::new(
                Id::Title,
                "Title",
                text(&format!(
                    "{}'s: {}",
                    s.agent,
                    s.title.as_deref().unwrap_or("none yet")
                )),
                s.title_override.clone().unwrap_or_default(),
            ),
            model_field(or_default(&s.model)),
            effort_field(or_default(&s.effort)),
        ];
        let mut form = Form::build(
            Purpose::EditSession(s.id.clone()),
            format!("Edit session {}", s.label()),
            fields,
            Vec::new(),
        );
        form.focus = form.fields.iter().position(Field::focusable).unwrap_or(0);
        form
    }

    /// Adopting `a`: into the project holding its directory, or a new one.
    /// It goes on with what it last ran on, the defaults (the project's
    /// model and effort, if it runs the same agent, `default_agent` if it
    /// doesn't pick one), or a model and effort picked here.
    pub fn adopt(a: &crate::adopt::Adoptable, default_agent: &str) -> Form {
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
                Field::new(Id::Project, "Project", text(""), &a.new_project)
            }
        };
        let defaults = a
            .project
            .as_ref()
            .filter(|p| p.agent.as_deref().unwrap_or(default_agent) == c.agent);
        let effort = c
            .effort
            .clone()
            .filter(|e| agent::check_effort(&c.agent, e).is_ok());
        let ran_on = (c.model.clone(), effort);
        let run_on = Field::new(
            Id::RunOn,
            "Settings",
            Kind::Choice(options(CONVERSATION, &[DEFAULTS, OVERRIDES])),
            CONVERSATION,
        );
        let moved = match c.open_in {
            Some(pid) => {
                format!(
                    "move it; it's open in {} (pid {pid}), close it there",
                    crate::util::capitalize(&c.agent)
                )
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
            Field::new(Id::Agent, "Agent", Kind::Info, &c.agent),
            Field::new(Id::Name, "Agent name", text("automatic"), ""),
            run_on,
            model_field(or_default(&defaults.and_then(|p| p.model.clone()))),
            effort_field(or_default(&defaults.and_then(|p| p.effort.clone()))),
            copy,
        ];
        let mut form = Form::build(
            Purpose::Adopt {
                conversation: c.id.clone(),
                open_in: c.open_in,
                ran_on,
                defaults: (
                    defaults.and_then(|p| p.model.clone()),
                    defaults.and_then(|p| p.effort.clone()),
                ),
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
            models: Models::known(),
        };
        form.update();
        form
    }

    /// The models to offer for each agent, and what each is: the default or
    /// an alias as it turned out (learned from sessions; "latest" until
    /// known), any other by what its agent says of it.
    pub fn with_models(mut self, models: &Models) -> Form {
        self.set_models(models);
        self
    }

    /// What's known of the agents' models now, as more arrive (see
    /// `tui::list_models`); what's in the form stays.
    pub fn set_models(&mut self, models: &Models) {
        self.models = models.clone();
        self.update();
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

    /// The default agent: the first of the Agent field's choices (as
    /// `agent::available` lists them), else the agent it's about.
    fn default_agent(&self) -> String {
        match self.field(Id::Agent).map(|f| &f.kind) {
            Some(Kind::Choice(agents)) if !agents.is_empty() => agents[0].clone(),
            _ => self.agent().to_string(),
        }
    }

    /// The agent the form is about: its Agent field's.
    pub fn agent(&self) -> &str {
        self.field(Id::Agent).map_or("", |f| f.value.as_str())
    }

    /// The models the form's agent offers (see `with_models`).
    fn offered(&self) -> &[agent::Model] {
        self.models
            .offered
            .get(self.agent())
            .map_or(&[], Vec::as_slice)
    }

    /// What the agent's model choices are (see `with_models`).
    fn model_notes(&self) -> std::collections::HashMap<String, String> {
        let agent = self.agent();
        let learned = |asked: &str| {
            self.models
                .names
                .get(&agent::learned_key(agent, asked))
                .cloned()
        };
        let mut notes = std::collections::HashMap::new();
        let name = crate::util::capitalize(agent);
        let default = match self.chosen().0 {
            Some(model) => format!("{} (config.toml)", learned(&model).unwrap_or(model)),
            None => learned(DEFAULT).unwrap_or_else(|| format!("{name}'s default")),
        };
        notes.insert(DEFAULT.to_string(), default);
        for m in self.offered() {
            let note = match &m.note {
                Some(note) => note.clone(),
                None => learned(&m.id).unwrap_or_else(|| "latest".into()),
            };
            notes.insert(m.id.clone(), note);
        }
        notes
    }

    /// The model and effort config.toml gives the form's agent's new
    /// sessions.
    fn chosen(&self) -> (Option<String>, Option<String>) {
        self.models
            .chosen
            .get(self.agent())
            .cloned()
            .unwrap_or_default()
    }

    /// What adopting with the conversation's settings and with the defaults
    /// comes to: `ran_on` and `defaults` (the project's), with what isn't
    /// known there from config.toml, else the agent's own.
    fn run_on_notes(
        &self,
        ran_on: (Option<String>, Option<String>),
        defaults: (Option<String>, Option<String>),
    ) -> std::collections::HashMap<String, String> {
        let (model, effort) = self.chosen();
        let name = crate::util::capitalize(self.agent());
        let default_model = defaults
            .0
            .or(model)
            .unwrap_or_else(|| format!("{name}'s default model"));
        let default_effort = defaults
            .1
            .or(effort)
            .unwrap_or_else(|| "its default effort".into());
        let conversation = format!(
            "{} · {}, as it last ran",
            ran_on.0.unwrap_or_else(|| default_model.clone()),
            ran_on.1.unwrap_or_else(|| default_effort.clone())
        );
        [
            (CONVERSATION.to_string(), conversation),
            (
                DEFAULTS.to_string(),
                format!("{default_model} · {default_effort}"),
            ),
            (
                OVERRIDES.to_string(),
                "the model and effort below".to_string(),
            ),
        ]
        .into()
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
        let offered: Vec<String> = self.offered().iter().map(|m| m.id.clone()).collect();
        if let Some(f) = self.field_mut(Id::Model) {
            f.hidden = choices.is_none();
            let offered: Vec<&str> = offered.iter().map(String::as_str).collect();
            f.kind = Kind::Combo(options(DEFAULT, &offered));
            f.notes = notes;
        }
        // Codex takes its first prompt on its command line (`takes_prompt`).
        let given = agent::adapter(self.agent()).takes_prompt();
        if let Some(Field {
            kind: Kind::Text { placeholder, .. },
            ..
        }) = self.field_mut(Id::Prompt)
        {
            *placeholder = match given {
                true => "optional: it starts with it".into(),
                false => "optional: typed in once the agent is ready".into(),
            };
        }
        let chosen_effort = self.chosen().1;
        if let Some(f) = self.field_mut(Id::Effort) {
            f.hidden = choices.is_none();
            f.kind = Kind::Choice(options(DEFAULT, choices.map_or(&[][..], |c| c.efforts)));
            f.notes = chosen_effort
                .map(|e| (DEFAULT.to_string(), format!("{e} (config.toml)")))
                .into_iter()
                .collect();
        }
        // Adopting: a model and effort to pick only for overrides; what the
        // others are, noted.
        if let Purpose::Adopt {
            ran_on, defaults, ..
        } = &self.purpose
        {
            let (ran_on, defaults) = (ran_on.clone(), defaults.clone());
            let overrides = self.value(Id::RunOn) == OVERRIDES;
            for id in [Id::Model, Id::Effort] {
                if let Some(f) = self.field_mut(id) {
                    f.hidden |= !overrides;
                }
            }
            let notes = self.run_on_notes(ran_on, defaults);
            if let Some(f) = self.field_mut(Id::RunOn) {
                f.hidden = choices.is_none();
                f.notes = notes;
            }
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
        // A new session takes its project's defaults, unless changed here:
        // its agent, and for that agent its model and effort.
        if self.purpose != Purpose::NewSession {
            return;
        }
        let project = self.value(Id::Project).to_string();
        let Some(p) = self.projects.iter().find(|p| p.name == project).cloned() else {
            return;
        };
        let project_agent = p.agent.clone().unwrap_or_else(|| self.default_agent());
        if self.filled_from.as_ref().map(|(p, _)| p) != Some(&project)
            && let Some(f) = self.field_mut(Id::Agent).filter(|f| !f.edited)
        {
            f.value = project_agent.clone();
        }
        let filled = (project, self.agent().to_string());
        if self.filled_from.as_ref() == Some(&filled) {
            return;
        }
        let (model, effort) = if filled.1 == project_agent {
            (or_default(&p.model), or_default(&p.effort))
        } else {
            (DEFAULT.to_string(), DEFAULT.to_string())
        };
        for (id, value) in [(Id::Model, model), (Id::Effort, effort)] {
            if let Some(f) = self.field_mut(id).filter(|f| !f.edited) {
                f.value = value;
            }
        }
        self.filled_from = Some(filled);
        // The agent may have changed.
        self.update();
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Outcome {
        self.note = None;
        if input::cancels(e) {
            return Outcome::Cancel;
        }
        match e.code {
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
            _ => self.edit(e),
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

    fn edit(&mut self, e: KeyEvent) {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        let before = field.value.clone();
        let code = e.code;
        match (&field.kind, code) {
            (Kind::Choice(opts) | Kind::Combo(opts), KeyCode::Left | KeyCode::Right) => {
                field.value = cycle(opts, &field.value, code == KeyCode::Right);
            }
            (Kind::Choice(opts), KeyCode::Char('h' | 'l' | ' ')) => {
                field.value = cycle(opts, &field.value, code != KeyCode::Char('h'));
            }
            (Kind::Text { .. } | Kind::Combo(_), _) => {
                // Typing replaces "default" rather than appending to it.
                let typed = matches!(code, KeyCode::Char(_))
                    && !e
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                if matches!(field.kind, Kind::Combo(_)) && field.value == DEFAULT && typed {
                    field.value.clear();
                }
                if !input::edit_line(&mut field.value, e) {
                    field.value = before;
                    return;
                }
            }
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
        let default = self.default_agent();
        let agent = || self.setting(Id::Agent).filter(|a| *a != default);
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
                ..
            } => {
                let overrides = self.value(Id::RunOn) == OVERRIDES;
                Op::Adopt {
                    conversation: conversation.clone(),
                    project: required(Id::Project, "The project")?,
                    name: self.setting(Id::Name),
                    run_on: if self.value(Id::RunOn) == DEFAULTS {
                        RunOn::Defaults
                    } else {
                        RunOn::Conversation
                    },
                    model: overrides.then(|| self.setting(Id::Model)),
                    effort: overrides.then(|| self.setting(Id::Effort)),
                    fork: self.value(Id::Copy) == "yes",
                    // Asked first (see App).
                    force: open_in.is_some(),
                }
            }
            Purpose::EditSession(id) => {
                // Only what was changed here changes.
                let changed = |id: Id| self.field(id).is_some_and(|f| f.edited);
                Op::EditSession {
                    id: id.clone(),
                    name: required(Id::Name, "The agent name")?,
                    title: self.setting(Id::Title),
                    model: changed(Id::Model).then(model),
                    effort: changed(Id::Effort).then(effort),
                }
            }
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
        // Typing replaces "default", a capital (with Shift) too.
        form.on_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        assert_eq!(form.value(Id::Model), "X");
        form.on_key(key(KeyCode::Backspace));
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
    fn each_agent_offers_its_own_models() {
        let projects = vec![project("alpha", None)];
        let mut models = Models::known();
        models.offered.insert(
            "pi".into(),
            vec![agent::Model {
                id: "claude-opus-4-8".into(),
                note: Some("1M context".into()),
            }],
        );
        let agents = vec!["claude".to_string(), "pi".to_string()];
        let mut form =
            Form::new_session(&projects, Some(&projects[0]), &agents).with_models(&models);
        form.on_key(key(KeyCode::Right)); // pi
        assert_eq!(form.value(Id::Agent), "pi");
        let model = form.fields.iter().position(|f| f.id == Id::Model).unwrap();
        form.focus = model;
        form.on_key(key(KeyCode::Right));
        assert_eq!(form.value(Id::Model), "claude-opus-4-8");
        assert_eq!(form.fields[model].notes["claude-opus-4-8"], "1M context");
        assert_eq!(form.fields[model].notes["default"], "Pi's default");
        // Back to Claude: its own list, its aliases first.
        form.focus = 1;
        form.on_key(key(KeyCode::Left));
        form.focus = model;
        form.on_key(key(KeyCode::Left));
        form.on_key(key(KeyCode::Right));
        assert_eq!(form.value(Id::Model), "opus");
        // What "default" is when config.toml picks one for the agent.
        models
            .chosen
            .insert("claude".into(), (Some("sonnet".into()), Some("low".into())));
        models.names.insert("sonnet".into(), "Sonnet 5.5".into());
        let form = Form::new_session(&projects, Some(&projects[0]), &agents).with_models(&models);
        assert_eq!(
            form.field(Id::Model).unwrap().notes["default"],
            "Sonnet 5.5 (config.toml)"
        );
        assert_eq!(
            form.field(Id::Effort).unwrap().notes["default"],
            "low (config.toml)"
        );
    }

    #[test]
    fn new_session_takes_the_projects_defaults() {
        let projects = vec![project("alpha", Some("opus")), project("beta", None)];
        let mut form = Form::new_session(&projects, Some(&projects[0]), &agents());
        // The agent comes first, then its name.
        assert_eq!(form.fields[form.focus].id, Id::Agent);
        assert_eq!(form.fields[form.focus + 1].label, "Agent name");
        assert_eq!(
            (form.value(Id::Model), form.value(Id::Effort)),
            ("opus", "high")
        );
        // Another agent: not the project's model and effort, which are its agent's.
        form.focus = 1;
        form.on_key(key(KeyCode::Right));
        assert_eq!(form.value(Id::Agent), "aider");
        form.on_key(key(KeyCode::Left));
        assert_eq!(form.value(Id::Agent), "claude");
        assert_eq!(form.value(Id::Model), "opus");
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
        a.conversation.model = Some("claude-sonnet-4-6".into());
        a.conversation.effort = Some("xhigh".into());
        let mut form = Form::adopt(&a, "claude");
        // The project is known: the first thing to fill in is the name.
        assert_eq!(form.fields[form.focus].id, Id::Name);
        assert!(matches!(form.field(Id::Project).unwrap().kind, Kind::Info));
        // It goes on with what it ran on, or the defaults, or what's picked.
        let run_on = form.field(Id::RunOn).unwrap();
        assert_eq!(run_on.value, "conversation");
        assert_eq!(
            run_on.notes["conversation"],
            "claude-sonnet-4-6 · xhigh, as it last ran"
        );
        assert_eq!(run_on.notes["defaults"], "opus · its default effort");
        assert!(form.field(Id::Model).unwrap().hidden);
        type_text(&mut form, "parser");
        form.focus = form.fields.iter().position(|f| f.id == Id::Copy).unwrap();
        form.on_key(key(KeyCode::Right));
        assert_eq!(
            submit(&mut form),
            Op::Adopt {
                conversation: "c1".into(),
                project: "alpha".into(),
                name: Some("parser".into()),
                run_on: RunOn::Conversation,
                model: None,
                effort: None,
                fork: true,
                force: false,
            }
        );
        let select = |form: &mut Form, value: &str| {
            form.focus = form.fields.iter().position(|f| f.id == Id::RunOn).unwrap();
            while form.value(Id::RunOn) != value {
                form.on_key(key(KeyCode::Right));
            }
        };
        select(&mut form, "defaults");
        assert!(matches!(
            submit(&mut form),
            Op::Adopt {
                run_on: RunOn::Defaults,
                model: None,
                ..
            }
        ));
        // Overrides: the model and effort to pick, the project's to start with.
        select(&mut form, "overrides");
        assert!(!form.field(Id::Model).unwrap().hidden);
        assert_eq!(form.value(Id::Model), "opus");
        assert!(matches!(
            submit(&mut form),
            Op::Adopt {
                model: Some(Some(_)),
                effort: Some(None),
                ..
            }
        ));
        // Outside a project: the new project's name comes first, and can't be empty.
        let mut form = Form::adopt(&adoptable("c2", "Elsewhere", None), "claude");
        assert_eq!(form.fields[form.focus].id, Id::Project);
        assert_eq!(form.value(Id::Project), "elsewhere");
        form.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(matches!(
            form.on_key(key(KeyCode::Enter)),
            Outcome::Continue
        ));
        assert!(form.error.is_some());
        // A project of another agent: its model isn't this one's.
        let form = Form::adopt(&a, "pi");
        assert_eq!(form.value(Id::Model), "default");
        assert_eq!(
            form.field(Id::RunOn).unwrap().notes["defaults"],
            "Claude's default model · its default effort"
        );
    }

    #[test]
    fn models_arriving_later_reach_an_open_form() {
        let projects = [project("api", None)];
        let agents = vec!["claude".to_string(), "pi".to_string()];
        let mut form = Form::new_session(&projects, None, &agents).with_models(&Models::known());
        form.on_key(key(KeyCode::Right)); // pi
        let model = form.fields.iter().position(|f| f.id == Id::Model).unwrap();
        form.fields[model].value = "typed-model".into();
        let mut models = Models::known();
        models.offered.insert(
            "pi".into(),
            vec![agent::Model {
                id: "fake-model-1".into(),
                note: None,
            }],
        );
        form.set_models(&models);
        let Kind::Combo(options) = &form.fields[model].kind else {
            panic!("not a combo");
        };
        assert!(options.contains(&"fake-model-1".to_string()), "{options:?}");
        // What was typed stays.
        assert_eq!(form.fields[model].value, "typed-model");
    }

    #[test]
    fn the_first_prompt_says_how_the_agent_gets_it() {
        let projects = [project("api", None)];
        let agents = vec!["claude".to_string(), "codex".to_string()];
        let mut form = Form::new_session(&projects, None, &agents);
        let placeholder = |form: &Form| match &form.field(Id::Prompt).unwrap().kind {
            Kind::Text { placeholder, .. } => placeholder.clone(),
            _ => panic!("not text"),
        };
        assert_eq!(
            placeholder(&form),
            "optional: typed in once the agent is ready"
        );
        form.on_key(key(KeyCode::Right)); // codex
        assert_eq!(placeholder(&form), "optional: it starts with it");
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
                model: None,
                effort: None
            }
        );
        // Changed to the default: so it says.
        form.focus = form.fields.iter().position(|f| f.id == Id::Model).unwrap();
        form.on_key(key(KeyCode::Left));
        assert_eq!(form.value(Id::Model), "default");
        assert!(matches!(
            submit(&mut form),
            Op::EditSession {
                model: Some(None),
                effort: None,
                ..
            }
        ));
    }
}
