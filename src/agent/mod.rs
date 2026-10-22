//! Agents: what roster needs to know about each beyond running its command.
//!
//! An agent roster knows nothing about (any CLI under `[agents.<name>]` in the
//! config) is generic: its state comes from its screen alone, and each start is
//! a new conversation. A built-in agent has an `Adapter` that tells more: how
//! to start and resume it, what its reports (hooks calling `roster report`)
//! say, which models and efforts it takes, and where its transcript is.
//! Adding one is a module here, implementing what it supports, a line in
//! `adapter` and its name in `BUILT_IN`.
//!
//! An agent that reports does so with `roster report <kind>`, a JSON payload
//! on its stdin (`ROSTER_SESSION`, `ROSTER_DB` and `ROSTER_EXE` are in its
//! environment). The kinds:
//!
//! - `session`: it (re)started or cleared its conversation, and waits for a
//!   prompt. Its payload names the transcript: until then, a first prompt
//!   isn't typed in (the agent may still be asking something else, such as
//!   whether to trust the directory).
//! - `prompt`: you gave it a task; it's working.
//! - `working`: it's still at it (after a tool, or a question answered).
//! - `needs-input`: it's waiting for you in the middle of a turn.
//! - `idle`: the turn is over.
//! - anything else (pi's extension says `info`): only its payload counts.
//!
//! An agent that reports needs to report the start of a turn as well as its
//! end: its screen only corrects an end that went unreported.

pub mod claude;
pub mod pi;

use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer};

use crate::config::Config;
use crate::model::{QUIET_MS, Session};
use crate::stats::Transcript;

pub use claude::CLAUDE;
pub use pi::PI;

/// Agents with an adapter. Claude is always offered; the others once they're
/// installed (their command is found) or configured.
pub const BUILT_IN: &[&str] = &[CLAUDE, PI];

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

    /// How lists mark its sessions; None for the generic mark, which says only
    /// that it's an agent, so lists name it as well.
    fn mark(&self) -> Option<Mark> {
        None
    }

    /// The efforts a session can ask for; None if it takes neither a model
    /// nor an effort.
    fn choices(&self) -> Option<Choices> {
        None
    }

    /// The models to offer for a session, for forms (any other name can be
    /// typed). An agent that can list its own does, through `ask`, which runs
    /// a command of its in the environment its sessions get and returns what
    /// it printed (None if it failed); others know theirs.
    fn models(&self, _config: &Config, _ask: &Ask<'_>) -> Vec<Model> {
        Vec::new()
    }

    /// The command line that starts `s`, given its command and the config's
    /// arguments.
    fn launch(&self, _s: &Session, argv: Vec<String>, _cx: &Context) -> Result<Launch> {
        Ok(Launch {
            argv,
            conversation_id: None,
        })
    }

    /// What a report's payload says, in roster's terms.
    fn report(&self, _payload: &serde_json::Value) -> Report {
        Report::default()
    }

    /// The task the agent names in its terminal title, if it names one.
    fn title(&self, _pane_title: &str) -> Option<String> {
        None
    }

    /// A one-off run of the agent for a small job such as a title: the prompt
    /// goes last, the answer comes on stdout, and nothing of it is kept
    /// (no tools, no session). `[agents.<name>] title_model` picks its model,
    /// else a cheap one the adapter knows, else the agent's default. None if
    /// the agent has no such mode.
    fn oneoff(&self, _config: &Config) -> Option<Vec<String>> {
        None
    }

    /// A reader for the transcript at `path` (as its reports named it). Reads
    /// nothing yet: whether an agent has one at all is asked this way.
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
        PI => &pi::Pi,
        _ => &Generic,
    }
}

/// An agent's mark in lists: a glyph, in its colour if it has one.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub glyph: &'static str,
    pub rgb: Option<(u8, u8, u8)>,
}

/// The efforts an agent takes.
#[derive(Clone, Copy, Debug)]
pub struct Choices {
    pub efforts: &'static [&'static str],
}

/// A model to offer for a session.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// What the agent's model option takes.
    pub id: String,
    /// Shown next to it. None for an alias, which follows the latest version:
    /// what it last turned out to be shows instead ("latest" until known).
    pub note: Option<String>,
}

/// Runs an agent's command (its argv) and returns what it printed.
pub type Ask<'a> = dyn Fn(&[String]) -> Option<String> + 'a;

/// What an agent reported besides its state: `Adapter::report` reads it from
/// the payload, whatever its agent calls things. An agent whose reports roster
/// writes itself (Pi's, through roster's extension) sends it as JSON with
/// these names.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Report {
    /// The conversation it's on now (it changes when the agent clears it).
    pub conversation_id: Option<String>,
    pub transcript_path: Option<String>,
    /// The conversation was cleared: a new task.
    pub cleared: bool,
    /// The model and effort it's running.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// What it says it's working on; Some(None): it no longer says.
    #[serde(deserialize_with = "present")]
    pub title: Option<Option<String>>,
    /// The context window of the model it's running, in tokens.
    pub context_window: Option<u64>,
}

/// A field that's there, even as null (missing ones get the default, None).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// What a report from agent `name` says; nothing if its payload isn't JSON.
pub fn read_report(name: &str, payload: Option<&str>) -> Report {
    payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .map(|v| adapter(name).report(&v))
        .unwrap_or_default()
}

/// What an adapter may use to start its agent.
pub struct Context<'a> {
    /// roster's own executable, which the agent's hooks call (`roster report`).
    pub roster: &'a Path,
    /// roster's state directory, for files an adapter gives its agent.
    pub state_dir: &'a Path,
}

pub struct Launch {
    pub argv: Vec<String>,
    /// The conversation the agent will use, to store with the session.
    pub conversation_id: Option<String>,
}

/// `name`'s adapter, if it's an agent that can be started (see `available`).
pub fn known(name: &str, config: &Config) -> Result<&'static dyn Adapter> {
    if offered(name, config) {
        Ok(adapter(name))
    } else {
        bail!(
            "unknown agent {name:?} (available: {})",
            available(config).join(", ")
        )
    }
}

/// Agents that can be started: the default one first, then the built-in ones
/// that are installed or configured (Claude always), then the others from the
/// config.
pub fn available(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = BUILT_IN
        .iter()
        .filter(|n| offered(n, config))
        .map(|n| n.to_string())
        .collect();
    names.extend(
        config
            .agents
            .keys()
            .filter(|n| !BUILT_IN.contains(&n.as_str()))
            .cloned(),
    );
    if let Some(i) = names.iter().position(|n| n == config.default_agent()) {
        let default = names.remove(i);
        names.insert(0, default);
    }
    names
}

fn offered(name: &str, config: &Config) -> bool {
    name == CLAUDE
        || config.agents.contains_key(name)
        || (BUILT_IN.contains(&name) && crate::util::on_path(name))
}

/// The key under which what a model asked of `agent` (or "default") turned
/// out to be is learned: Claude's as it is, as before there were other
/// agents; another's after its name ("pi:default"). `Db::learn_model_names`
/// has the same rule in SQL.
pub fn learned_key(agent: &str, asked: &str) -> String {
    if agent == CLAUDE {
        asked.to_string()
    } else {
        format!("{agent}:{asked}")
    }
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
pub fn launch(s: &Session, config: &Config, cx: &Context) -> Result<Launch> {
    let adapter = known(&s.agent, config)?;
    let agent = config.agent(&s.agent);
    let mut argv = vec![agent.command.clone().unwrap_or_else(|| s.agent.clone())];
    argv.extend(agent.args.iter().cloned());
    adapter.launch(s, argv, cx)
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

    /// A launch context with roster at `roster` and nowhere to keep files.
    pub(super) fn cx(roster: &str) -> Context<'_> {
        Context {
            roster: Path::new(roster),
            state_dir: Path::new("/nonexistent"),
        }
    }

    #[test]
    fn configured_agents_are_generic() {
        let mut s = session("a");
        s.agent = "codex".into();
        assert!(launch(&s, &Config::default(), &cx("/bin/roster")).is_err());

        let agent = AgentConfig {
            args: vec!["--quiet".into()],
            idle_after: Some(2),
            ..AgentConfig::default()
        };
        let config = config_with("codex", agent);
        let l = launch(&s, &config, &cx("/bin/roster")).unwrap();
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
        // Built-in agents first: Claude, then Pi if it's installed here.
        let expected: &[&str] = if crate::util::on_path("pi") {
            &["claude", "pi", "codex"]
        } else {
            &["claude", "codex"]
        };
        assert_eq!(available(&config), expected);
        assert_eq!(idle_after_ms("codex", &config), 2000);
        assert_eq!(idle_after_ms("claude", &config), QUIET_MS);
    }

    #[test]
    fn built_in_agents_are_offered_once_installed_or_configured() {
        let mut config = Config::default();
        config.agents.insert("pi".into(), AgentConfig::default());
        assert!(known("pi", &config).unwrap().reports());
        assert_eq!(available(&config), ["claude", "pi"]);
        assert!(known("claude", &Config::default()).is_ok());
    }

    #[test]
    fn the_default_agent_comes_first() {
        let mut config = config_with("aider", AgentConfig::default());
        config.default_agent = Some("aider".into());
        assert_eq!(
            available(&config).first().map(String::as_str),
            Some("aider")
        );
        assert!(available(&config).contains(&"claude".to_string()));
    }

    #[test]
    fn models_are_learned_per_agent() {
        assert_eq!(learned_key("claude", "opus"), "opus");
        assert_eq!(learned_key("pi", "default"), "pi:default");
    }

    #[test]
    fn reports_in_rosters_own_terms() {
        let report: Report = serde_json::from_str(
            r#"{"conversation_id":"c-1","cleared":true,"title":null,"context_window":200000,"extra":1}"#,
        )
        .unwrap();
        assert_eq!(report.conversation_id.as_deref(), Some("c-1"));
        assert!(report.cleared);
        // A title that's gone is told apart from one not reported.
        assert_eq!(report.title, Some(None));
        assert_eq!(report.context_window, Some(200_000));
        let report: Report = serde_json::from_str(r#"{"title":"Fix it"}"#).unwrap();
        assert_eq!(report.title, Some(Some("Fix it".into())));
        assert_eq!(
            serde_json::from_str::<Report>("{}").unwrap(),
            Report::default()
        );
    }
}
