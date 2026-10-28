//! `config.toml`: every setting is optional. Changed by hand, or by roster's
//! settings view and `roster config` (`settings`).
//!
//! ```toml
//! shell = "/usr/bin/zsh"          # sources the env files; default: $SHELL if POSIX-like
//! chats_dir = "~/chats"           # where chats run; default: roster's state directory
//! default_agent = "pi"            # for new sessions, unless the project says; default claude
//! status_line = false             # default true: while attached, a line at the top
//!                                 # with the project, session and title
//! fit_preview = "never"           # "auto" (default): agents drawing full screen are
//!                                 # sized to the TUI's preview while no one's attached
//! account_limits = false          # default true: asks your Claude and codex accounts
//!                                 # what's left of their limits (claude -p, codex
//!                                 # app-server)
//! sidebar_usage = false           # default true: under the session list, what your
//!                                 # Claude and codex accounts have used of their limits
//! trust_project_folders = true    # default false: a project's folder trusted in Claude
//!                                 # and codex as a session starts there, as their own
//!                                 # question would (written in their configs)
//! group_by = "project"            # "attention" (default): the list on shelves, by what
//!                                 # waits for you; "project": under each project
//! card_subagents = "unfolded"     # a session's subagents at work, on its card:
//!                                 # "folded" (default): how many, and a line each
//!                                 # once unfolded (l); "unfolded": the lines always;
//!                                 # "count": how many only
//!
//! [order]                         # within each shelf: "oldest" or "newest" first
//! needs_input = "oldest"          # the defaults: first come, first served,
//! idle = "oldest"
//! stopped = "oldest"
//! working = "oldest"              # (the one working longest on top)
//! archived = "newest"             # and the most recently archived on top
//!
//! [notes]                         # a session's note (s o), a project's (p o)
//! editor = "external"             # "builtin" (default): roster's own editor, with
//!                                 # vim's keys; "external": $VISUAL or $EDITOR
//! escape = "jk"                   # typed quickly in insert mode: as Esc (default none)
//! escape_ms = 300                 # how quickly: at most this long between the keys
//! lines = 2                       # of each note the preview shows, from the pinned one
//!
//! [keys]                          # TUI keys: one, a list, or "" to disable
//! detach = "C-q"                  # back to roster while attached (a tmux key)
//! down = ["j", "Down"]
//! [keys.session]                  # a menu: `menu` opens it, the rest act inside
//! menu = "s"
//! delete = ""                     # no key for deleting sessions for good
//!
//! [agents.claude]
//! command = "claude"              # default: the agent's name, found on PATH
//! args = ["--verbose"]            # added to every launch
//! model = "sonnet"                # new sessions', unless they or their project pick
//! effort = "high"                 # one; default: the agent's own
//! title_model = "haiku"           # makes generated titles; default: model above
//! [agents.pi]                     # pi is offered once installed; this only adds to it
//! args = ["--tui-mode", "regular"]
//! title_model = "claude-haiku-4-5"
//! auto_title = false              # no title made after its first turn (default true)
//!
//! [agents.aider]                  # any other agent: state from its screen
//! idle_after = 4                  # seconds without screen changes = idle
//! fit_preview = "never"           # also per agent; the agent's wins
//!
//! [prices."gpt-5-codex"]          # what a model's calls cost, in dollars per million
//! input = 1.25                    # tokens: for one roster has no price for, or in
//! output = 10.0                   # place of roster's own
//! cache_read = 0.125              # default 0, as cache_write
//! [prices."gpt-5-codex".long]     # a call sending more than `above` tokens: all of
//! above = 272000                  # it at these
//! input = 2.5
//! output = 15.0
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub shell: Option<String>,
    /// Where chats (sessions without a project of their own) run.
    pub chats_dir: Option<String>,
    pub order: Order,
    pub keys: Keys,
    pub agents: BTreeMap<String, AgentConfig>,
    pub fit_preview: Option<FitPreview>,
    /// What keys roster has by default gave way to the config's (`gone`).
    #[serde(skip)]
    pub yielded: Vec<String>,
    /// While attached, a line at the top saying where you are (default on).
    pub status_line: Option<bool>,
    /// Whether the agents' accounts are asked what's left of their limits
    /// (default on), of Claude and codex themselves.
    pub account_limits: Option<bool>,
    /// Under the session list, a line with what the agents' accounts have
    /// used of their limits (default on).
    pub sidebar_usage: Option<bool>,
    /// A session's folder trusted in its agent (Claude, codex) as it
    /// starts, in the agent's own config, so it doesn't ask (default off).
    pub trust_project_folders: Option<bool>,
    /// The agent new sessions get unless the project or the user picks one.
    pub default_agent: Option<String>,
    pub notes: Notes,
    /// How the session list is grouped (default: by attention, on shelves).
    pub group_by: Option<GroupBy>,
    /// What a card says of its session's subagents at work (default: their
    /// count, and a line each once unfolded).
    pub card_subagents: Option<CardSubagents>,
    /// What models' calls cost, by model: in place of roster's own prices,
    /// or for models it has none for (`tokens::Prices`).
    pub prices: BTreeMap<String, Price>,
}

/// What a model's calls cost, in dollars per million tokens.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    /// What all of a call sending more tokens than its `above` costs.
    pub long: Option<LongPrice>,
}

/// A model's prices for a long call: one sending more than `above` tokens.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LongPrice {
    pub above: u64,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

/// How the session list is grouped: on shelves by what waits for you, or
/// under each project.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    #[default]
    Attention,
    Project,
}

/// What a card says of its session's subagents at work: their count only;
/// that, and a line each when unfolded (`l`); or the lines always.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CardSubagents {
    Count,
    #[default]
    Folded,
    Unfolded,
}

/// Sessions' and projects' notes: where they open, and the editor's keys.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notes {
    pub editor: Option<NoteEditor>,
    /// Keys typed quickly in insert mode that leave it, as Esc does (vim's
    /// `inoremap jk <Esc>`).
    pub escape: Option<String>,
    /// How quickly: at most this many milliseconds from one key to the next.
    pub escape_ms: Option<u64>,
    /// How many of a note's lines the preview shows, from the pinned one.
    pub lines: Option<usize>,
}

/// `escape_ms` unless set.
pub const ESCAPE_MS: u64 = 300;

/// `lines` unless set, and the most it can be: more would push the agent's
/// screen out of the preview.
pub const NOTE_LINES: usize = 2;
pub const MAX_NOTE_LINES: usize = 10;

impl Notes {
    pub fn editor(&self) -> NoteEditor {
        self.editor.unwrap_or_default()
    }

    pub fn lines(&self) -> usize {
        self.lines.unwrap_or(NOTE_LINES)
    }

    /// The keys that leave insert mode, and how soon each must follow the one
    /// before.
    pub fn escape(&self) -> Option<(Vec<char>, std::time::Duration)> {
        let keys = self.escape.as_ref()?.chars().collect();
        let ms = self.escape_ms.unwrap_or(ESCAPE_MS);
        Some((keys, std::time::Duration::from_millis(ms)))
    }
}

/// Where a note opens: in roster's own editor (vim's keys, the
/// agent's screen still in view), or in `$VISUAL` / `$EDITOR`.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NoteEditor {
    #[default]
    Builtin,
    External,
}

/// Whether an agent drawing full screen (in the terminal's alternate screen,
/// as Claude's fullscreen renderer does) gets the TUI preview's size while no
/// one is attached, so its screen fits instead of being cut.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FitPreview {
    Auto,
    Never,
}

/// Which comes first within a shelf: the session that got there earliest, or
/// latest (archived: by when it was archived).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum First {
    Oldest,
    Newest,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Order {
    pub needs_input: First,
    pub idle: First,
    pub stopped: First,
    pub working: First,
    pub archived: First,
}

impl Default for Order {
    fn default() -> Self {
        Order {
            needs_input: First::Oldest,
            idle: First::Oldest,
            stopped: First::Oldest,
            working: First::Oldest,
            archived: First::Newest,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    /// Back to roster while attached; bound in roster's tmux, so a tmux key name.
    pub detach: String,
    pub up: KeyList,
    pub down: KeyList,
    /// Fold the selected shelf or project group (from a session: its group).
    pub collapse: KeyList,
    pub expand: KeyList,
    /// Attach to the selected session, open or fold a heading, restore when archived.
    pub confirm: KeyList,
    /// Filter the session list by typing.
    pub search: KeyList,
    pub quit: KeyList,
    pub help: KeyList,
    pub session: SessionKeys,
    pub project: ProjectKeys,
    pub view: ViewKeys,
}

/// The session menu: `menu` opens it, the others act inside it.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionKeys {
    pub menu: KeyList,
    pub new: KeyList,
    pub edit: KeyList,
    pub start: KeyList,
    pub stop: KeyList,
    pub restart: KeyList,
    /// The key that gave a first prompt again, gone (starting asks):
    /// taken, and said to be gone (`Config::gone`).
    pub prompt: Option<toml::Value>,
    /// Archive a session, or restore an archived one.
    pub archive: KeyList,
    /// Delete a session for good, archived or not.
    pub delete: KeyList,
    pub toggle_read: KeyList,
    pub retitle: KeyList,
    pub adopt: KeyList,
    /// A new chat: a session in the built-in chats project.
    pub chat: KeyList,
    /// The session's note, to read and write.
    pub note: KeyList,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectKeys {
    pub menu: KeyList,
    pub new: KeyList,
    pub edit: KeyList,
    pub remove: KeyList,
    pub filter: KeyList,
    /// The projects view.
    pub list: KeyList,
    /// The project's note, to read and write.
    pub note: KeyList,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewKeys {
    pub menu: KeyList,
    /// The projects view (also in the project menu).
    pub projects: KeyList,
    pub server: KeyList,
    /// Every setting, to see and change.
    pub settings: KeyList,
    /// What the agents' accounts have used of their limits.
    pub usage: KeyList,
    /// The key that grouped the list by project, gone (`group_by` does it):
    /// taken, and said to be gone (`Config::gone`).
    pub group: Option<toml::Value>,
}

fn keys(names: &[&str]) -> KeyList {
    KeyList(names.iter().map(|n| n.to_string()).collect())
}

impl Default for Keys {
    fn default() -> Self {
        Keys {
            detach: "C-q".into(),
            up: keys(&["k", "Up"]),
            down: keys(&["j", "Down"]),
            collapse: keys(&["h", "Left"]),
            expand: keys(&["l", "Right"]),
            confirm: keys(&["Enter"]),
            search: keys(&["/"]),
            quit: keys(&["q", "C-c"]),
            help: keys(&["?"]),
            session: SessionKeys::default(),
            project: ProjectKeys::default(),
            view: ViewKeys::default(),
        }
    }
}

impl Default for SessionKeys {
    fn default() -> Self {
        SessionKeys {
            menu: keys(&["s"]),
            new: keys(&["n"]),
            edit: keys(&["e"]),
            start: keys(&["s"]),
            stop: keys(&["x"]),
            restart: keys(&["r"]),
            prompt: None,
            archive: keys(&["a"]),
            delete: keys(&["D"]),
            toggle_read: keys(&["u"]),
            retitle: keys(&["t"]),
            adopt: keys(&["A"]),
            chat: keys(&["c"]),
            note: keys(&["o"]),
        }
    }
}

impl SessionKeys {
    /// The session menu's keys, by setting (`menu`, which opens it, is the
    /// list's).
    fn menu_keys(&mut self) -> [(&'static str, &mut KeyList); 12] {
        [
            ("new", &mut self.new),
            ("chat", &mut self.chat),
            ("edit", &mut self.edit),
            ("start", &mut self.start),
            ("stop", &mut self.stop),
            ("restart", &mut self.restart),
            ("toggle_read", &mut self.toggle_read),
            ("retitle", &mut self.retitle),
            ("note", &mut self.note),
            ("archive", &mut self.archive),
            ("delete", &mut self.delete),
            ("adopt", &mut self.adopt),
        ]
    }

    /// A key the config sets for one action that archive or adopt has by
    /// default: their default gives it way, so a config written before they
    /// had it still loads, and that's said. Any other default the config sets
    /// for another action stays an error, as do two the config sets
    /// (`Keymap::new`).
    fn yield_defaults(&mut self) -> Vec<String> {
        const MOVED: [&str; 2] = ["archive", "adopt"];
        let mut defaults = SessionKeys::default();
        let defaults: Vec<Vec<String>> = (defaults.menu_keys().into_iter())
            .map(|(_, list)| list.0.clone())
            .collect();
        let mut keys = self.menu_keys();
        let set: Vec<bool> = (keys.iter().zip(&defaults))
            .map(|((_, list), default)| list.0 != *default)
            .collect();
        let taken: Vec<(crate::keys::Key, &str)> = (keys.iter().zip(&set))
            .filter(|(_, set)| **set)
            .flat_map(|((name, list), _)| {
                (list.0.iter()).filter_map(|k| Some((crate::keys::Key::parse(k).ok()?, *name)))
            })
            .collect();
        let mut said = Vec::new();
        for ((name, list), set) in keys.iter_mut().zip(&set) {
            if *set || !MOVED.contains(name) {
                continue;
            }
            list.0.retain(|k| {
                let key = crate::keys::Key::parse(k).ok();
                let Some((_, by)) = taken.iter().find(|(t, _)| Some(*t) == key) else {
                    return true;
                };
                said.push(format!(
                    "keys.session.{by} takes {k:?}, {name}'s key by default, so {name} goes \
                     without it: give it one in config.toml (roster config --edit)"
                ));
                false
            });
        }
        said
    }
}

impl Default for ProjectKeys {
    fn default() -> Self {
        ProjectKeys {
            menu: keys(&["p"]),
            new: keys(&["n"]),
            edit: keys(&["e"]),
            remove: keys(&["d"]),
            filter: keys(&["f"]),
            list: keys(&["l"]),
            note: keys(&["o"]),
        }
    }
}

impl Default for ViewKeys {
    fn default() -> Self {
        ViewKeys {
            menu: keys(&["v"]),
            projects: keys(&["p"]),
            server: keys(&["t"]),
            settings: keys(&["s"]),
            usage: keys(&["u"]),
            group: None,
        }
    }
}

/// The keys of one TUI action: `"x"`, `["x", "Down"]`, or `""` / `[]` for none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyList(pub Vec<String>);

impl<'de> Deserialize<'de> for KeyList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        let names = match Raw::deserialize(d)? {
            Raw::One(name) => vec![name],
            Raw::Many(names) => names,
        };
        Ok(KeyList(
            names.into_iter().filter(|n| !n.is_empty()).collect(),
        ))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Seconds without screen changes after which an agent without hooks is idle.
    pub idle_after: Option<u64>,
    pub fit_preview: Option<FitPreview>,
    /// The model and effort new sessions get unless they or their project
    /// pick one (else the agent's own defaults), for agents that take them.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The model a generated title is made with (by the agent's one-off
    /// mode); else `model`, else the agent's own default.
    pub title_model: Option<String>,
    /// For an agent that doesn't name its task itself (pi): a title made
    /// after its first turn (default true).
    pub auto_title: Option<bool>,
}

impl AgentConfig {
    /// The model generated titles are made with, if one is picked:
    /// `title_model`, else `model` (else the agent's own default).
    pub fn titles_by(&self) -> Option<String> {
        self.title_model.clone().or_else(|| self.model.clone())
    }
}

/// The config file's text; "" if there's none.
pub fn read_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("can't read {}", path.display())),
    }
}

impl Config {
    /// Reads `path`, with the text it came from; a missing file means all
    /// defaults ("").
    pub fn load_text(path: &Path) -> Result<(Config, String)> {
        let text = read_text(path)?;
        let config =
            Config::parse(&text).with_context(|| format!("invalid config {}", path.display()))?;
        Ok((config, text))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let mut config: Config = toml::from_str(text)?;
        check_detach(&config.keys.detach)?;
        config.yielded = config.keys.session.yield_defaults();
        crate::keys::Keymap::new(&config.keys)?;
        if let Some(agent) = &config.default_agent
            && !crate::agent::BUILT_IN.contains(&agent.as_str())
            && !config.agents.contains_key(agent)
        {
            bail!(
                "default_agent {agent:?} isn't one roster knows: {} or one under [agents]",
                crate::agent::BUILT_IN.join(", ")
            );
        }
        if let Some(escape) = &config.notes.escape
            && (escape.chars().count() < 2
                || escape.chars().any(|c| c.is_whitespace() || c.is_control()))
        {
            bail!(
                "notes.escape must be two or more keys typed in a row, such as \"jk\", not {escape:?}"
            );
        }
        if config.notes.escape_ms == Some(0) {
            bail!("notes.escape_ms must be at least 1 (milliseconds)");
        }
        if let Some(lines) = config.notes.lines
            && !(1..=MAX_NOTE_LINES).contains(&lines)
        {
            bail!("notes.lines must be 1 to {MAX_NOTE_LINES}, not {lines}");
        }
        for (name, agent) in &config.agents {
            if agent.idle_after == Some(0) {
                bail!("agents.{name}.idle_after must be at least 1 (seconds)");
            }
            if crate::agent::adapter(name).choices().is_none()
                && (agent.model.is_some() || agent.effort.is_some())
            {
                bail!("agents.{name}: {name} is started without a model or effort to pick");
            }
            for (key, model) in [("model", &agent.model), ("title_model", &agent.title_model)] {
                if model
                    .as_deref()
                    .is_some_and(|m| m.trim().is_empty() || m == "default")
                {
                    bail!("agents.{name}.{key}: leave it out for {name}'s own default");
                }
            }
            if let Some(effort) = &agent.effort {
                crate::agent::check_effort(name, effort)
                    .with_context(|| format!("agents.{name}.effort"))?;
            }
        }
        for (model, price) in &config.prices {
            if model.trim().is_empty() {
                bail!("prices: a model needs a name");
            }
            let long = price.long.as_ref();
            let rates = [
                ("input", Some(price.input)),
                ("output", Some(price.output)),
                ("cache_read", Some(price.cache_read)),
                ("cache_write", Some(price.cache_write)),
                ("long.input", long.map(|l| l.input)),
                ("long.output", long.map(|l| l.output)),
                ("long.cache_read", long.map(|l| l.cache_read)),
                ("long.cache_write", long.map(|l| l.cache_write)),
            ];
            for (key, rate) in rates {
                // A million dollars a million tokens: past any price, and
                // far from where a cost would overflow.
                if let Some(rate) = rate
                    && !(rate.is_finite() && (0.0..=1e6).contains(&rate))
                {
                    bail!("prices.{model:?}.{key} must be dollars per million tokens, not {rate}");
                }
            }
            if long.is_some_and(|l| l.above == 0) {
                bail!("prices.{model:?}.long.above must be a number of tokens, at least 1");
            }
        }
        Ok(config)
    }

    /// What it sets that roster no longer has, to say: taken all the same,
    /// so a config that set it still works. And keys it sets that roster's
    /// defaults had, which those gave way to.
    pub fn gone(&self) -> Vec<String> {
        let mut gone = self.yielded.clone();
        if self.keys.view.group.is_some() {
            gone.push(
                "keys.view.group is gone: group_by = \"project\" groups the list by \
                 project; take it out of config.toml (roster config --edit)"
                    .to_string(),
            );
        }
        if self.keys.session.prompt.is_some() {
            gone.push(
                "keys.session.prompt is gone: starting a session whose first prompt \
                 wasn't seen taken asks whether to give it again; take it out of \
                 config.toml (roster config --edit)"
                    .to_string(),
            );
        }
        gone
    }

    pub fn status_line(&self) -> bool {
        self.status_line.unwrap_or(true)
    }

    pub fn account_limits(&self) -> bool {
        self.account_limits.unwrap_or(true)
    }

    pub fn trust_project_folders(&self) -> bool {
        self.trust_project_folders.unwrap_or(false)
    }

    pub fn sidebar_usage(&self) -> bool {
        self.sidebar_usage.unwrap_or(true)
    }

    pub fn card_subagents(&self) -> CardSubagents {
        self.card_subagents.unwrap_or_default()
    }

    /// Whether the session list is grouped by project.
    pub fn grouped(&self) -> bool {
        self.group_by.unwrap_or_default() == GroupBy::Project
    }

    /// The agent sessions get unless their project picks one: `default_agent`
    /// if it can be started (a config shared between machines can name one
    /// that isn't installed on this one), else Claude.
    pub fn default_agent(&self) -> &str {
        match &self.default_agent {
            Some(agent) if crate::agent::offered(agent, self) => agent,
            _ => crate::agent::CLAUDE,
        }
    }

    pub fn agent(&self, name: &str) -> AgentConfig {
        self.agents.get(name).cloned().unwrap_or_default()
    }
}

/// The names tmux gives keys other than letters, digits and F1 to F12.
const TMUX_KEYS: &[&str] = &[
    "Enter", "Escape", "Tab", "BTab", "Space", "BSpace", "Up", "Down", "Left", "Right", "Home",
    "End", "PageUp", "PgUp", "PPage", "PageDown", "PgDn", "NPage", "Insert", "IC", "Delete", "DC",
];

/// `keys.detach` is bound in roster's tmux, over whatever the agent would
/// get: a key tmux has a name for (or roster's tmux has no way back at
/// all), with Ctrl or Alt unless it's an F key, so the agents keep their
/// plain keys, and not their interrupt, C-c.
fn check_detach(key: &str) -> Result<()> {
    let (mut base, mut modified) = (key, false);
    while let Some((m, rest)) = ["C-", "M-", "S-"]
        .iter()
        .find_map(|m| Some((*m, base.strip_prefix(m).filter(|r| !r.is_empty())?)))
    {
        modified |= m != "S-";
        base = rest;
    }
    let f_key = base
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=12).contains(&n));
    let single = base.len() == 1 && base.chars().all(|c| c.is_ascii_alphanumeric());
    if !(f_key || single || TMUX_KEYS.contains(&base)) {
        bail!("keys.detach must be a key tmux knows, such as \"C-q\", not {key:?}");
    }
    if !modified && !f_key {
        bail!("keys.detach needs C- or M- (or an F key), or the agents lose that key: not {key:?}");
    }
    if key.eq_ignore_ascii_case("C-c") {
        bail!("keys.detach can't be C-c: that's how you interrupt an agent");
    }
    if ["C-m", "C-j", "C-i", "C-h"]
        .iter()
        .any(|k| key.eq_ignore_ascii_case(k))
    {
        bail!("keys.detach can't be {key}: terminals send it for Enter, Tab or Backspace");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_has_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.keys.detach, "C-q");
        assert!(config.shell.is_none());
        assert!(config.agent("claude").command.is_none());
        assert!(config.status_line());
        assert!(!Config::parse("status_line = false").unwrap().status_line());
    }

    #[test]
    fn reads_agents_and_keys() {
        let config = Config::parse(
            r#"
            shell = "/bin/bash"
            [keys]
            detach = "M-q"
            [agents.claude]
            command = "/opt/claude"
            args = ["--model", "haiku"]
            "#,
        )
        .unwrap();
        assert_eq!(config.keys.detach, "M-q");
        assert_eq!(
            config.agent("claude").command.as_deref(),
            Some("/opt/claude")
        );
        assert_eq!(config.agent("claude").args, ["--model", "haiku"]);
    }

    #[test]
    fn shelf_order_is_oldest_first_but_archived_newest() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.order, Order::default());
        assert_eq!(
            (config.order.needs_input, config.order.archived),
            (First::Oldest, First::Newest)
        );
        let config = Config::parse("[order]\nworking = \"newest\"").unwrap();
        assert_eq!(
            (config.order.working, config.order.idle),
            (First::Newest, First::Oldest)
        );
        assert!(Config::parse("[order]\nworking = \"random\"").is_err());
        assert!(Config::parse("[order]\nnope = \"oldest\"").is_err());
    }

    #[test]
    fn tui_keys_take_one_several_or_none() {
        let config = Config::parse(
            "[keys]\ndown = [\"j\", \"PageDown\"]\n[keys.session]\nstop = \"k\"\ndelete = \"\"",
        )
        .unwrap();
        // Menu keys may repeat top-level ones: they only act inside the menu.
        assert_eq!(config.keys.session.stop, KeyList(vec!["k".into()]));
        assert_eq!(config.keys.down.0, ["j", "PageDown"]);
        assert!(config.keys.session.delete.0.is_empty());
        assert_eq!(config.keys.quit.0, ["q", "C-c"]);
        assert!(Config::parse("[keys.session]\nstop = \"Nope\"").is_err());
        // Within one menu, or among top-level keys and menu openers, a key is unique.
        assert!(Config::parse("[keys.session]\nstop = \"n\"").is_err());
        assert!(Config::parse("[keys]\nhelp = \"s\"").is_err());
    }

    #[test]
    fn the_readme_lists_the_default_keys() {
        // The README's [keys] example is the defaults, word for word.
        let readme = include_str!("../README.md");
        let start = readme
            .find("    ```toml\n    [keys]\n")
            .expect("the README's [keys]");
        let block = &readme[start + "    ```toml\n".len()..];
        let block = &block[..block.find("    ```").unwrap()];
        let toml: String = block
            .lines()
            .map(|l| l.trim_start().to_string() + "\n")
            .collect();
        let config = Config::parse(&toml).unwrap();
        let defaults = format!("{:?}", Keys::default());
        assert_eq!(format!("{:?}", config.keys), defaults);
        // Each of them: one left out would take its default unseen. As many
        // as there are, none unknown (that fails to parse) nor twice.
        fn keys(table: &toml::Table) -> usize {
            (table.values()).map(|v| v.as_table().map_or(1, keys)).sum()
        }
        let table: toml::Table = toml.parse().unwrap();
        let there = defaults.matches(": KeyList(").count() + defaults.matches("detach: ").count();
        assert_eq!(keys(&table), there, "{block}");
    }

    #[test]
    fn an_agents_model_and_effort_are_its_own() {
        let config =
            Config::parse("[agents.pi]\nmodel = \"sonnet\"\neffort = \"minimal\"").unwrap();
        assert_eq!(config.agent("pi").model.as_deref(), Some("sonnet"));
        assert!(Config::parse("[agents.claude]\neffort = \"minimal\"").is_err());
        assert!(Config::parse("[agents.aider]\nmodel = \"x\"").is_err());
        assert!(Config::parse("[agents.pi]\nmodel = \"default\"").is_err());
        assert!(Config::parse("[agents.pi]\ntitle_model = \"\"").is_err());
    }

    #[test]
    fn the_default_agent_is_one_roster_knows() {
        assert_eq!(Config::parse("").unwrap().default_agent(), "claude");
        assert_eq!(
            Config::parse("default_agent = \"pi\"\n[agents.pi]")
                .unwrap()
                .default_agent(),
            "pi"
        );
        // pi, on a machine without it: Claude.
        let mut config = Config::parse("default_agent = \"pi\"").unwrap();
        config.agents.clear();
        if !crate::util::on_path("pi") {
            assert_eq!(config.default_agent(), "claude");
        }
        let config = Config::parse("default_agent = \"aider\"\n[agents.aider]").unwrap();
        assert_eq!(config.default_agent(), "aider");
        assert!(Config::parse("default_agent = \"nope\"").is_err());
    }

    #[test]
    fn rejects_unknown_settings_and_unsafe_keys() {
        assert!(Config::parse("colour = 1").is_err());
        assert!(Config::parse("[keys]\ndetach = \"C-q; kill-server\"").is_err());
        // Keys tmux knows, that leave the agents theirs.
        let detach = |key: &str| Config::parse(&format!("[keys]\ndetach = \"{key}\""));
        for good in ["C-q", "M-x", "C-M-Left", "F12", "S-F5", "C-BSpace"] {
            detach(good).unwrap_or_else(|e| panic!("{good}: {e:#}"));
        }
        for bad in [
            "Backspace",
            "C-Backspace",
            "q",
            "Enter",
            "S-q",
            "C-m",
            "C-H",
            "C-c",
            "C-",
            "",
        ] {
            assert!(detach(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_list_is_on_shelves_unless_grouped_by_project() {
        assert!(!Config::default().grouped());
        assert!(Config::parse("group_by = \"project\"").unwrap().grouped());
        assert!(!Config::parse("group_by = \"attention\"").unwrap().grouped());
        assert!(Config::parse("group_by = \"shelf\"").is_err());
        // Its key is gone: a config still setting it works, and says so.
        let old = Config::parse("[keys.view]\ngroup = \"g\"").unwrap();
        assert!(
            old.gone().iter().any(|g| g.contains("group_by")),
            "{:?}",
            old.gone()
        );
        assert!(Config::default().gone().is_empty());
    }

    #[test]
    fn the_key_giving_a_first_prompt_again_is_gone_but_taken() {
        let old = Config::parse("[keys.session]\nprompt = \"p\"").unwrap();
        let gone = old.gone();
        assert!(
            gone.iter()
                .any(|g| g.contains("keys.session.prompt") && g.contains("asks")),
            "{gone:?}"
        );
    }

    #[test]
    fn a_default_key_gives_way_to_one_the_config_sets() {
        // Set for another action before archive took "a", or adopt "A":
        // theirs, and said.
        for (text, by, yields) in [
            ("toggle_read = \"a\"", "toggle_read", "archive"),
            ("adopt = \"a\"", "adopt", "archive"),
            ("retitle = [\"t\", \"A\"]", "retitle", "adopt"),
        ] {
            let config = Config::parse(&format!("[keys.session]\n{text}")).unwrap();
            let gone = config.gone();
            assert!(
                gone.len() == 1
                    && gone[0].starts_with(&format!("keys.session.{by} takes "))
                    && gone[0].contains(&format!("{yields} goes without it")),
                "{gone:?}"
            );
            let mut keys = config.keys.session;
            let lists = keys.menu_keys();
            let list = &lists.iter().find(|(name, _)| *name == yields).unwrap().1;
            assert!(list.0.is_empty(), "{text}");
        }
        // Another default, or two the config sets: still an error.
        assert!(Config::parse("[keys.session]\nadopt = \"n\"").is_err());
        assert!(Config::parse("[keys.session]\ntoggle_read = \"a\"\nretitle = \"a\"").is_err());
        // The defaults as they are: nothing to say.
        assert!(
            Config::parse("[keys.session]\narchive = \"a\"")
                .unwrap()
                .gone()
                .is_empty()
        );
    }

    #[test]
    fn notes_show_two_lines_unless_set_from_one_to_ten() {
        assert_eq!(Config::default().notes.lines(), 2);
        let notes = |text: &str| Config::parse(&format!("[notes]\n{text}"));
        assert_eq!(notes("lines = 5").unwrap().notes.lines(), 5);
        assert!(notes("lines = 0").is_err());
        assert!(notes("lines = 11").is_err());
    }

    #[test]
    fn notes_escape_is_keys_typed_in_a_row() {
        let config = Config::parse("[notes]\nescape = \"jk\"\nescape_ms = 200").unwrap();
        let (keys, within) = config.notes.escape().unwrap();
        assert_eq!((keys, within.as_millis()), (vec!['j', 'k'], 200));
        assert_eq!(Config::default().notes.escape(), None);
        let notes = |text: &str| Config::parse(&format!("[notes]\n{text}"));
        assert_eq!(
            notes("escape = \"jj\"")
                .unwrap()
                .notes
                .escape()
                .unwrap()
                .1
                .as_millis(),
            300
        );
        for bad in [
            "escape = \"j\"",
            "escape = \"j k\"",
            "escape = \"\"",
            "escape_ms = 0",
        ] {
            assert!(notes(bad).is_err(), "{bad}");
        }
    }
}
