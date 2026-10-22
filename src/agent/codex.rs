//! Codex (OpenAI's coding agent): hooks given to each run report its state,
//! `codex resume` continues a conversation and `codex fork` copies one, and its
//! rollout file gives the statistics. Codex names neither its conversation nor
//! the task it's on, so its id arrives with the first report and roster makes
//! its title (`[agents.codex] auto_title`).

pub mod conversations;
mod transcript;

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::{Adapter, Ask, Choices, Context, Conversation, Launch, Mark, Model, Report};
use crate::config::Config;
use crate::model::Session;
use crate::stats::Transcript;
use transcript::CodexTranscript;

pub const CODEX: &str = "codex";

/// Codex's reasoning efforts, as its own model catalog names them.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

/// The hook events roster listens to, and the kind of state each one is.
/// `Interrupt` and `SessionEnd` are left alone: an interrupted turn reports
/// `Stop` too, and a closing session leaves its pane, which roster notices.
const HOOKS: &[(&str, &str)] = &[
    ("SessionStart", "session"),
    ("UserPromptSubmit", "prompt"),
    ("PostToolUse", "working"),
    ("PermissionRequest", "needs-input"),
    ("Stop", "idle"),
];

pub struct Codex;

impl Adapter for Codex {
    fn reports(&self) -> bool {
        true
    }

    /// Codex's TUI runs no `SessionStart` hook: the conversation and its file
    /// are named by the reports of the first turn instead.
    fn reports_its_start(&self) -> bool {
        false
    }

    fn resumes(&self) -> bool {
        true
    }

    fn adopts(&self) -> bool {
        true
    }

    fn conversations(&self, wanted: &dyn Fn(&str) -> bool) -> Vec<Conversation> {
        conversations::codex_dir()
            .map_or_else(Vec::new, |dir| conversations::conversations(&dir, wanted))
    }

    /// Its knot, in OpenAI's teal.
    fn mark(&self) -> Option<Mark> {
        Some(Mark {
            glyph: "⬡",
            rgb: Some((16, 163, 127)),
        })
    }

    fn choices(&self) -> Option<Choices> {
        Some(Choices { efforts: EFFORTS })
    }

    /// The models codex knows about, as `codex debug models` lists them (its
    /// catalog comes from the server, so roster asks rather than naming any).
    fn models(&self, config: &Config, ask: &Ask<'_>) -> Vec<Model> {
        let command = config
            .agent(CODEX)
            .command
            .unwrap_or_else(|| CODEX.to_string());
        let argv = [command, "debug".into(), "models".into()];
        ask(&argv).map_or_else(Vec::new, |json| catalog_models(&json))
    }

    /// A new conversation, or `resume` on the one it has; an adopted copy
    /// starts as a `fork` of the original until it has a file of its own.
    /// Codex takes no id at the start, so the conversation comes back with the
    /// first `SessionStart` report rather than from here.
    fn launch(&self, s: &Session, mut argv: Vec<String>, cx: &Context) -> Result<Launch> {
        // Model and effort as config overrides, the way codex spells them: a
        // flag of the top level wouldn't reach `resume` and `fork`. Anything
        // that isn't valid TOML codex takes as a literal, so they're quoted.
        if let Some(model) = &s.model {
            argv.extend(["-c".into(), format!("model={}", toml_str(model))]);
        }
        if let Some(effort) = &s.effort {
            argv.extend([
                "-c".into(),
                format!("model_reasoning_effort={}", toml_str(effort)),
            ]);
        }
        // Codex writes its rollout as it starts a conversation; until there is
        // one, there's nothing to resume.
        let has_file = s
            .transcript_path
            .as_deref()
            .is_some_and(|p| Path::new(p).exists());
        match (&s.conversation_id, &s.fork_from, has_file) {
            (_, Some(from), false) => argv.extend(["fork".into(), from.clone()]),
            (Some(id), _, true) => argv.extend(["resume".into(), id.clone()]),
            _ => {}
        }
        argv.extend(hooks(&cx.roster.to_string_lossy()));
        Ok(Launch {
            argv,
            conversation_id: None,
        })
    }

    /// Codex's payloads name the conversation and the file it's kept in, both
    /// of which change when it's cleared; `source` says how it started.
    fn report(&self, payload: &Value) -> Report {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
        Report {
            conversation_id: text(payload.get("session_id")),
            transcript_path: text(payload.get("transcript_path")),
            cleared: payload.get("source").and_then(Value::as_str) == Some("clear"),
            model: text(payload.get("model")),
            ..Report::default()
        }
    }

    fn transcript(&self, path: &Path) -> Option<Box<dyn Transcript>> {
        Some(Box::new(CodexTranscript::new(path)))
    }

    /// `codex exec`: nothing kept (`--ephemeral`), nothing asked (a read-only
    /// sandbox, no approvals), and the answer alone on stdout. The model
    /// `title_model` (or `model`) picks, else codex's own default.
    fn oneoff(&self, config: &Config) -> Option<Vec<String>> {
        let agent = config.agent(CODEX);
        let mut argv: Vec<String> = [
            agent.command.clone().unwrap_or_else(|| CODEX.to_string()),
            "exec".into(),
            "--skip-git-repo-check".into(),
            "--ephemeral".into(),
            "-s".into(),
            "read-only".into(),
            "-c".into(),
            r#"approval_policy="never""#.into(),
        ]
        .to_vec();
        if let Some(model) = agent.titles_by() {
            argv.extend(["-c".into(), format!("model={}", toml_str(&model))]);
        }
        Some(argv)
    }
}

/// The `-c` arguments giving a run its hooks: each event runs
/// `roster report <kind>` with the payload on its stdin.
///
/// Codex keeps its hooks in a config file of its own, trusted once by hand, so
/// they come as inline TOML instead, and the run says it needn't be asked to
/// trust them (`--dangerously-bypass-hook-trust`), which codex itself calls
/// out as a warning when it starts.
fn hooks(exe: &str) -> Vec<String> {
    let mut argv = vec!["--dangerously-bypass-hook-trust".to_string()];
    for (event, kind) in HOOKS {
        let command = format!("{exe} report {kind}");
        argv.extend([
            "-c".to_string(),
            format!(
                r#"hooks.{event}=[{{hooks=[{{type="command",command={}}}]}}]"#,
                toml_str(&command)
            ),
        ]);
    }
    argv
}

/// A string as codex reads it in a `-c` value: quoted, with the few escapes
/// TOML has.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', r#"\""#))
}

/// The slugs and display names in `codex debug models`' JSON. A catalog it
/// can't fetch is an empty one; anything unreadable is left out.
fn catalog_models(json: &str) -> Vec<Model> {
    let Ok(list) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    list["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    let slug = m["slug"].as_str()?;
                    Some(Model {
                        id: slug.to_string(),
                        note: m["display_name"].as_str().map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::config_with;
    use crate::config::AgentConfig;
    use crate::model::tests::session;
    use std::fs;

    fn codex_session() -> Session {
        let mut s = session("a");
        s.agent = CODEX.into();
        s
    }

    fn config() -> Config {
        config_with(CODEX, AgentConfig::default())
    }

    #[test]
    fn starts_cold_and_reports_through_its_hooks() {
        let cx = crate::agent::Context {
            roster: Path::new("/roster with spaces"),
            state_dir: Path::new("/state"),
        };
        let launch = Codex
            .launch(&codex_session(), vec!["codex".into()], &cx)
            .unwrap();
        let argv = launch.argv.join(" ");
        // No id to give it: codex names its own conversation.
        assert!(launch.conversation_id.is_none());
        assert!(!argv.contains("resume"), "{argv}");
        assert!(argv.contains("--dangerously-bypass-hook-trust"));
        assert!(argv.contains(r#"hooks.SessionStart=[{hooks=[{type="command",command="/roster with spaces report session"}]}]"#));
        assert!(argv.contains(
            r#"hooks.Stop=[{hooks=[{type="command",command="/roster with spaces report idle"}]}]"#
        ));
        // Quoted for the shell once, for TOML inside the value: no double work.
        assert!(!argv.contains("'\"'"), "{argv}");
    }

    #[test]
    fn resumes_what_it_has_a_file_for_and_forks_what_it_hasnt() {
        let cx = crate::agent::Context {
            roster: Path::new("/roster"),
            state_dir: Path::new("/state"),
        };
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rollout-x.jsonl");
        fs::write(&file, "{}\n").unwrap();

        let mut s = codex_session();
        s.conversation_id = Some("01a1".into());
        s.transcript_path = Some(file.to_string_lossy().into_owned());
        let argv = Codex.launch(&s, vec!["codex".into()], &cx).unwrap().argv;
        let at = argv.iter().position(|a| a == "resume").unwrap();
        assert_eq!(
            (argv[at].as_str(), argv[at + 1].as_str()),
            ("resume", "01a1")
        );

        // An adopted copy forks the original until it has a file of its own.
        let mut copy = codex_session();
        copy.fork_from = Some("01a2".into());
        let argv = Codex.launch(&copy, vec!["codex".into()], &cx).unwrap().argv;
        let at = argv.iter().position(|a| a == "fork").unwrap();
        assert_eq!((argv[at].as_str(), argv[at + 1].as_str()), ("fork", "01a2"));

        // A conversation whose file is gone: started again, as a new one.
        std::fs::remove_file(&file).unwrap();
        let argv = Codex.launch(&s, vec!["codex".into()], &cx).unwrap().argv;
        assert!(!argv.iter().any(|a| a == "resume" || a == "fork"));
    }

    #[test]
    fn its_model_and_effort_come_as_config_overrides() {
        let cx = crate::agent::Context {
            roster: Path::new("/roster"),
            state_dir: Path::new("/state"),
        };
        let mut s = codex_session();
        s.model = Some("gpt-6.1-sol".into());
        s.effort = Some("xhigh".into());
        let argv = Codex.launch(&s, vec!["codex".into()], &cx).unwrap().argv;
        let at = argv.iter().position(|a| a == "-c").unwrap();
        assert_eq!(argv[at + 1], r#"model="gpt-6.1-sol""#);
        assert!(
            argv.windows(2)
                .any(|w| w[1] == r#"model_reasoning_effort="xhigh""#)
        );
    }

    #[test]
    fn reads_what_its_hooks_report() {
        let report = Codex.report(&serde_json::json!({
            "session_id": "01a1",
            "transcript_path": "/s/rollout-01a1.jsonl",
            "model": "gpt-6.1-sol",
            "source": "clear",
            "prompt": "unimportant for roster",
        }));
        assert_eq!(
            (
                report.conversation_id.as_deref(),
                report.transcript_path.as_deref(),
                report.model.as_deref()
            ),
            (
                Some("01a1"),
                Some("/s/rollout-01a1.jsonl"),
                Some("gpt-6.1-sol")
            )
        );
        assert!(report.cleared);
        // A payload of another shape reports nothing, and isn't an error.
        assert_eq!(Codex.report(&Value::Null), Report::default());
    }

    #[test]
    fn lists_the_models_codex_has() {
        let json = r#"{"models":[{"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol"},
            {"slug":"gpt-reserve"},{"nope":true}]}"#;
        assert_eq!(
            catalog_models(json),
            vec![
                Model {
                    id: "gpt-6.1-sol".into(),
                    note: Some("GPT-6.1-Sol".into())
                },
                Model {
                    id: "gpt-reserve".into(),
                    note: None
                },
            ]
        );
        assert!(catalog_models("not json").is_empty());
    }

    #[test]
    fn titles_are_made_by_codexs_default_unless_told() {
        let argv = Codex.oneoff(&config()).unwrap().join(" ");
        assert!(
            argv.contains("exec --skip-git-repo-check --ephemeral"),
            "{argv}"
        );
        assert!(argv.contains("-s read-only"));
        assert!(!argv.contains("model="));
        let config = config_with(
            CODEX,
            AgentConfig {
                title_model: Some("gpt-reserve".into()),
                ..AgentConfig::default()
            },
        );
        assert!(
            Codex
                .oneoff(&config)
                .unwrap()
                .join(" ")
                .contains(r#"model="gpt-reserve""#)
        );
    }

    #[test]
    fn its_efforts_are_its_own() {
        use crate::agent::check_effort;
        assert!(check_effort(CODEX, "ultra").is_ok());
        // Claude's set, which codex doesn't take.
        assert!(check_effort(CODEX, "minimal").is_err());
    }
}
