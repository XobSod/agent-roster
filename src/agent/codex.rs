//! Codex (OpenAI's coding agent): hooks given to each run report its state,
//! `codex resume` continues a conversation and `codex fork` copies one, and its
//! rollout file gives the statistics. Codex names neither its conversation nor
//! the task it's on, so its id arrives with the first report and roster makes
//! its title (`[agents.codex] auto_title`). Its hooks say nothing before its
//! first turn, so a first prompt goes on its command line: typed in, it would
//! land in whatever codex shows first (a folder to trust, an update to run).

pub mod conversations;
mod transcript;

use anyhow::{Context as _, Result};
use serde_json::Value;

use super::{Adapter, Ask, Choices, Context, Conversation, Launch, Mark, Model, Report};
use crate::config::Config;
use crate::model::Session;
use crate::stats::Transcript;
use transcript::CodexTranscript;

pub const CODEX: &str = "codex";

/// Codex's reasoning efforts (`model_reasoning_effort`); which of them a model
/// takes, its catalog says (shown with each model, see `catalog_models`).
const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

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

    /// Its hooks run from its first turn on, `SessionStart` too: a first
    /// prompt starts that turn, from its command line.
    fn takes_prompt(&self) -> bool {
        true
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

    /// The models codex lists, as `codex debug models` gives them (its
    /// catalog comes from its server, cached, so roster asks rather than
    /// naming any).
    fn models(&self, config: &Config, ask: &Ask<'_>) -> Vec<Model> {
        let command = config
            .agent(CODEX)
            .command
            .unwrap_or_else(|| CODEX.to_string());
        let argv = [command, "debug".into(), "models".into()];
        ask(&argv).map_or_else(Vec::new, |json| catalog_models(&json))
    }

    /// A new conversation, or `resume` on the one it has; an adopted copy
    /// starts as a `fork` of the original until codex has reported the copy's
    /// own. Codex takes no id at the start, so the conversation comes back with
    /// the first `SessionStart` report rather than from here.
    fn launch(&self, s: &Session, given: Vec<String>, cx: &Context) -> Result<Launch> {
        let (command, args) = given.split_first().context("no command to run codex")?;
        let mut argv = vec![command.clone()];
        // By id, whether or not roster knows its file: codex finds it, and
        // says so if it can't, where starting afresh would lose it unasked.
        match (&s.fork_from, &s.conversation_id) {
            (Some(from), _) => argv.extend(["fork".into(), from.clone()]),
            (None, Some(id)) => argv.extend(["resume".into(), id.clone()]),
            (None, None) => {}
        }
        // The rest after that, `[agents.codex] args` too: given both before
        // and after a subcommand, codex before 0.164 keeps only the `-c`s
        // after it.
        argv.extend(args.iter().cloned());
        // Its own server
        // rather than the shared one, so its tools get the session's
        // environment, the env files' included.
        argv.push("--no-daemon".into());
        // Model and effort as config overrides, which `resume` and `fork` take
        // as a new conversation does. Anything that isn't valid TOML codex
        // takes as a literal, so they're quoted.
        if let Some(model) = &s.model {
            argv.extend(["-c".into(), format!("model={}", toml_str(model))]);
        }
        if let Some(effort) = &s.effort {
            argv.extend([
                "-c".into(),
                format!("model_reasoning_effort={}", toml_str(effort)),
            ]);
        }
        argv.extend(hooks(&cx.roster.to_string_lossy()));
        // A first prompt as codex's own (after `--`, so one starting with a
        // dash isn't taken for an option): its first turn starts with it.
        if let Some(prompt) = &s.pending_prompt {
            argv.extend(["--".into(), prompt.clone()]);
        }
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

    fn transcript(&self, path: &std::path::Path) -> Option<Box<dyn Transcript>> {
        Some(Box::new(CodexTranscript::new(path)))
    }

    /// `codex exec`: nothing kept (`--ephemeral`), nothing changed (a read-only
    /// sandbox; exec asks for no approvals), and the answer alone on stdout.
    /// The model `title_model` (or `model`) picks, else codex's own default.
    fn oneoff(&self, config: &Config) -> Option<Vec<String>> {
        let agent = config.agent(CODEX);
        let mut argv: Vec<String> = vec![
            agent.command.clone().unwrap_or_else(|| CODEX.to_string()),
            "exec".into(),
            "--skip-git-repo-check".into(),
            "--ephemeral".into(),
            "-s".into(),
            "read-only".into(),
        ];
        if let Some(model) = agent.titles_by() {
            argv.extend(["-c".into(), format!("model={}", toml_str(&model))]);
        }
        Some(argv)
    }
}

/// The `-c` arguments giving a run its hooks: each event runs
/// `roster report <kind>` with the payload on its stdin.
///
/// Codex runs a hook only once it's trusted, so it asks to review these the
/// first time (and again if roster moves: the command is part of what's
/// trusted). They're the same every run, so trusting them once does. Skipping
/// the review instead (`--dangerously-bypass-hook-trust`) would skip it for
/// every hook, the project's and your own too.
fn hooks(exe: &str) -> Vec<String> {
    let mut argv = Vec::new();
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

/// The models `codex debug models` lists for choosing (its catalog has hidden
/// ones too), each noted with its name and the efforts it takes, first to
/// last. A catalog it can't fetch is an empty one; anything unreadable is left
/// out.
fn catalog_models(json: &str) -> Vec<Model> {
    let Ok(list) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    list["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
                .filter_map(|m| {
                    let slug = m["slug"].as_str()?;
                    let efforts: Vec<&str> = m["supported_reasoning_levels"]
                        .as_array()
                        .map(|levels| levels.iter().filter_map(|l| l["effort"].as_str()).collect())
                        .unwrap_or_default();
                    let efforts = match efforts[..] {
                        [] => None,
                        [one] => Some(one.to_string()),
                        [first, .., last] => Some(format!("{first}…{last}")),
                    };
                    let note = match (m["display_name"].as_str(), efforts) {
                        (Some(name), Some(efforts)) => Some(format!("{name} · {efforts}")),
                        (Some(name), None) => Some(name.to_string()),
                        (None, efforts) => efforts,
                    };
                    Some(Model {
                        id: slug.to_string(),
                        note,
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
    use std::path::Path;

    fn codex_session() -> Session {
        let mut s = session("a");
        s.agent = CODEX.into();
        s
    }

    fn config() -> Config {
        config_with(CODEX, AgentConfig::default())
    }

    fn argv(s: &Session) -> Vec<String> {
        let cx = crate::agent::Context {
            roster: Path::new("/roster with spaces"),
            state_dir: Path::new("/state"),
        };
        Codex.launch(s, vec!["codex".into()], &cx).unwrap().argv
    }

    #[test]
    fn starts_cold_and_reports_through_its_hooks() {
        let s = codex_session();
        let cx = crate::agent::Context {
            roster: Path::new("/roster"),
            state_dir: Path::new("/state"),
        };
        // No id to give it: codex names its own conversation.
        let launch = Codex.launch(&s, vec!["codex".into()], &cx).unwrap();
        assert!(launch.conversation_id.is_none());
        let argv = argv(&s).join(" ");
        assert!(!argv.contains("resume") && !argv.contains("fork"), "{argv}");
        assert!(argv.contains(r#"hooks.SessionStart=[{hooks=[{type="command",command="/roster with spaces report session"}]}]"#));
        assert!(argv.contains(
            r#"hooks.Stop=[{hooks=[{type="command",command="/roster with spaces report idle"}]}]"#
        ));
        // Its own server, so its tools get the session's environment; and its
        // review of hooks isn't skipped (for every hook, that would be).
        assert!(argv.contains("--no-daemon"), "{argv}");
        assert!(!argv.contains("dangerously"), "{argv}");
        // Quoted for the shell once, for TOML inside the value: no double work.
        assert!(!argv.contains("'\"'"), "{argv}");
    }

    #[test]
    fn resumes_by_id_and_forks_a_copy_until_it_has_its_own() {
        // Its file or not: codex finds it, or says it can't. Never a fresh one.
        let mut s = codex_session();
        s.conversation_id = Some("01a1".into());
        s.transcript_path = Some("/gone/rollout-x.jsonl".into());
        assert_eq!(argv(&s)[1..3], ["resume", "01a1"]);
        // An adopted copy forks the original (its id is roster's until codex
        // reports the copy's own).
        let mut copy = codex_session();
        copy.conversation_id = Some("made-by-roster".into());
        copy.fork_from = Some("01a2".into());
        assert_eq!(argv(&copy)[1..3], ["fork", "01a2"]);
    }

    #[test]
    fn everything_comes_after_the_subcommand() {
        let mut s = codex_session();
        s.conversation_id = Some("01a1".into());
        s.model = Some("gpt-6.1-sol".into());
        s.effort = Some("xhigh".into());
        let cx = crate::agent::Context {
            roster: Path::new("/roster"),
            state_dir: Path::new("/state"),
        };
        // Its own arguments from config.toml too.
        let given = vec!["codex".into(), "-c".into(), "tui.theme=\"x\"".into()];
        let argv = Codex.launch(&s, given, &cx).unwrap().argv;
        assert_eq!(argv[1..5], ["resume", "01a1", "-c", "tui.theme=\"x\""]);
        assert!(
            argv.windows(2)
                .any(|w| w == ["-c", r#"model="gpt-6.1-sol""#])
        );
        assert!(
            argv.windows(2)
                .any(|w| w == ["-c", r#"model_reasoning_effort="xhigh""#])
        );
        assert!(argv.iter().position(|a| a == "--no-daemon").unwrap() > 2);
    }

    #[test]
    fn a_first_prompt_goes_on_its_command_line() {
        let mut s = codex_session();
        s.pending_prompt = Some("-v is broken".into());
        let argv = argv(&s);
        // Last, after `--`, so a dash doesn't make it an option.
        assert_eq!(argv[argv.len() - 2..], ["--", "-v is broken"]);
        assert!(Codex.takes_prompt());
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
    fn lists_the_models_codex_offers_with_their_efforts() {
        let json = r#"{"models":[
            {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"xhigh"}]},
            {"slug":"gpt-reserve"},
            {"slug":"gpt-internal","visibility":"hide"},
            {"nope":true}]}"#;
        assert_eq!(
            catalog_models(json),
            vec![
                Model {
                    id: "gpt-6.1-sol".into(),
                    note: Some("GPT-6.1-Sol · low…xhigh".into())
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
        for effort in ["none", "minimal", "xhigh", "ultra"] {
            assert!(check_effort(CODEX, effort).is_ok(), "{effort}");
        }
        assert!(check_effort(CODEX, "extreme").is_err());
    }
}
