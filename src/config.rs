//! `config.toml`: hand-edited, read-only for roster. Every setting
//! is optional.
//!
//! ```toml
//! shell = "/usr/bin/zsh"          # sources the env files; default: $SHELL if POSIX-like
//!
//! [keys]                          # TUI keys: one, a list, or "" to disable
//! detach = "C-q"                  # back to roster while attached (a tmux key)
//! down = ["j", "Down"]
//! delete = ""
//!
//! [agents.claude]
//! command = "claude"              # default: found on PATH
//! args = ["--model", "haiku"]     # added to every launch
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
    pub attach: KeyList,
    pub new: KeyList,
    pub add_project: KeyList,
    pub start: KeyList,
    pub stop: KeyList,
    pub restart: KeyList,
    pub delete: KeyList,
    pub toggle_read: KeyList,
    pub deleted: KeyList,
    pub quit: KeyList,
}

impl Default for Keys {
    fn default() -> Self {
        let keys = |names: &[&str]| KeyList(names.iter().map(|n| n.to_string()).collect());
        Keys {
            detach: "C-q".into(),
            up: keys(&["k", "Up"]),
            down: keys(&["j", "Down"]),
            attach: keys(&["Enter"]),
            new: keys(&["n"]),
            add_project: keys(&["P"]),
            start: keys(&["s"]),
            stop: keys(&["d"]),
            restart: keys(&["R"]),
            delete: keys(&["D"]),
            toggle_read: keys(&["u"]),
            deleted: keys(&["X"]),
            quit: keys(&["q", "C-c"]),
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
        let config =
            Config::parse("[keys]\nstop = \"x\"\ndown = [\"j\", \"PageDown\"]\ndelete = \"\"")
                .unwrap();
        assert_eq!(config.keys.stop, KeyList(vec!["x".into()]));
        assert_eq!(config.keys.down.0, ["j", "PageDown"]);
        assert!(config.keys.delete.0.is_empty());
        assert_eq!(config.keys.quit.0, ["q", "C-c"]);
        assert!(Config::parse("[keys]\nstop = \"Nope\"").is_err());
        assert!(Config::parse("[keys]\nstop = \"q\"").is_err());
    }

    #[test]
    fn rejects_unknown_settings_and_unsafe_keys() {
        assert!(Config::parse("colour = 1").is_err());
        assert!(Config::parse("[keys]\ndetach = \"C-q; kill-server\"").is_err());
    }
}
