//! Agents: what roster needs to know about each beyond running its command.
//!
//! An agent roster knows nothing about (any CLI under `[agents.<name>]` in the
//! config) is generic: its state comes from its screen alone, and each start is
//! a new conversation. A built-in agent has an `Adapter` that tells more: how
//! to start and resume it, what its reports (hooks calling `roster report`)
//! say, which models and efforts it takes, and where its transcript is.
//! Adding one is a module here, implementing what it supports, and a line in
//! `adapter`.

pub mod claude;

use std::path::Path;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::model::{QUIET_MS, Session};
use crate::stats::Transcript;

pub use claude::CLAUDE;

/// The agent new sessions get unless the project or the user picks another.
pub const DEFAULT: &str = CLAUDE;

/// Agents with an adapter, which can be started without configuring them.
pub const BUILT_IN: &[&str] = &[CLAUDE];

/// What a built-in agent can do beyond running. Every method has a default for
/// an agent that doesn't, so an adapter implements only what its agent supports.
pub trait Adapter: Sync {
    /// Whether it reports its state through hooks calling `roster report`;
    /// if not, its screen is all roster has to go on.
    fn reports(&self) -> bool {
        false
    }

    /// Whether starting it again resumes its conversation.
    fn resumes(&self) -> bool {
        false
    }

    /// Whether its conversations outlive roster's sessions, to be adopted again.
    fn adopts(&self) -> bool {
        false
    }

    /// The models and efforts a session can ask for; None if it takes neither.
    fn choices(&self) -> Option<Choices> {
        None
    }

    /// The command line that starts `s`, given its command and the config's
    /// arguments; the agent reports through `roster`.
    fn launch(&self, _s: &Session, argv: Vec<String>, _roster: &Path) -> Launch {
        Launch {
            argv,
            conversation_id: None,
        }
    }

    /// What a report's payload says, in roster's terms.
    fn report(&self, _payload: &serde_json::Value) -> Report {
        Report::default()
    }

    /// The task the agent names in its terminal title, if it names one.
    fn title(&self, _pane_title: &str) -> Option<String> {
        None
    }

    /// A reader for the transcript at `path` (as its reports named it).
    fn transcript(&self, _path: &Path) -> Option<Box<dyn Transcript>> {
        None
    }
}

/// Any agent without an adapter of its own.
struct Generic;

impl Adapter for Generic {}

/// The adapter for agent `name`: a built-in agent's, else the generic one (also
/// for agents since removed from the config, which are still watched).
pub fn adapter(name: &str) -> &'static dyn Adapter {
    match name {
        CLAUDE => &claude::Claude,
        _ => &Generic,
    }
}

/// The models and efforts an agent takes.
#[derive(Clone, Copy, Debug)]
pub struct Choices {
    /// Names that follow the latest version ("opus").
    pub aliases: &'static [&'static str],
    /// Full model ids, for pinning a version.
    pub pinned: &'static [&'static str],
    pub efforts: &'static [&'static str],
}

impl Choices {
    /// The aliases, then the pinned ids.
    pub fn models(&self) -> Vec<&'static str> {
        self.aliases.iter().chain(self.pinned).copied().collect()
    }
}

/// What an agent reported besides its state: `Adapter::report` reads it from
/// the payload, whatever its agent calls things.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// The conversation it's on now (it changes when the agent clears it).
    pub conversation_id: Option<String>,
    pub transcript_path: Option<String>,
    /// The conversation was cleared: a new task.
    pub cleared: bool,
    /// The model and effort it's running.
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// What a report from agent `name` says; nothing if its payload isn't JSON.
pub fn read_report(name: &str, payload: Option<&str>) -> Report {
    payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .map(|v| adapter(name).report(&v))
        .unwrap_or_default()
}

pub struct Launch {
    pub argv: Vec<String>,
    /// The conversation the agent will use, to store with the session.
    pub conversation_id: Option<String>,
}

/// `name`'s adapter, if it's an agent that can be started: built in, or
/// configured under `[agents.<name>]`.
pub fn known(name: &str, config: &Config) -> Result<&'static dyn Adapter> {
    if BUILT_IN.contains(&name) || config.agents.contains_key(name) {
        Ok(adapter(name))
    } else {
        bail!(
            "unknown agent {name:?} (available: {})",
            available(config).join(", ")
        )
    }
}

/// Agents that can be started: the built-in ones, then the others from the config.
pub fn available(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = BUILT_IN.iter().map(|n| n.to_string()).collect();
    names.extend(
        config
            .agents
            .keys()
            .filter(|n| !BUILT_IN.contains(&n.as_str()))
            .cloned(),
    );
    names
}

/// Checks an effort against the ones `agent` takes. An agent that takes none
/// ignores it.
pub fn check_effort(agent: &str, effort: &str) -> Result<()> {
    let Some(choices) = adapter(agent).choices() else {
        return Ok(());
    };
    if !choices.efforts.contains(&effort) {
        bail!(
            "unknown effort {effort:?} (one of: {})",
            choices.efforts.join(", ")
        );
    }
    Ok(())
}

/// How long an agent without hooks must show a still screen to count as idle.
pub fn idle_after_ms(name: &str, config: &Config) -> i64 {
    config
        .agent(name)
        .idle_after
        .map_or(QUIET_MS, |secs| secs as i64 * 1000)
}

/// Whether `name`'s sessions may be sized to the TUI's preview (when drawing
/// full screen): its own setting, else the global one, else yes.
pub fn fits_preview(name: &str, config: &Config) -> bool {
    let setting = config.agent(name).fit_preview.or(config.fit_preview);
    setting != Some(crate::config::FitPreview::Never)
}

/// How to start `s` now: its command and the config's arguments, completed by
/// its adapter (for Claude, a new conversation or its existing one resumed).
pub fn launch(s: &Session, config: &Config, roster_exe: &Path) -> Result<Launch> {
    let adapter = known(&s.agent, config)?;
    let agent = config.agent(&s.agent);
    let mut argv = vec![agent.command.clone().unwrap_or_else(|| s.agent.clone())];
    argv.extend(agent.args.iter().cloned());
    Ok(adapter.launch(s, argv, roster_exe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::model::tests::session;

    pub(super) fn config_with(name: &str, agent: AgentConfig) -> Config {
        let mut config = Config::default();
        config.agents.insert(name.into(), agent);
        config
    }

    #[test]
    fn configured_agents_are_generic() {
        let mut s = session("a");
        s.agent = "codex".into();
        assert!(launch(&s, &Config::default(), Path::new("/bin/roster")).is_err());

        let agent = AgentConfig {
            args: vec!["--quiet".into()],
            idle_after: Some(2),
            ..AgentConfig::default()
        };
        let config = config_with("codex", agent);
        let l = launch(&s, &config, Path::new("/bin/roster")).unwrap();
        // The command defaults to the agent's name; no hooks, no conversation.
        assert_eq!(l.argv, ["codex", "--quiet"]);
        assert_eq!(l.conversation_id, None);
        let codex = known("codex", &config).unwrap();
        assert!(!codex.reports() && !codex.resumes() && codex.choices().is_none());
        assert!(adapter(CLAUDE).resumes());
        // An effort means nothing to it.
        assert!(check_effort("codex", "extreme").is_ok());
        // Fitting the preview: the agent's setting, else the global one, else yes.
        let mut config = config;
        assert!(fits_preview("codex", &config));
        config.fit_preview = Some(crate::config::FitPreview::Never);
        assert!(!fits_preview("codex", &config) && !fits_preview("claude", &config));
        config.agents.get_mut("codex").unwrap().fit_preview = Some(crate::config::FitPreview::Auto);
        assert!(fits_preview("codex", &config) && !fits_preview("claude", &config));
        assert_eq!(available(&config), ["claude", "codex"]);
        assert_eq!(idle_after_ms("codex", &config), 2000);
        assert_eq!(idle_after_ms("claude", &config), QUIET_MS);
    }
}
