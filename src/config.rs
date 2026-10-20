//! `config.toml`: hand-edited, read-only for roster. Every setting
//! is optional.
//!
//! ```toml
//! shell = "/usr/bin/zsh"          # sources the env files; default: $SHELL if POSIX-like
//! chats_dir = "~/chats"           # where chats run; default: roster's state directory
//!
//! [order]                         # within each shelf: "oldest" or "newest" first
//! needs_input = "oldest"          # the defaults: first come, first served,
//! idle = "oldest"
//! stopped = "oldest"
//! working = "oldest"              # (the one working longest on top)
//! archived = "newest"             # and the most recently archived on top
//!
//! [keys]                          # TUI keys: one, a list, or "" to disable
//! detach = "C-q"                  # back to roster while attached (a tmux key)
//! down = ["j", "Down"]
//! [keys.session]                  # a menu: `menu` opens it, the rest act inside
//! menu = "s"
//! delete = ""                     # no key for deleting archived sessions for good
//!
//! [agents.claude]
//! command = "claude"              # default: the agent's name, found on PATH
//! args = ["--model", "haiku"]     # added to every launch
//!
//! [agents.aider]                  # any other agent: state from its screen
//! idle_after = 4                  # seconds without screen changes = idle
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
    /// Archive a session, or restore an archived one.
    pub archive: KeyList,
    /// Delete an archived session for good.
    pub delete: KeyList,
    pub toggle_read: KeyList,
    pub retitle: KeyList,
    pub adopt: KeyList,
    /// A new chat: a session in the built-in chats project.
    pub chat: KeyList,
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
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewKeys {
    pub menu: KeyList,
    pub group: KeyList,
    /// The projects view (also in the project menu).
    pub projects: KeyList,
    pub server: KeyList,
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
            archive: keys(&["d"]),
            delete: keys(&["D"]),
            toggle_read: keys(&["u"]),
            retitle: keys(&["t"]),
            adopt: keys(&["a"]),
            chat: keys(&["c"]),
        }
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
        }
    }
}

impl Default for ViewKeys {
    fn default() -> Self {
        ViewKeys {
            menu: keys(&["v"]),
            group: keys(&["g"]),
            projects: keys(&["p"]),
            server: keys(&["t"]),
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
}

impl Config {
    /// Reads `path`; a missing file means all defaults.
    pub fn load(path: &Path) -> Result<Config> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("can't read {}", path.display())),
        };
        let config =
            Config::parse(&text).with_context(|| format!("invalid config {}", path.display()))?;
        Ok(config)
    }

    fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        let key = &config.keys.detach;
        // It goes into the generated tmux config, so only allow tmux key-name characters.
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!("keys.detach must be a tmux key name such as \"C-q\", not {key:?}");
        }
        crate::keys::Keymap::new(&config.keys)?;
        for (name, agent) in &config.agents {
            if agent.idle_after == Some(0) {
                bail!("agents.{name}.idle_after must be at least 1 (seconds)");
            }
        }
        Ok(config)
    }

    pub fn agent(&self, name: &str) -> AgentConfig {
        self.agents.get(name).cloned().unwrap_or_default()
    }
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
    fn rejects_unknown_settings_and_unsafe_keys() {
        assert!(Config::parse("colour = 1").is_err());
        assert!(Config::parse("[keys]\ndetach = \"C-q; kill-server\"").is_err());
    }
}
