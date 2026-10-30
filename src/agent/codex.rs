//! Codex (OpenAI's coding agent): hooks given to each run report its state,
//! `codex resume` continues a conversation and `codex fork` copies one, and its
//! rollout file gives the statistics. Codex takes no id for a conversation, so
//! its id arrives with the first report; it names the conversation itself,
//! from its first prompt on, in its index of them (`conversations::Names`).
//! Its hooks say nothing before its first turn, so a first prompt goes on its
//! command line: typed in, it would land in whatever codex shows first (a
//! folder to trust, an update to run).

pub mod conversations;
mod limits;
mod shells;
mod subagents;
mod tokens;
mod transcript;
mod trust;

use anyhow::{Context as _, Result};
use serde_json::Value;

use super::{Adapter, Ask, Choices, Context, Conversation, Launch, Mark, Model, Question, Report};
use crate::config::Config;
use crate::model::Session;
use crate::stats::{Jsonl, Transcript};
use crate::tokens::Spending;
use crate::usage::Usage;
use crate::util::sh_quote;
use transcript::CodexTranscript;

pub const CODEX: &str = "codex";

/// Codex's reasoning efforts (`model_reasoning_effort`); which of them a model
/// takes, its catalog says (shown with each model, see `catalog_models`).
const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// The hook events roster listens to, which of each (its matcher, if only
/// some), and the kind of state each one is. `Interrupt` and `SessionEnd`
/// are left alone: an interrupted turn, which reports no `Stop`, ends on the
/// screen as Claude's does, and a closing session leaves its pane, which
/// roster notices.
///
/// Codex's question form (`request_user_input`, in Plan mode) waits for you
/// with no hook of its own: the tool's call starting is its question. Its
/// other question tool, `request_user_input_async`, waits for nothing (the
/// question ends the turn). As Claude's, a compaction is reported only when
/// you ask for one: codex's own, mid-turn, isn't a turn's end.
const HOOKS: &[(&str, Option<&str>, &str)] = &[
    ("SessionStart", None, "session"),
    ("UserPromptSubmit", None, "prompt"),
    ("PreToolUse", Some("^request_user_input$"), "needs-input"),
    ("PostToolUse", None, "working"),
    ("PermissionRequest", None, "needs-input"),
    ("PreCompact", Some("manual"), "compacting"),
    ("PostCompact", Some("manual"), "compacted"),
    ("Stop", None, "idle"),
    ("SubagentStart", None, "subagent-start"),
    ("SubagentStop", None, "subagent-stop"),
];

/// What a one-off run turns off, as config overrides: every feature that
/// gives the model a tool, hooks (all of them: a feature, not a layer), web
/// search, MCP servers, subagents and the project's AGENTS.md. As `-c` rather
/// than `--disable`, so a feature a later codex drops is a warning, not an
/// error. Codex still offers `exec` and `request_user_input`, which no
/// setting takes away; with `code_mode_host` off, `exec` fails at once, and
/// what it would run the read-only sandbox refuses. The user's own AGENTS.md
/// still comes, as nothing turns it off.
const ONEOFF_OFF: &[&str] = &[
    "features.shell_tool=false",
    "features.unified_exec=false",
    "features.code_mode_host=false",
    "features.multi_agent=false",
    "features.sleep_tool=false",
    "features.view_image=false",
    "features.image_generation=false",
    "features.apps=false",
    "features.plugins=false",
    "features.hooks=false",
    "features.skill_search=false",
    "features.tool_suggest=false",
    "features.browser_use=false",
    "features.computer_use=false",
    "features.goals=false",
    r#"web_search="disabled""#,
    "mcp_servers={}",
    "agents.enabled=false",
    "project_doc_max_bytes=0",
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

    fn names_its_conversation(&self) -> bool {
        true
    }

    fn says_subagents_start(&self) -> bool {
        true
    }

    /// Esc ends its own turn only: they finish theirs, and say so.
    fn subagents_outlive_interrupts(&self) -> bool {
        true
    }

    fn keeps_transcript(&self) -> bool {
        true
    }

    fn runs_oneoff(&self) -> bool {
        true
    }

    fn conversations(&self, wanted: &dyn Fn(&str) -> bool) -> Vec<Conversation> {
        conversations::codex_dir()
            .map_or_else(Vec::new, |dir| conversations::conversations(&dir, wanted))
    }

    /// Its knot, in OpenAI's teal.
    fn mark(&self) -> Option<Mark> {
        Some(Mark {
            glyph: "⌬",
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
        ask(Question::of(argv.into())).map_or_else(|_| Vec::new(), |json| catalog_models(&json))
    }

    /// As codex's own server says, which codex's login is enough for.
    fn usage(&self, config: &Config, ask: &Ask<'_>) -> Option<Result<Usage>> {
        let command = config
            .agent(CODEX)
            .command
            .unwrap_or_else(|| CODEX.to_string());
        limits::ask(command, ask)
    }

    fn usage_in(&self, path: &std::path::Path) -> Option<Usage> {
        transcript::last_limits(path)
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
        // dash isn't taken for an option): its first turn starts with it. It
        // goes after the `--` from a file, so it can be longer than tmux
        // takes a command line.
        if s.pending_prompt.is_some() {
            argv.push("--".into());
        }
        Ok(Launch {
            argv,
            conversation_id: None,
            prompt: s.pending_prompt.clone(),
        })
    }

    /// A hook's payload, as Claude's are (`super::hook_report`). Codex's
    /// don't say the effort, which its rollout, named in them, does (the
    /// session's own: a subagent's names none).
    ///
    /// A review (`/review`) runs in a thread of its own, whose hooks come
    /// with the session's id but the review's rollout (named for another
    /// thread) and model: the session takes neither, nor the review's
    /// instructions for its last prompt. Its work is the session's turn all
    /// the same, which ends with no `Stop` (`turn_ended`).
    fn report(&self, payload: &Value) -> Report {
        let report = super::hook_report(payload);
        if let (Some(id), Some(path)) = (&report.conversation_id, &report.transcript_path)
            && !rollout_of(path, id)
        {
            return Report {
                transcript_path: None,
                transcript_written: false,
                model: None,
                ..report
            };
        }
        let path = report.transcript_path.as_deref().map(std::path::Path::new);
        let effort = path.and_then(transcript::last_effort);
        // Its turn over, what it left running: no hook says. Its rollout as
        // far as it went as the hook ran, not a later turn's, read late.
        let shells = match path {
            Some(path) if payload["hook_event_name"] == "Stop" => {
                let upto = payload[crate::report::TRANSCRIPT_SIZE].as_u64();
                shells::left(path, upto)
            }
            _ => Vec::new(),
        };
        Report {
            effort,
            shells,
            ..report
        }
    }

    fn shells_left(&self, transcript: &std::path::Path) -> Vec<super::Backgrounded> {
        shells::left(transcript, None)
    }

    fn shells_ended(
        &self,
        transcript: &std::path::Path,
        from: u64,
    ) -> Option<(super::ShellEnds, u64)> {
        shells::ended(transcript, from)
    }

    fn turn_ended(&self, transcript: &std::path::Path) -> Option<i64> {
        transcript::turn_ended(transcript)
    }

    fn names_its_task(&self) -> bool {
        true
    }

    /// The name codex gave the conversation, once it has: from its first
    /// prompt, kept until it's cleared. (Its terminal title shows it too, but
    /// that's the user's to configure, `[tui] terminal_title`.)
    fn title(&self, _pane_title: &str, conversation: Option<&str>) -> Option<String> {
        conversations::name_of(conversation?)
    }

    fn transcript(&self, path: &std::path::Path) -> Option<Box<dyn Transcript>> {
        Some(Box::new(CodexTranscript::new(path)))
    }

    fn trusts_folders(&self) -> bool {
        true
    }

    fn trust_folder(&self, dir: &std::path::Path) -> Result<bool> {
        match conversations::codex_dir() {
            Some(home) => trust::trust(&home.join("config.toml"), dir),
            None => Ok(false),
        }
    }

    fn subagent_info(&self, transcript: &std::path::Path, id: &str) -> super::SubagentInfo {
        subagents::info(transcript, id)
    }

    fn subagent_finished(&self, transcript: &std::path::Path, id: &str, heard: i64) -> bool {
        subagents::finished(transcript, id, heard)
    }

    fn subagent_doing(&self, file: &std::path::Path) -> Option<String> {
        subagents::doing(file)
    }

    fn token_files(&self) -> Vec<(std::path::PathBuf, String)> {
        conversations::codex_dir().map_or_else(Vec::new, |dir| tokens::files(&dir))
    }

    fn tokens(&self, path: &std::path::Path) -> Option<Box<dyn Spending>> {
        Some(Box::new(Jsonl::<tokens::ModelCalls>::new(path)))
    }

    /// `codex exec`: nothing kept (`--ephemeral`), nothing changed (a read-only
    /// sandbox; exec asks for no approvals), and the answer alone on stdout.
    /// Its tools, hooks, MCP servers and the project's AGENTS.md are turned
    /// off (`ONEOFF_OFF`). The model `title_model` (or `model`) picks, else
    /// codex's own default.
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
        for setting in ONEOFF_OFF {
            argv.extend(["-c".into(), setting.to_string()]);
        }
        if let Some(model) = agent.titles_by() {
            argv.extend(["-c".into(), format!("model={}", toml_str(&model))]);
        }
        Some(argv)
    }
}

/// The `-c` arguments giving a run its hooks: each event runs
/// `roster report <kind>` with the payload on its stdin, through a shell, so
/// roster's path is quoted for it.
///
/// They add to the user's and the project's hooks rather than replace them:
/// though `-c hooks.Stop=[…]` replaces `hooks.Stop` in the merged config,
/// codex gathers hooks layer by layer, and `-c` is a layer of its own.
///
/// Codex runs a hook only once it's trusted, so it asks to review these the
/// first time (and again if roster moves: the command is part of what's
/// trusted). They're the same every run, so trusting them once does. Skipping
/// the review instead (`--dangerously-bypass-hook-trust`) would skip it for
/// every hook, the project's and your own too.
fn hooks(exe: &str) -> Vec<String> {
    let mut argv = Vec::new();
    for (event, matcher, kind) in HOOKS {
        let command = format!("{} report {kind}", sh_quote(exe));
        let matcher = matcher.map_or_else(String::new, |m| format!("matcher={},", toml_str(m)));
        argv.extend([
            "-c".to_string(),
            format!(
                r#"hooks.{event}=[{{{matcher}hooks=[{{type="command",command={}}}]}}]"#,
                toml_str(&command)
            ),
        ]);
    }
    argv
}

/// Whether the rollout at `path` is thread `id`'s, by its name:
/// `rollout-<time>-<thread>.jsonl`, or `-<thread>_<rollout>.jsonl` for one
/// codex wrote the thread over into (an earlier prompt of it edited).
fn rollout_of(path: &str, id: &str) -> bool {
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str());
    let ids = name
        .and_then(|n| n.strip_suffix(".jsonl"))
        .unwrap_or_default();
    let thread = ids.split_once('_').map_or(ids, |(thread, _)| thread);
    thread.ends_with(&format!("-{id}"))
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
        assert!(argv.contains(r#"hooks.SessionStart=[{hooks=[{type="command",command="'/roster with spaces' report session"}]}]"#));
        assert!(argv.contains(
            r#"hooks.Stop=[{hooks=[{type="command",command="'/roster with spaces' report idle"}]}]"#
        ));
        // Its question form's call, and only a compaction you asked for.
        assert!(argv.contains(
            r#"hooks.PreToolUse=[{matcher="^request_user_input$",hooks=[{type="command",command="'/roster with spaces' report needs-input"}]}]"#
        ));
        assert!(argv.contains(r#"hooks.PreCompact=[{matcher="manual",hooks="#));
        assert!(argv.contains(r#"hooks.PostCompact=[{matcher="manual",hooks="#));
        // Its own server, so its tools get the session's environment; and its
        // review of hooks isn't skipped (for every hook, that would be).
        assert!(argv.contains("--no-daemon"), "{argv}");
        assert!(!argv.contains("dangerously"), "{argv}");
        // Quoted for the shell once, for TOML inside the value: no double work.
        assert!(!argv.contains("'\"'"), "{argv}");
    }

    /// Each hook's command, as codex reads it from its `-c` value and runs it
    /// with `sh -c`, runs roster at a path with a space and a quote in it.
    #[test]
    fn its_hooks_run_roster_wherever_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("my tools").join("it's roster");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/sh\necho \"ran $*\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = hooks(&exe.to_string_lossy());
        for ((event, matcher, kind), pair) in HOOKS.iter().zip(argv.chunks(2)) {
            let value = pair[1].strip_prefix(&format!("hooks.{event}=")).unwrap();
            let parsed: toml::Value = toml::from_str(&format!("v = {value}")).unwrap();
            assert_eq!(
                parsed["v"][0].get("matcher").and_then(|m| m.as_str()),
                *matcher
            );
            let command = parsed["v"][0]["hooks"][0]["command"].as_str().unwrap();
            let out = std::process::Command::new("sh")
                .args(["-c", command])
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("ran report {kind}\n"),
                "{command}"
            );
        }
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
        let cx = crate::agent::Context {
            roster: Path::new("/roster"),
            state_dir: Path::new("/state"),
        };
        let launch = Codex.launch(&s, vec!["codex".into()], &cx).unwrap();
        // Last, after `--`, so a dash doesn't make it an option; from a file
        // the launch script reads, past what tmux takes a command line.
        assert_eq!(launch.argv.last().map(String::as_str), Some("--"));
        assert_eq!(launch.prompt.as_deref(), Some("-v is broken"));
        assert!(Codex.takes_prompt());
        s.pending_prompt = None;
        assert!(!argv(&s).contains(&"--".to_string()));
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
        assert!(report.cleared && !report.started);
        // The effort, from the rollout it names, where codex says it.
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout-01a1.jsonl");
        std::fs::write(
            &rollout,
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6-luna\",\"effort\":\"low\"}}\n",
        )
        .unwrap();
        let report =
            Codex.report(&serde_json::json!({"session_id": "01a1", "transcript_path": rollout}));
        assert_eq!(report.effort.as_deref(), Some("low"));
        let source = |source: &str| Codex.report(&serde_json::json!({"source": source}));
        assert!(source("startup").started && !source("startup").cleared);
        assert!(!source("resume").started && !source("resume").cleared);
        assert!(source("compact").compacted && !source("compact").started);
        // A subagent's: its parent's conversation, but its own rollout and
        // model, of which the session takes neither; what kind it is.
        let report = Codex.report(&serde_json::json!({
            "session_id": "01a1",
            "transcript_path": rollout,
            "model": "gpt-6-luna",
            "agent_id": "019a0000-0000-7000-8000-000000000002",
            "agent_type": "worker",
        }));
        assert_eq!(
            report,
            Report {
                subagent: Some("019a0000-0000-7000-8000-000000000002".into()),
                subagent_kind: Some("worker".into()),
                ..Report::default()
            }
        );
        // Its subagents' starts are reported: one heard of without its start
        // started all the same.
        let payload = r#"{"session_id":"01a1","agent_id":"019a","agent_type":"worker"}"#;
        assert!(crate::agent::read_report("codex", Some(payload)).subagents_start_said);
        // A review's, from its own thread: the session's conversation, but
        // the review's rollout and model, of which the session takes
        // neither.
        let report = Codex.report(&serde_json::json!({
            "session_id": "01a1",
            "transcript_path": "/s/rollout-2026-01-01T10-00-00-01a2.jsonl",
            "model": "gpt-6-luna",
            "permission_mode": "bypassPermissions",
            "prompt": "Review the current code changes",
        }));
        assert_eq!(
            report,
            Report {
                conversation_id: Some("01a1".into()),
                ..Report::default()
            }
        );
        // The session's own, written over into a rollout of its own once
        // an earlier prompt is edited: taken.
        let edited = "/s/rollout-2026-01-01T10-00-00-01a1_01b2.jsonl";
        let report = Codex.report(&serde_json::json!({
            "session_id": "01a1", "transcript_path": edited, "model": "gpt-6-luna"}));
        assert_eq!(report.transcript_path.as_deref(), Some(edited));
        assert_eq!(report.model.as_deref(), Some("gpt-6-luna"));
        // Its turn over, what it left running, from its rollout: no other
        // hook's payload says.
        std::fs::write(
            &rollout,
            [
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","type":"session_meta","payload":{"id":"01a1","history_mode":"paginated"}}"#,
                r#"{"timestamp":"2026-01-01T10:00:01.000Z","type":"event_msg","payload":{"type":"task_started"}}"#,
                r#"{"timestamp":"2026-01-01T10:00:02.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Wall time: 1.0 seconds\nProcess running with session ID 2001\nOutput:\n"}}"#,
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        let hook = |event: &str| {
            Codex.report(&serde_json::json!({"hook_event_name": event, "session_id": "01a1", "transcript_path": rollout}))
        };
        let ids = |report: Report| report.shells.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(hook("Stop")), ["2001"]);
        assert!(ids(hook("PostToolUse")).is_empty());
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
        for off in [
            "-c features.shell_tool=false",
            "-c features.code_mode_host=false",
            "-c features.hooks=false",
            "-c mcp_servers={}",
            "-c agents.enabled=false",
            "-c project_doc_max_bytes=0",
        ] {
            assert!(argv.contains(off), "{off} in {argv}");
        }
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
