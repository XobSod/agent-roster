//! `config.toml`: hand-edited, read-only for roster. Every setting
//! is optional.
//!
//! ```toml
//! shell = "/usr/bin/zsh"          # sources the env files; default: $SHELL if POSIX-like
//!
//! [keys]                          # TUI keys: one, a list, or "" to disable
//! detach = "C-q"                  # back to roster while attached (a tmux key)
//! down = ["j", "Down"]
//! [keys.session]                  # a menu: `menu` opens it, the rest act inside
//! menu = "s"
//! delete = ""
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
    pub keys: Keys,
    pub agents: BTreeMap<String, AgentConfig>,
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
    /// Attach to the selected session, open or fold a heading, restore when deleted.
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
    pub start: KeyList,
    pub stop: KeyList,
    pub restart: KeyList,
    pub delete: KeyList,
    pub toggle_read: KeyList,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectKeys {
    pub menu: KeyList,
    pub new: KeyList,
    pub remove: KeyList,
    pub filter: KeyList,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewKeys {
    pub menu: KeyList,
    pub group: KeyList,
    pub deleted: KeyList,
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
            start: keys(&["s"]),
            stop: keys(&["x"]),
            restart: keys(&["r"]),
            delete: keys(&["d"]),
            toggle_read: keys(&["u"]),
        }
    }
}

impl Default for ProjectKeys {
    fn default() -> Self {
        ProjectKeys {
            menu: keys(&["p"]),
            new: keys(&["n"]),
            remove: keys(&["d"]),
            filter: keys(&["f"]),
        }
    }
}

impl Default for ViewKeys {
    fn default() -> Self {
        ViewKeys {
            menu: keys(&["v"]),
            group: keys(&["p"]),
            deleted: keys(&["d"]),
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
