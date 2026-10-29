//! What the agents' accounts have used of their limits, as the TUI knows it:
//! asked of each agent in the background (at the start, every few minutes,
//! when the usage view opens, and on request), and taken from
//! the transcripts of agents that write it as they go (codex) whenever they
//! change, which costs nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;

use crate::model::{Session, State};
use crate::usage::Usage;

/// How often the agents are asked while it's shown.
pub const EVERY: Duration = Duration::from_secs(5 * 60);
/// Opening the view asks again, unless asked this recently.
const RECENT: Duration = Duration::from_secs(60);
/// What's known this long shows as of when it was so.
pub const STALE_MS: i64 = 15 * 60_000;

/// What the agents asked answer: those that say, in the order agents are
/// offered.
pub type Answers = Vec<(String, Result<Usage>)>;

#[derive(Default)]
pub struct Limits {
    /// Agent by agent: what each last said, in the order they're offered.
    pub agents: Vec<Said>,
    asking: Option<Receiver<Answers>>,
    asked: Option<Instant>,
    /// The view just opened, or asked to ask again: whether it insisted
    /// (asking even if it just did).
    wanted: Option<bool>,
    /// The transcripts read for limits, as of when each last changed.
    read: HashMap<PathBuf, SystemTime>,
}

/// What an agent last said of its account's limits.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Said {
    pub agent: String,
    /// The latest it said, from its service or its transcript.
    pub usage: Option<Usage>,
    /// Why it couldn't be asked the last time, if it couldn't.
    pub error: Option<String>,
}

impl Limits {
    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// When it last asked.
    pub fn asked(&self) -> Option<Instant> {
        self.asked
    }

    /// Asks again soon: the view opened (`insist` false: unless it just
    /// did), or it was asked to.
    pub fn want(&mut self, insist: bool) {
        self.wanted = Some(insist || self.wanted == Some(true));
    }

    /// Whether to ask now, `shown` meaning something shows it: never asked,
    /// wanted, or due again.
    pub fn due(&self, shown: bool) -> bool {
        if self.asking() {
            return false;
        }
        let since = |d| self.asked.is_none_or(|at| at.elapsed() >= d);
        match self.wanted {
            Some(true) => true,
            Some(false) => since(RECENT),
            None => shown && since(EVERY),
        }
    }

    /// Asks on a thread: `ask` is `Roster::usage_reader`'s.
    pub fn ask(&mut self, ask: impl FnOnce() -> Answers + Send + 'static) {
        let (send, answers) = std::sync::mpsc::channel();
        std::thread::spawn(move || send.send(ask()));
        self.asking = Some(answers);
        self.asked = Some(Instant::now());
        self.wanted = None;
    }

    /// Takes the answers if they've come; whether they have.
    pub fn arrived(&mut self) -> bool {
        let Some(asking) = &self.asking else {
            return false;
        };
        let answer = match asking.try_recv() {
            Ok(answers) => Some(answers),
            Err(TryRecvError::Empty) => return false,
            // The thread gone without an answer: what was known stays.
            Err(TryRecvError::Disconnected) => None,
        };
        self.asking = None;
        if let Some(answers) = answer {
            self.take(answers);
        }
        true
    }

    /// The agents that answered, in their order: what each said, or why it
    /// couldn't, keeping what it said before (an agent that no longer says
    /// is gone). What a transcript said since it was asked stays.
    fn take(&mut self, answers: Answers) {
        let mut before = std::mem::take(&mut self.agents);
        for (agent, answer) in answers {
            let known = before
                .iter()
                .position(|s| s.agent == agent)
                .map(|i| before.remove(i));
            let mut said = known.unwrap_or(Said {
                agent,
                usage: None,
                error: None,
            });
            match answer {
                Ok(usage) => {
                    if said.usage.as_ref().is_none_or(|u| u.at <= usage.at) {
                        said.usage = Some(usage);
                    }
                    said.error = None;
                }
                Err(e) => said.error = Some(format!("{e:#}")),
            }
            self.agents.push(said);
        }
    }

    /// Reads what the running sessions' transcripts say of their agents'
    /// limits, each only once it has changed; whether that told anything new.
    pub fn read_transcripts(&mut self, sessions: &[Session]) -> bool {
        let mut any = false;
        for s in sessions {
            let Some(path) = s.transcript_path.as_deref() else {
                continue;
            };
            if s.deleted_at.is_some() || s.state == State::Stopped {
                continue;
            }
            let path = PathBuf::from(path);
            let Ok(changed) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
                continue;
            };
            if self.read.get(&path) == Some(&changed) {
                continue;
            }
            self.read.insert(path.clone(), changed);
            if let Some(usage) = crate::agent::adapter(&s.agent).usage_in(&path) {
                any |= self.written(&s.agent, usage);
            }
        }
        any
    }

    /// What `agent`'s transcript says, if it's newer than what's known;
    /// whether it was. Its notes (a transcript has none) stay.
    fn written(&mut self, agent: &str, mut usage: Usage) -> bool {
        let i = match self.agents.iter().position(|s| s.agent == agent) {
            Some(i) => i,
            None => {
                self.agents.push(Said {
                    agent: agent.to_string(),
                    usage: None,
                    error: None,
                });
                self.agents.len() - 1
            }
        };
        let said = &mut self.agents[i];
        if let Some(known) = &said.usage {
            if known.at >= usage.at {
                return false;
            }
            if usage.notes.is_empty() {
                usage.notes = known.notes.clone();
            }
        }
        said.usage = Some(usage);
        said.error = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Window;

    fn usage(used: f64, at: i64) -> Usage {
        Usage {
            plan: None,
            windows: vec![Window {
                minutes: 300,
                used,
                resets_at: None,
            }],
            notes: Vec::new(),
            at,
        }
    }

    #[test]
    fn an_answer_replaces_what_was_said_and_an_error_keeps_it() {
        let mut limits = Limits::default();
        limits.take(vec![
            ("claude".into(), Ok(usage(10.0, 1))),
            ("codex".into(), Err(anyhow::anyhow!("no answer"))),
        ]);
        assert_eq!(limits.agents[0].usage, Some(usage(10.0, 1)));
        assert_eq!(limits.agents[1].usage, None);
        assert_eq!(limits.agents[1].error.as_deref(), Some("no answer"));
        limits.take(vec![
            ("claude".into(), Err(anyhow::anyhow!("expired"))),
            ("codex".into(), Ok(usage(20.0, 2))),
        ]);
        // What was said before stays, with why it couldn't say again.
        assert_eq!(limits.agents[0].usage, Some(usage(10.0, 1)));
        assert_eq!(limits.agents[0].error.as_deref(), Some("expired"));
        assert_eq!(limits.agents[1].error, None);
        // One that no longer says (logged out) is gone.
        limits.take(vec![("codex".into(), Ok(usage(30.0, 3)))]);
        assert_eq!(limits.agents.len(), 1);
        assert_eq!(limits.agents[0].agent, "codex");
    }

    #[test]
    fn a_transcript_counts_when_newer_and_keeps_the_notes() {
        let mut limits = Limits::default();
        let mut asked = usage(10.0, 100);
        asked.notes = vec!["2 full resets left".into()];
        limits.take(vec![("codex".into(), Ok(asked))]);
        assert!(!limits.written("codex", usage(5.0, 50)));
        assert!(limits.written("codex", usage(15.0, 150)));
        let said = &limits.agents[0];
        assert_eq!(said.usage.as_ref().unwrap().windows[0].used, 15.0);
        assert_eq!(said.usage.as_ref().unwrap().notes, ["2 full resets left"]);
        // Asked again, an answer older than what was written since doesn't
        // undo it.
        limits.take(vec![("codex".into(), Ok(usage(12.0, 120)))]);
        assert_eq!(limits.agents[0].usage.as_ref().unwrap().at, 150);
        // Before any answer, a transcript tells too.
        let mut limits = Limits::default();
        assert!(limits.written("codex", usage(1.0, 1)));
        assert_eq!(limits.agents[0].agent, "codex");
    }

    #[test]
    fn a_running_sessions_transcript_is_read_each_time_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let write = |used: f64| {
            let at = format!("2026-10-25T09:00:{:02}.000Z", used as u64 / 10);
            let line = serde_json::json!({"timestamp": at, "type": "event_msg", "payload": {"type": "token_count",
                "rate_limits": {"limit_id": "codex",
                    "primary": {"used_percent": used, "window_minutes": 300, "resets_at": 100}}}});
            std::fs::write(&path, format!("{line}\n")).unwrap();
        };
        write(10.0);
        let mut s = crate::model::tests::session("c1");
        s.agent = "codex".into();
        s.state = State::Idle;
        s.transcript_path = Some(path.display().to_string());
        let mut limits = Limits::default();
        assert!(limits.read_transcripts(std::slice::from_ref(&s)));
        // Unchanged, it isn't read again.
        assert!(!limits.read_transcripts(std::slice::from_ref(&s)));
        // Changed, it is (a moment later, so its time differs).
        std::thread::sleep(Duration::from_millis(20));
        write(20.0);
        assert!(limits.read_transcripts(std::slice::from_ref(&s)));
        assert_eq!(
            limits.agents[0].usage.as_ref().unwrap().windows[0].used,
            20.0
        );
        // A stopped session's isn't read.
        std::thread::sleep(Duration::from_millis(20));
        write(30.0);
        s.state = State::Stopped;
        assert!(!limits.read_transcripts(std::slice::from_ref(&s)));
    }

    #[test]
    fn asking_that_ends_without_an_answer_keeps_what_was_known() {
        let mut limits = Limits::default();
        limits.take(vec![("claude".into(), Ok(usage(10.0, 1)))]);
        limits.ask(|| panic!("no answer"));
        let by = Instant::now() + Duration::from_secs(2);
        while !limits.arrived() && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!limits.asking());
        assert_eq!(limits.agents[0].usage, Some(usage(10.0, 1)));
    }

    #[test]
    fn it_asks_at_first_when_due_and_when_wanted() {
        let mut limits = Limits::default();
        // Not shown, it asks only when wanted.
        assert!(!limits.due(false));
        assert!(limits.due(true));
        limits.ask(Vec::new);
        // Not while asking.
        assert!(!limits.due(true));
        let by = Instant::now() + Duration::from_secs(2);
        while !limits.arrived() && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!limits.asking());
        // Just asked: not due, and the view opening doesn't ask again...
        assert!(!limits.due(true));
        limits.want(false);
        assert!(!limits.due(false));
        // ...but asking to does.
        limits.want(true);
        assert!(limits.due(false));
    }
}
