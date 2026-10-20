//! Agent adapters: Claude Code, with hooks injected for this
//! run only, and a generic adapter for any other CLI agent, whose state comes from
//! its screen alone.

use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use crate::config::Config;
use crate::model::{QUIET_MS, Session};
use crate::util::sh_quote;

pub const CLAUDE: &str = "claude";

/// Model aliases Claude accepts (`claude --help`); any full model name works too.
pub const CLAUDE_MODELS: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// Claude's `--effort` levels.
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

pub fn check_effort(effort: &str) -> Result<()> {
    if !EFFORTS.contains(&effort) {
        bail!("unknown effort {effort:?} (one of: {})", EFFORTS.join(", "));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Hooks report its state, and its conversations resume.
    Claude,
    /// Any other command: state from screen changes, no resume.
    Generic,
}

impl Kind {
    pub fn can_resume(self) -> bool {
        self == Kind::Claude
    }
}

/// What kind of agent `name` is: Claude, or one configured under `[agents.<name>]`.
pub fn kind(name: &str, config: &Config) -> Result<Kind> {
    if name == CLAUDE {
        Ok(Kind::Claude)
    } else if config.agents.contains_key(name) {
        Ok(Kind::Generic)
    } else {
        bail!(
            "unknown agent {name:?} (available: {})",
            available(config).join(", ")
        )
    }
}

/// Agents that can be started: Claude, then the others from the config.
pub fn available(config: &Config) -> Vec<String> {
    let mut names = vec![CLAUDE.to_string()];
    names.extend(config.agents.keys().filter(|n| *n != CLAUDE).cloned());
    names
}

/// How long an agent without hooks must show a still screen to count as idle.
pub fn idle_after_ms(name: &str, config: &Config) -> i64 {
    config
        .agent(name)
        .idle_after
        .map_or(QUIET_MS, |secs| secs as i64 * 1000)
}

pub struct Launch {
    pub argv: Vec<String>,
    /// The conversation the agent will use, to store with the session.
    pub conversation_id: Option<String>,
}

/// How to start `s` now: for Claude a new conversation or its existing one resumed.
pub fn launch(s: &Session, config: &Config, roster_exe: &Path) -> Result<Launch> {
    let agent = config.agent(&s.agent);
    let mut argv = vec![agent.command.clone().unwrap_or_else(|| s.agent.clone())];
    argv.extend(agent.args.iter().cloned());
    Ok(match kind(&s.agent, config)? {
        Kind::Claude => claude(s, argv, roster_exe),
        Kind::Generic => Launch {
            argv,
            conversation_id: None,
        },
    })
}

fn claude(s: &Session, mut argv: Vec<String>, roster_exe: &Path) -> Launch {
    // After the config's args, so the session's choice wins.
    if let Some(model) = &s.model {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &s.effort {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    // --session-id fails once the conversation has messages, i.e. once its
    // transcript exists; from then on it has to be --resume.
    let has_transcript = s
        .transcript_path
        .as_deref()
        .is_some_and(|p| Path::new(p).exists());
    let conversation_id = match (&s.conversation_id, has_transcript) {
        (Some(id), true) => {
            argv.extend(["--resume".into(), id.clone()]);
            id.clone()
        }
        (existing, _) => {
            let id = existing
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            argv.extend(["--session-id".into(), id.clone()]);
            id
        }
    };
    argv.extend(["--settings".into(), claude_settings(roster_exe)]);
    Launch {
        argv,
        conversation_id: Some(conversation_id),
    }
}

/// Extra settings for this run only: hooks that report to roster. They run next to
/// the user's and the project's own hooks.
fn claude_settings(roster_exe: &Path) -> String {
    let report = |kind: &str| json!([{"hooks": [{"type": "command", "command": format!("{} report {kind}", sh_quote(&roster_exe.to_string_lossy()))}]}]);
    json!({"hooks": {
        "SessionStart": report("session"),
        "UserPromptSubmit": report("working"),
        "PostToolUse": report("working"),
        "PermissionRequest": report("needs-input"),
        "Stop": report("idle"),
    }})
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::model::tests::session;

    fn arg_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|a| a == flag)
            .map(|i| argv[i + 1].as_str())
    }

    fn config_with(name: &str, agent: AgentConfig) -> Config {
        let mut config = Config::default();
        config.agents.insert(name.into(), agent);
        config
    }

    #[test]
    fn first_start_assigns_a_conversation_id() {
        let s = session("a");
        let l = launch(&s, &Config::default(), Path::new("/bin/roster")).unwrap();
        assert_eq!(l.argv[0], "claude");
        assert_eq!(
            arg_after(&l.argv, "--session-id"),
            l.conversation_id.as_deref()
        );
        assert!(!l.argv.contains(&"--resume".to_string()));
    }

    #[test]
    fn resumes_once_the_transcript_exists() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("c-1.jsonl");
        let mut s = session("a");
        s.conversation_id = Some("c-1".into());
        s.transcript_path = Some(transcript.to_string_lossy().into());

        // No transcript yet (e.g. right after /clear): reuse the ID as a new conversation.
        let l = launch(&s, &Config::default(), Path::new("/bin/roster")).unwrap();
        assert_eq!(arg_after(&l.argv, "--session-id"), Some("c-1"));

        std::fs::write(&transcript, "{}").unwrap();
        let l = launch(&s, &Config::default(), Path::new("/bin/roster")).unwrap();
        assert_eq!(arg_after(&l.argv, "--resume"), Some("c-1"));
        assert_eq!(l.conversation_id.as_deref(), Some("c-1"));
    }

    #[test]
    fn uses_configured_command_and_args() {
        let agent = AgentConfig {
            command: Some("/opt/claude".into()),
            args: vec!["--model".into(), "haiku".into()],
            idle_after: None,
        };
        let l = launch(
            &session("a"),
            &config_with("claude", agent),
            Path::new("/bin/roster"),
        )
        .unwrap();
        assert_eq!(l.argv[..3], ["/opt/claude", "--model", "haiku"]);
    }

    #[test]
    fn hooks_report_through_the_quoted_roster_path() {
        let l = launch(
            &session("a"),
            &Config::default(),
            Path::new("/my tools/roster"),
        )
        .unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(arg_after(&l.argv, "--settings").unwrap()).unwrap();
        let stop = &settings["hooks"]["Stop"][0]["hooks"][0]["command"];
        assert_eq!(stop, "'/my tools/roster' report idle");
        assert_eq!(
            settings["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "'/my tools/roster' report needs-input"
        );
    }

    #[test]
    fn session_model_and_effort_follow_the_config_args() {
        let agent = AgentConfig {
            args: vec!["--model".into(), "sonnet".into()],
            ..AgentConfig::default()
        };
        let mut s = session("a");
        s.model = Some("opus".into());
        s.effort = Some("high".into());
        let l = launch(&s, &config_with("claude", agent), Path::new("/bin/roster")).unwrap();
        assert_eq!(
            l.argv[1..7],
            ["--model", "sonnet", "--model", "opus", "--effort", "high"]
        );
        assert!(check_effort("xhigh").is_ok() && check_effort("extreme").is_err());
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
        assert_eq!(kind("codex", &config).unwrap(), Kind::Generic);
        assert!(!Kind::Generic.can_resume() && Kind::Claude.can_resume());
        assert_eq!(available(&config), ["claude", "codex"]);
        assert_eq!(idle_after_ms("codex", &config), 2000);
        assert_eq!(idle_after_ms("claude", &config), QUIET_MS);
    }
}
