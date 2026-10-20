//! Where roster keeps its files (XDG directories) and which tmux
//! socket it uses.

use std::path::PathBuf;

use anyhow::{Context, Result};

const APP: &str = "agent-roster";

/// Per-project env file, sourced from the project's origin before every start.
pub const PROJECT_ENV: &str = ".agent-roster.env";

pub struct Paths {
    /// `$XDG_CONFIG_HOME/agent-roster`: config.toml, env, tmux.conf (all hand-edited).
    pub config_dir: PathBuf,
    /// `$XDG_STATE_HOME/agent-roster`: the database and the generated tmux config.
    pub state_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Paths> {
        let config =
            dirs::config_dir().context("can't find the config directory (set XDG_CONFIG_HOME)")?;
        let state =
            dirs::state_dir().context("can't find the state directory (set XDG_STATE_HOME)")?;
        Ok(Paths {
            config_dir: config.join(APP),
            state_dir: state.join(APP),
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Global env file, sourced before every start.
    pub fn global_env(&self) -> PathBuf {
        self.config_dir.join("env")
    }

    /// The user's optional additions to roster's tmux config.
    pub fn user_tmux_conf(&self) -> PathBuf {
        self.config_dir.join("tmux.conf")
    }

    pub fn db(&self) -> PathBuf {
        self.state_dir.join("roster.db")
    }

    /// Where chats run unless `chats_dir` says otherwise.
    pub fn chats_dir(&self) -> PathBuf {
        self.state_dir.join("chats")
    }

    /// The tmux config roster generates for its private server.
    pub fn tmux_conf(&self) -> PathBuf {
        self.state_dir.join("tmux.conf")
    }
}

/// Socket name of roster's private tmux server. `ROSTER_TMUX_SOCKET` overrides it,
/// for tests and for trying a development build next to a real one.
pub fn tmux_socket() -> String {
    std::env::var("ROSTER_TMUX_SOCKET").unwrap_or_else(|_| APP.into())
}
