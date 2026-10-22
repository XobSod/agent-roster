//! Pi (the pi coding agent): roster's own extension, loaded into each run
//! with `-e`, reports its state; `--session-id` resumes its conversation, and
//! its session file gives the statistics. Whatever roster needs that pi
//! doesn't do goes into that extension, and only that.

mod transcript;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::Value;

use super::{Adapter, Choices, Context, Launch, Report};
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

    /// Any model pi knows, by id or a part of its name; no list to offer.
    fn choices(&self) -> Option<Choices> {
        Some(Choices {
            aliases: &[],
            pinned: &[],
            efforts: THINKING,
        })
    }

    /// `--session-id` opens the conversation, or creates it the first time.
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
    fn reads_what_the_extension_reports() {
        let report = read_report(
            PI,
            Some(
                r#"{"conversation_id":"c-1","transcript_path":"/s/c-1.jsonl","cleared":false,"model":"claude-opus-4-8","effort":"medium","title":null,"context_window":1000000}"#,
            ),
        );
        assert_eq!(
            report,
            Report {
                conversation_id: Some("c-1".into()),
                transcript_path: Some("/s/c-1.jsonl".into()),
                cleared: false,
                model: Some("claude-opus-4-8".into()),
                effort: Some("medium".into()),
                title: Some(None),
                context_window: Some(1_000_000),
            }
        );
        assert_eq!(read_report(PI, Some("[1, 2]")), Report::default());
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
