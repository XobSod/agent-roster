//! Pi (the pi coding agent): roster's own extension, loaded into each run
//! with `-e`, reports its state; `--session-id` resumes its conversation, and
//! its session file gives the statistics. Whatever roster needs that pi
//! doesn't do goes into that extension, and only that.

pub mod conversations;
mod transcript;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::Value;

use super::{Adapter, Ask, Choices, Context, Conversation, Launch, Mark, Model, Report};
use crate::config::Config;
use crate::model::Session;
use crate::stats::Transcript;
use transcript::PiTranscript;

pub const PI: &str = "pi";

/// Pi's `--thinking` levels.
const THINKING: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The extension that reports to roster (see `extension.ts`).
const EXTENSION: &str = include_str!("pi/extension.ts");

pub struct Pi;

impl Adapter for Pi {
    fn reports(&self) -> bool {
        true
    }

    fn resumes(&self) -> bool {
        true
    }

    fn adopts(&self) -> bool {
        true
    }

    fn keeps_transcript(&self) -> bool {
        true
    }

    fn runs_oneoff(&self) -> bool {
        true
    }

    fn conversations(&self, wanted: &dyn Fn(&str) -> bool) -> Vec<Conversation> {
        conversations::sessions_dir()
            .map_or_else(Vec::new, |dir| conversations::conversations(&dir, wanted))
    }

    /// Its π, in the terminal's colour: its logo is black and white.
    fn mark(&self) -> Option<Mark> {
        Some(Mark {
            glyph: "π",
            rgb: None,
        })
    }

    fn choices(&self) -> Option<Choices> {
        Some(Choices { efforts: THINKING })
    }

    /// Those pi has credentials for, as `pi --list-models` lists them (it
    /// takes any of its models by id or a part of its name, too).
    fn models(&self, config: &Config, ask: &Ask<'_>) -> Vec<Model> {
        let command = config.agent(PI).command.unwrap_or_else(|| PI.to_string());
        let argv = [command, "--list-models".into(), "--offline".into()];
        ask(&argv).map_or_else(Vec::new, |table| listed_models(&table))
    }

    /// `--session-id` opens the conversation, or creates it the first time;
    /// an adopted copy is forked from the original (`--fork`) until it has a
    /// session file of its own.
    fn launch(&self, s: &Session, mut argv: Vec<String>, cx: &Context) -> Result<Launch> {
        // After the config's args, so the session's choice wins.
        if let Some(model) = &s.model {
            argv.extend(["--model".into(), model.clone()]);
        }
        if let Some(effort) = &s.effort {
            argv.extend(["--thinking".into(), effort.clone()]);
        }
        let id = s
            .conversation_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let extension = write_extension(cx.state_dir)?;
        // pi makes the copy's file as it starts, before it reports it: it
        // may have one roster hasn't heard of (and refuses to fork onto it).
        let has_file = || {
            s.transcript_path
                .as_deref()
                .is_some_and(|p| Path::new(p).exists())
                || conversations::sessions_dir()
                    .is_some_and(|dir| conversations::session_file(&dir, &id).is_some())
        };
        if let Some(from) = s.fork_from.as_ref().filter(|_| !has_file()) {
            argv.extend(["--fork".into(), from.clone()]);
        }
        argv.extend([
            "--session-id".into(),
            id.clone(),
            "-e".into(),
            extension.to_string_lossy().into_owned(),
        ]);
        Ok(Launch {
            argv,
            conversation_id: Some(id),
        })
    }

    /// The extension reports in roster's own terms.
    fn report(&self, payload: &Value) -> Report {
        Report::deserialize(payload).unwrap_or_default()
    }

    fn transcript(&self, path: &Path) -> Option<Box<dyn Transcript>> {
        Some(Box::new(PiTranscript::new(path)))
    }

    /// `pi -p`, with none of the user's extensions, tools or context files and
    /// no session kept. roster can't tell which of pi's models is a cheap one:
    /// the model `title_model` (or `model`) picks, else pi's own default.
    fn oneoff(&self, config: &Config) -> Option<Vec<String>> {
        let agent = config.agent(PI);
        let mut argv = vec![
            agent.command.clone().unwrap_or_else(|| PI.to_string()),
            "-p".into(),
            "--no-session".into(),
            "--no-tools".into(),
            "--no-extensions".into(),
            "--no-context-files".into(),
            "--offline".into(),
        ];
        if let Some(model) = agent.titles_by() {
            argv.extend(["--model".into(), model]);
        }
        Some(argv)
    }
}

/// The models in `pi --list-models`' table ("provider  model  context  …"),
/// by id, with their context window; by `provider/id` if several providers
/// offer one.
fn listed_models(table: &str) -> Vec<Model> {
    let rows: Vec<Vec<&str>> = table
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|r| r.len() >= 3 && r[0] != "provider")
        .collect();
    let offered = |id: &str| rows.iter().filter(|r| r[1] == id).count();
    rows.iter()
        .map(|r| Model {
            id: if offered(r[1]) > 1 {
                format!("{}/{}", r[0], r[1])
            } else {
                r[1].to_string()
            },
            note: Some(format!("{} context", r[2])),
        })
        .collect()
}

/// Puts the extension in roster's state directory, unless it's there already
/// as it should be; returns its path.
fn write_extension(state_dir: &Path) -> Result<PathBuf> {
    let path = state_dir.join("pi").join("agent-roster.ts");
    if fs::read_to_string(&path).ok().as_deref() != Some(EXTENSION) {
        let dir = path.parent().expect("a file in a directory");
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        // Written whole and moved into place: a pi starting meanwhile reads
        // either version, never half of one. Another roster may be doing the
        // same, so each writes its own file.
        let partial = path.with_extension(format!("ts.{}.partial", std::process::id()));
        fs::write(&partial, EXTENSION)
            .with_context(|| format!("can't write {}", partial.display()))?;
        fs::rename(&partial, &path)
            .with_context(|| format!("can't move {} into place", partial.display()))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::config_with;
    use crate::agent::{check_effort, launch, read_report};
    use crate::config::{AgentConfig, Config};
    use crate::model::tests::session;

    fn pi_session() -> Session {
        let mut s = session("a");
        s.agent = PI.into();
        s
    }

    fn config() -> Config {
        config_with(PI, AgentConfig::default())
    }

    #[test]
    fn starts_with_its_conversation_and_rosters_extension() {
        let dir = tempfile::tempdir().unwrap();
        let cx = Context {
            roster: Path::new("/bin/roster"),
            state_dir: dir.path(),
        };
        let mut s = pi_session();
        s.model = Some("sonnet".into());
        s.effort = Some("high".into());
        let l = launch(&s, &config(), &cx).unwrap();
        let id = l.conversation_id.clone().unwrap();
        let extension = dir.path().join("pi/agent-roster.ts");
        assert_eq!(
            l.argv,
            [
                "pi",
                "--model",
                "sonnet",
                "--thinking",
                "high",
                "--session-id",
                &id,
                "-e",
                extension.to_str().unwrap()
            ]
        );
        assert_eq!(fs::read_to_string(&extension).unwrap(), EXTENSION);
        // Started again, it opens the same conversation; a changed extension
        // file is put back.
        fs::write(&extension, "changed").unwrap();
        s.conversation_id = Some(id.clone());
        let l = launch(&s, &config(), &cx).unwrap();
        assert_eq!(l.conversation_id.as_deref(), Some(id.as_str()));
        assert_eq!(fs::read_to_string(&extension).unwrap(), EXTENSION);
        assert!(check_effort(PI, "minimal").is_ok() && check_effort(PI, "extreme").is_err());
    }

    #[test]
    fn an_adopted_copy_forks_until_it_has_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let cx = Context {
            roster: Path::new("/bin/roster"),
            state_dir: dir.path(),
        };
        let mut s = pi_session();
        s.conversation_id = Some("copy".into());
        s.fork_from = Some("original".into());
        let l = launch(&s, &config(), &cx).unwrap();
        let i = l.argv.iter().position(|a| a == "--fork").unwrap();
        assert_eq!(
            l.argv[i..i + 4],
            ["--fork", "original", "--session-id", "copy"]
        );
        let file = dir.path().join("copy.jsonl");
        fs::write(&file, "{}\n").unwrap();
        s.transcript_path = Some(file.to_string_lossy().into_owned());
        let l = launch(&s, &config(), &cx).unwrap();
        assert!(!l.argv.contains(&"--fork".to_string()));
        assert!(Pi.adopts());
    }

    #[test]
    fn reads_what_the_extension_reports() {
        let report = read_report(
            PI,
            Some(
                r#"{"conversation_id":"c-1","transcript_path":"/s/c-1.jsonl","cleared":false,"started":true,"model":"claude-opus-4-8","effort":"medium","title":null,"context_window":1000000}"#,
            ),
        );
        assert_eq!(
            report,
            Report {
                conversation_id: Some("c-1".into()),
                transcript_path: Some("/s/c-1.jsonl".into()),
                cleared: false,
                started: true,
                model: Some("claude-opus-4-8".into()),
                effort: Some("medium".into()),
                title: Some(None),
                context_window: Some(1_000_000),
            }
        );
        assert_eq!(read_report(PI, Some("[1, 2]")), Report::default());
    }

    #[test]
    fn lists_the_models_pi_has() {
        let table = "provider   model              context  max-out  thinking  images
anthropic  claude-opus-4-8    1M       128K     yes       yes
anthropic  claude-haiku-4-5   200K     64K      yes       yes
openai     gpt-5              400K     128K     yes       yes
openrouter gpt-5              400K     128K     yes       yes
";
        let ask = |argv: &[String]| {
            assert_eq!(argv[1..], ["--list-models", "--offline"]);
            Some(table.to_string())
        };
        let models = Pi.models(&config(), &ask);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-opus-4-8",
                "claude-haiku-4-5",
                "openai/gpt-5",
                "openrouter/gpt-5"
            ]
        );
        assert_eq!(models[0].note.as_deref(), Some("1M context"));
        // It failed, or printed nothing: none to offer, any can be typed.
        assert!(Pi.models(&config(), &|_| None).is_empty());
        assert!(Pi.models(&config(), &|_| Some(String::new())).is_empty());
    }

    #[test]
    fn titles_are_made_by_pis_default_unless_told() {
        let argv = Pi.oneoff(&config()).unwrap();
        assert_eq!(argv[..3], ["pi", "-p", "--no-session"]);
        assert!(!argv.contains(&"--model".to_string()));
        let agent = AgentConfig {
            title_model: Some("claude-haiku-4-5".into()),
            ..AgentConfig::default()
        };
        let argv = Pi.oneoff(&config_with(PI, agent)).unwrap();
        assert_eq!(argv[argv.len() - 2..], ["--model", "claude-haiku-4-5"]);
    }

    #[test]
    fn the_extension_reports_every_state() {
        for kind in [
            "session",
            "prompt",
            "working",
            "idle",
            "needs-input",
            "info",
        ] {
            assert!(
                EXTENSION.contains(&format!("report(\"{kind}\"")),
                "no {kind} report"
            );
        }
    }
}
