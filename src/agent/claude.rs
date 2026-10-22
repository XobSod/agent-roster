//! Claude Code: hooks injected for each run report its state, its
//! conversations resume, and its transcript gives the statistics.

pub mod conversations;
mod transcript;

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use super::{Adapter, Ask, Choices, Context, Launch, Mark, Model, Report};
use crate::config::Config;
use crate::model::Session;
use crate::stats::Transcript;
use crate::util::sh_quote;
use transcript::ClaudeTranscript;

pub const CLAUDE: &str = "claude";

/// Model aliases Claude accepts (`claude --help`); any full model name works too.
const MODELS: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// Current models by full id, offered next to the aliases for pinning a version.
const PINNED: &[&str] = &[
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-1",
];

/// Claude's `--effort` levels.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

pub struct Claude;

impl Adapter for Claude {
    fn reports(&self) -> bool {
        true
    }

    fn resumes(&self) -> bool {
        true
    }

    fn adopts(&self) -> bool {
        true
    }

    /// Its spark, in its brand orange.
    fn mark(&self) -> Option<Mark> {
        Some(Mark {
            glyph: "✱",
            rgb: Some((217, 119, 87)),
        })
    }

    fn choices(&self) -> Option<Choices> {
        Some(Choices { efforts: EFFORTS })
    }

    /// Claude can't list its models, so roster knows them: the aliases, then
    /// the current ones by id.
    fn models(&self, _config: &Config, _ask: &Ask<'_>) -> Vec<Model> {
        let aliases = MODELS.iter().map(|m| Model {
            id: m.to_string(),
            note: None,
        });
        let pinned = PINNED.iter().map(|id| Model {
            id: id.to_string(),
            note: Some(crate::util::model_name(id)),
        });
        aliases.chain(pinned).collect()
    }

    /// A new conversation, or its existing one resumed.
    fn launch(&self, s: &Session, mut argv: Vec<String>, cx: &Context) -> Result<Launch> {
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
            // An adopted copy until it has a transcript of its own (Claude writes it
            // with the first message): fork the original into the copy's id.
            _ if s.fork_from.is_some() => {
                let id = s
                    .conversation_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let from = s.fork_from.clone().unwrap_or_default();
                argv.extend([
                    "--resume".into(),
                    from,
                    "--fork-session".into(),
                    "--session-id".into(),
                    id.clone(),
                ]);
                id
            }
            (existing, _) => {
                let id = existing
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                argv.extend(["--session-id".into(), id.clone()]);
                id
            }
        };
        argv.extend(["--settings".into(), settings(cx.roster)]);
        Ok(Launch {
            argv,
            conversation_id: Some(conversation_id),
        })
    }

    /// Hook payloads carry the current conversation, which changes after
    /// /clear; SessionStart names the model, Stop the effort.
    fn report(&self, payload: &Value) -> Report {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
        Report {
            conversation_id: text(payload.get("session_id")),
            transcript_path: text(payload.get("transcript_path")),
            cleared: payload.get("source").and_then(Value::as_str) == Some("clear"),
            model: text(payload.get("model")),
            effort: text(payload.pointer("/effort/level")),
            ..Report::default()
        }
    }

    fn names_its_task(&self) -> bool {
        true
    }

    /// "✳ Fix the parser" → "Fix the parser"; None while Claude shows its
    /// default title. Claude's titles start with a glyph; anything else is
    /// tmux's default (the host name).
    fn title(&self, pane_title: &str) -> Option<String> {
        if pane_title.chars().next().is_none_or(char::is_alphanumeric) {
            return None;
        }
        let title = pane_title
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .trim();
        (!title.is_empty() && title != "Claude Code").then(|| title.to_string())
    }

    fn transcript(&self, path: &Path) -> Option<Box<dyn Transcript>> {
        Some(Box::new(ClaudeTranscript::new(path)))
    }

    /// `claude -p`: no tools, no settings (so no hooks), nothing saved; Haiku
    /// unless `title_model` says otherwise.
    fn oneoff(&self, config: &Config) -> Option<Vec<String>> {
        let agent = config.agent(CLAUDE);
        let command = agent.command.unwrap_or_else(|| CLAUDE.to_string());
        let model = agent.title_model.unwrap_or_else(|| "haiku".to_string());
        let argv = [
            command.as_str(),
            "-p",
            "--no-session-persistence",
            "--setting-sources",
            "",
            "--tools",
            "",
            "--model",
            &model,
        ];
        Some(argv.map(String::from).to_vec())
    }
}

/// Extra settings for this run only: hooks that report to roster. They run next to
/// the user's and the project's own hooks.
fn settings(roster_exe: &Path) -> String {
    let report = |kind: &str| json!([{"hooks": [{"type": "command", "command": format!("{} report {kind}", sh_quote(&roster_exe.to_string_lossy()))}]}]);
    json!({"hooks": {
        "SessionStart": report("session"),
        // Working too, and tells that you gave it a task.
        "UserPromptSubmit": report("prompt"),
        "PostToolUse": report("working"),
        "PermissionRequest": report("needs-input"),
        "Stop": report("idle"),
    }})
    .to_string()
}

/// The last `"model":"claude-…"` in (the last megabyte of) a transcript, for
/// sessions that predate recording the model.
pub fn last_model_in(transcript: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(1 << 20)))
        .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let key = "\"model\":\"claude-";
    let start = text.rfind(key)? + key.len() - "claude-".len();
    let end = start + text[start..].find('"')?;
    Some(text[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::{config_with, cx};
    use crate::agent::{check_effort, launch, read_report};
    use crate::config::AgentConfig;
    use crate::model::tests::session;

    fn arg_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|a| a == flag)
            .map(|i| argv[i + 1].as_str())
    }

    #[test]
    fn first_start_assigns_a_conversation_id() {
        let s = session("a");
        let l = launch(&s, &Config::default(), &cx("/bin/roster")).unwrap();
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
        let l = launch(&s, &Config::default(), &cx("/bin/roster")).unwrap();
        assert_eq!(arg_after(&l.argv, "--session-id"), Some("c-1"));

        std::fs::write(&transcript, "{}").unwrap();
        let l = launch(&s, &Config::default(), &cx("/bin/roster")).unwrap();
        assert_eq!(arg_after(&l.argv, "--resume"), Some("c-1"));
        assert_eq!(l.conversation_id.as_deref(), Some("c-1"));
    }

    #[test]
    fn uses_configured_command_and_args() {
        let agent = AgentConfig {
            command: Some("/opt/claude".into()),
            args: vec!["--model".into(), "haiku".into()],
            ..AgentConfig::default()
        };
        let l = launch(
            &session("a"),
            &config_with("claude", agent),
            &cx("/bin/roster"),
        )
        .unwrap();
        assert_eq!(l.argv[..3], ["/opt/claude", "--model", "haiku"]);
    }

    #[test]
    fn hooks_report_through_the_quoted_roster_path() {
        let l = launch(&session("a"), &Config::default(), &cx("/my tools/roster")).unwrap();
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
    fn an_adopted_copy_forks_on_its_first_start() {
        let mut s = session("a");
        s.conversation_id = Some("copy".into());
        s.fork_from = Some("original".into());
        let l = launch(&s, &Config::default(), &cx("/bin/roster")).unwrap();
        let i = l.argv.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(
            l.argv[i..i + 5],
            [
                "--resume",
                "original",
                "--fork-session",
                "--session-id",
                "copy"
            ]
        );
        assert_eq!(l.conversation_id.as_deref(), Some("copy"));
        // Once the copy has its own transcript, it resumes that.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("copy.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        s.transcript_path = Some(transcript.to_string_lossy().into_owned());
        let l = launch(&s, &Config::default(), &cx("/bin/roster")).unwrap();
        let i = l.argv.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(l.argv[i..i + 2], ["--resume", "copy"]);
        assert!(!l.argv.contains(&"--fork-session".to_string()));
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
        let l = launch(&s, &config_with("claude", agent), &cx("/bin/roster")).unwrap();
        assert_eq!(
            l.argv[1..7],
            ["--model", "sonnet", "--model", "opus", "--effort", "high"]
        );
        assert!(check_effort(CLAUDE, "xhigh").is_ok() && check_effort(CLAUDE, "extreme").is_err());
    }

    #[test]
    fn reads_what_hooks_report() {
        let report = read_report(
            CLAUDE,
            Some(
                r#"{"session_id":"c-1","transcript_path":"/t/c-1.jsonl","source":"startup","model":"claude-opus-5-5[1m]"}"#,
            ),
        );
        assert_eq!(
            report,
            Report {
                conversation_id: Some("c-1".into()),
                transcript_path: Some("/t/c-1.jsonl".into()),
                cleared: false,
                model: Some("claude-opus-5-5[1m]".into()),
                ..Report::default()
            }
        );
        let report = read_report(CLAUDE, Some(r#"{"session_id":"c-2","source":"clear"}"#));
        assert!(report.cleared);
        let report = read_report(CLAUDE, Some(r#"{"effort":{"level":"xhigh"}}"#));
        assert_eq!(report.effort.as_deref(), Some("xhigh"));
        assert_eq!(read_report(CLAUDE, Some("not json")), Report::default());
        assert_eq!(read_report(CLAUDE, None), Report::default());
    }

    #[test]
    fn titles_are_made_by_haiku_unless_told() {
        let argv = Claude.oneoff(&Config::default()).unwrap();
        assert_eq!(argv[..2], ["claude", "-p"]);
        assert_eq!(arg_after(&argv, "--model"), Some("haiku"));
        let agent = AgentConfig {
            title_model: Some("sonnet".into()),
            ..AgentConfig::default()
        };
        let argv = Claude.oneoff(&config_with("claude", agent)).unwrap();
        assert_eq!(arg_after(&argv, "--model"), Some("sonnet"));
    }

    #[test]
    fn reads_claudes_task_title() {
        let title = |t: &str| Claude.title(t);
        assert_eq!(
            title("✳ tmux pane explanation").as_deref(),
            Some("tmux pane explanation")
        );
        assert_eq!(title("⠐ Fix the parser").as_deref(), Some("Fix the parser"));
        assert_eq!(title("✳ Claude Code"), None);
        assert_eq!(title(""), None);
        assert_eq!(title("computer"), None);
    }

    #[test]
    fn finds_the_last_model_in_a_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        fs::write(&path, "{\"model\":\"claude-haiku-5-5\"}\n{\"model\":\"<synthetic>\"}\n{\"message\":{\"model\":\"claude-opus-5-5\"}}\n").unwrap();
        assert_eq!(last_model_in(&path).as_deref(), Some("claude-opus-5-5"));
        assert_eq!(last_model_in(&dir.path().join("missing")), None);
    }
}
