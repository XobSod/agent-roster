//! Agent adapters: the command line that starts an agent with roster's hooks
//! injected for this run only. The MVP has Claude Code.

use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;

use crate::config::AgentConfig;
use crate::model::Session;
use crate::util::sh_quote;

pub const AGENTS: &[&str] = &["claude"];

pub fn check(agent: &str) -> Result<()> {
    if !AGENTS.contains(&agent) {
        bail!("unknown agent {agent:?} (available: {})", AGENTS.join(", "));
    }
    Ok(())
}

pub struct Launch {
    pub argv: Vec<String>,
    /// The conversation the agent will use, to store with the session.
    pub conversation_id: String,
}

/// How to start `s` now: a new conversation, or resuming its existing one.
pub fn launch(s: &Session, config: &AgentConfig, roster_exe: &Path) -> Result<Launch> {
    check(&s.agent)?;
    Ok(claude(s, config, roster_exe))
}

fn claude(s: &Session, config: &AgentConfig, roster_exe: &Path) -> Launch {
    let mut argv = vec![config.command.clone().unwrap_or_else(|| "claude".into())];
    argv.extend(config.args.iter().cloned());
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
        conversation_id,
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
    use crate::model::tests::session;

    fn arg_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|a| a == flag)
            .map(|i| argv[i + 1].as_str())
    }

    #[test]
    fn first_start_assigns_a_conversation_id() {
        let s = session("a");
        let l = launch(&s, &AgentConfig::default(), Path::new("/bin/roster")).unwrap();
        assert_eq!(l.argv[0], "claude");
        assert_eq!(
            arg_after(&l.argv, "--session-id"),
            Some(l.conversation_id.as_str())
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
        let l = launch(&s, &AgentConfig::default(), Path::new("/bin/roster")).unwrap();
        assert_eq!(arg_after(&l.argv, "--session-id"), Some("c-1"));

        std::fs::write(&transcript, "{}").unwrap();
        let l = launch(&s, &AgentConfig::default(), Path::new("/bin/roster")).unwrap();
        assert_eq!(arg_after(&l.argv, "--resume"), Some("c-1"));
        assert_eq!(l.conversation_id, "c-1");
    }

    #[test]
    fn uses_configured_command_and_args() {
        let config = AgentConfig {
            command: Some("/opt/claude".into()),
            args: vec!["--model".into(), "haiku".into()],
        };
        let l = launch(&session("a"), &config, Path::new("/bin/roster")).unwrap();
        assert_eq!(l.argv[..3], ["/opt/claude", "--model", "haiku"]);
    }

    #[test]
    fn hooks_report_through_the_quoted_roster_path() {
        let l = launch(
            &session("a"),
            &AgentConfig::default(),
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
    fn rejects_unknown_agents() {
        let mut s = session("a");
        s.agent = "codex".into();
        assert!(launch(&s, &AgentConfig::default(), Path::new("/bin/roster")).is_err());
    }
}
