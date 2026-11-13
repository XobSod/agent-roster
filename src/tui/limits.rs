//! What the accounts have used of their limits, as the TUI knows it: asked
//! in the background (at the start, every few minutes, when the usage view
//! opens, and on request) of a CLI Proxy for every account it holds, and of
//! each agent of the services it holds none of (of every agent, without
//! one); and taken from the transcripts of agents that write it as they go
//! (codex) whenever they change, which costs nothing, unless the proxy holds
//! that service's accounts: a transcript then says the limits of whichever
//! account answered, which can't be told.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use crate::model::{Session, State};
use crate::proxy::Account;
use crate::usage::{Asked, Known, Usage, Via};

/// How often the agents are asked while it's shown.
pub const EVERY: Duration = Duration::from_secs(5 * 60);
/// Opening the view asks again, unless asked this recently.
const RECENT: Duration = Duration::from_secs(60);
/// What's known this long shows as of when it was so.
pub const STALE_MS: i64 = 15 * 60_000;

#[derive(Default)]
pub struct Limits {
    /// What each agent, or account of the CLI Proxy's, last said, in the
    /// order they answered.
    pub agents: Vec<Said>,
    /// What the last round went through, once one has come.
    pub via: Option<Via>,
    /// Whether the CLI Proxy has answered since it was set: then it isn't
    /// fallen back from, what it said standing.
    proxy_answered: bool,
    asking: Option<Receiver<Asked>>,
    asked: Option<Instant>,
    /// The view just opened, or asked to ask again: whether it insisted
    /// (asking even if it just did).
    wanted: Option<bool>,
    /// The transcripts read for limits, as of when each last changed.
    read: HashMap<PathBuf, SystemTime>,
}

/// What an agent last said of its account's limits, or a CLI Proxy of one
/// of its accounts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Said {
    /// The agent; for an account of the CLI Proxy's, its provider.
    pub agent: String,
    /// The CLI Proxy's account it's about, as last listed; None for an
    /// agent's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
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

    /// What a round needs from before (`Roster::usage_reader`).
    pub fn known(&self) -> Known {
        let accounts = self
            .agents
            .iter()
            .filter_map(|s| Some((s.account.as_ref()?, s)));
        let mut held: Vec<String> = accounts.clone().map(|(a, _)| a.provider.clone()).collect();
        held.sort();
        held.dedup();
        let plans = accounts
            .filter(|(a, _)| a.provider == "claude")
            .filter_map(|(a, s)| Some((a.id.clone(), s.usage.as_ref()?.plan.clone()?)))
            .collect();
        Known {
            answered: self.proxy_answered,
            held,
            plans,
        }
    }

    /// The CLI Proxy changed (another, none, another key): what it said is
    /// forgotten, and it's asked again at once.
    pub fn forget_proxy(&mut self) {
        self.agents.retain(|s| s.account.is_none());
        self.via = None;
        self.proxy_answered = false;
        // A round asked of the one before: its answer isn't taken, and a
        // new round needn't wait for it. The transcripts are read again.
        self.asking = None;
        self.asked = None;
        self.read.clear();
        self.want(true);
    }

    /// Whether the CLI Proxy holds `agent`'s service's accounts.
    fn proxied(&self, agent: &str) -> bool {
        (self.agents.iter()).any(|s| s.account.as_ref().is_some_and(|a| a.provider == agent))
    }

    /// Asks on a thread: `ask` is `Roster::usage_reader`'s.
    pub fn ask(&mut self, ask: impl FnOnce() -> Asked + Send + 'static) {
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

    /// Those that answered, in their order: what each said, or why it
    /// couldn't, keeping what it said before (one that no longer says, an
    /// agent logged out or an account gone from the proxy, is gone). What a
    /// transcript said since it was asked stays. The proxy failing, having
    /// answered before, its accounts stay as they were.
    fn take(&mut self, asked: Asked) {
        let held = self.known().held;
        let mut before = std::mem::take(&mut self.agents);
        let mut kept = Vec::new();
        match asked.via {
            Via::Proxy(_) => self.proxy_answered = true,
            Via::Failed { .. } => {
                (kept, before) = before.into_iter().partition(|s| s.account.is_some());
            }
            Via::Agents | Via::FellBack { .. } => {}
        }
        for answer in asked.answers {
            let same = |s: &Said| match (&s.account, &answer.account) {
                (None, None) => s.agent == answer.agent,
                (Some(a), Some(b)) => a.same(b),
                _ => false,
            };
            let known = before.iter().position(same).map(|i| before.remove(i));
            let mut said = known.unwrap_or(Said {
                agent: answer.agent,
                account: None,
                usage: None,
                error: None,
            });
            said.account = answer.account;
            match answer.said {
                Ok(Some(mut usage)) => {
                    // A plan asked of its own, not asked again.
                    let plan = said.usage.as_ref().and_then(|u| u.plan.clone());
                    if said.account.is_some() && usage.plan.is_none() {
                        usage.plan = plan;
                    }
                    if said.usage.as_ref().is_none_or(|u| u.at <= usage.at) {
                        said.usage = Some(usage);
                    }
                    said.error = None;
                }
                // Not asked: disabled, or of a service roster can't ask.
                Ok(None) => {
                    said.usage = None;
                    said.error = None;
                }
                Err(e) => said.error = Some(format!("{e:#}")),
            }
            self.agents.push(said);
        }
        self.agents.extend(kept);
        self.via = Some(asked.via);
        // The proxy holding other services now: every transcript is read
        // again, what one said before being of no account while it held it.
        if self.known().held != held {
            self.read.clear();
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
            // Not read while the proxy holds its service, nor marked read,
            // what was read of it before forgotten: once it doesn't, it's
            // read again.
            if self.proxied(&s.agent) {
                self.read.remove(&path);
                continue;
            }
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
    /// whether it was. Its notes (a transcript has none) stay. Not if the
    /// CLI Proxy holds that service's accounts.
    fn written(&mut self, agent: &str, mut usage: Usage) -> bool {
        if self.proxied(agent) {
            return false;
        }
        let own = |s: &Said| s.agent == agent && s.account.is_none();
        let i = match self.agents.iter().position(own) {
            Some(i) => i,
            None => {
                self.agents.push(Said {
                    agent: agent.to_string(),
                    account: None,
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
    use anyhow::Result;

    /// A round of the agents alone.
    fn agents(answers: Vec<(String, Result<Usage>)>) -> Asked {
        Asked {
            via: Via::Agents,
            answers: answers
                .into_iter()
                .map(|(agent, said)| crate::usage::Answer {
                    agent,
                    account: None,
                    said: said.map(Some),
                })
                .collect(),
        }
    }

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
        limits.take(agents(vec![
            ("claude".into(), Ok(usage(10.0, 1))),
            ("codex".into(), Err(anyhow::anyhow!("no answer"))),
        ]));
        assert_eq!(limits.agents[0].usage, Some(usage(10.0, 1)));
        assert_eq!(limits.agents[1].usage, None);
        assert_eq!(limits.agents[1].error.as_deref(), Some("no answer"));
        limits.take(agents(vec![
            ("claude".into(), Err(anyhow::anyhow!("expired"))),
            ("codex".into(), Ok(usage(20.0, 2))),
        ]));
        // What was said before stays, with why it couldn't say again.
        assert_eq!(limits.agents[0].usage, Some(usage(10.0, 1)));
        assert_eq!(limits.agents[0].error.as_deref(), Some("expired"));
        assert_eq!(limits.agents[1].error, None);
        // One that no longer says (logged out) is gone.
        limits.take(agents(vec![("codex".into(), Ok(usage(30.0, 3)))]));
        assert_eq!(limits.agents.len(), 1);
        assert_eq!(limits.agents[0].agent, "codex");
    }

    #[test]
    fn a_transcript_counts_when_newer_and_keeps_the_notes() {
        let mut limits = Limits::default();
        let mut asked = usage(10.0, 100);
        asked.notes = vec!["2 full resets left".into()];
        limits.take(agents(vec![("codex".into(), Ok(asked))]));
        assert!(!limits.written("codex", usage(5.0, 50)));
        assert!(limits.written("codex", usage(15.0, 150)));
        let said = &limits.agents[0];
        assert_eq!(said.usage.as_ref().unwrap().windows[0].used, 15.0);
        assert_eq!(said.usage.as_ref().unwrap().notes, ["2 full resets left"]);
        // Asked again, an answer older than what was written since doesn't
        // undo it.
        limits.take(agents(vec![("codex".into(), Ok(usage(12.0, 120)))]));
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
        limits.take(agents(vec![("claude".into(), Ok(usage(10.0, 1)))]));
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
        limits.ask(|| agents(Vec::new()));
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

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use crate::usage::{Answer, Window};
    use anyhow::Result;

    fn usage(used: f64, at: i64, plan: Option<&str>) -> Usage {
        Usage {
            plan: plan.map(String::from),
            windows: vec![Window {
                minutes: 300,
                used,
                resets_at: None,
            }],
            notes: Vec::new(),
            at,
        }
    }

    fn account(provider: &str, id: &str) -> Account {
        Account {
            provider: provider.into(),
            id: id.into(),
            email: Some(format!("{id}@example.com")),
            disabled: false,
            sign_in: false,
        }
    }

    fn via_proxy(
        answers: Vec<(Account, Result<Option<Usage>>)>,
        agents: Vec<(&str, Usage)>,
    ) -> Asked {
        let mut all: Vec<Answer> = answers
            .into_iter()
            .map(|(account, said)| Answer {
                agent: account.provider.clone(),
                account: Some(account),
                said,
            })
            .collect();
        all.extend(agents.into_iter().map(|(agent, usage)| Answer {
            agent: agent.into(),
            account: None,
            said: Ok(Some(usage)),
        }));
        Asked {
            via: Via::Proxy("http://127.0.0.1:8317".into()),
            answers: all,
        }
    }

    #[test]
    fn the_proxys_accounts_each_say_and_keep_what_they_said() {
        let mut limits = Limits::default();
        let (max, pro, plus) = (
            account("claude", "a1"),
            account("claude", "b2"),
            account("codex", "c3"),
        );
        limits.take(via_proxy(
            vec![
                (max.clone(), Ok(Some(usage(10.0, 1, Some("max 5x"))))),
                (pro.clone(), Ok(Some(usage(20.0, 1, Some("pro"))))),
                (plus.clone(), Ok(Some(usage(30.0, 1, Some("plus"))))),
            ],
            vec![],
        ));
        assert_eq!(limits.agents.len(), 3);
        let known = limits.known();
        assert!(known.answered);
        assert_eq!(known.held, ["claude", "codex"]);
        // Claude's plans, not to be asked again.
        assert_eq!(known.plans.len(), 2);
        assert_eq!(known.plans["a1"], "max 5x");
        // One failing keeps what it said, with why; one without a plan this
        // time keeps the one it had; one disabled says nothing; one gone is
        // gone.
        let mut off = plus.clone();
        off.disabled = true;
        limits.take(via_proxy(
            vec![
                (max.clone(), Err(anyhow::anyhow!("it asked to wait (429)"))),
                (pro.clone(), Ok(Some(usage(25.0, 2, None)))),
                (off, Ok(None)),
            ],
            vec![],
        ));
        let said = |id: &str| {
            limits
                .agents
                .iter()
                .find(|s| s.account.as_ref().unwrap().id == id)
                .unwrap()
        };
        assert_eq!(said("a1").usage.as_ref().unwrap().windows[0].used, 10.0);
        assert!(said("a1").error.as_deref().unwrap().contains("429"));
        assert_eq!(
            said("b2").usage.as_ref().unwrap().plan.as_deref(),
            Some("pro")
        );
        assert_eq!(said("b2").usage.as_ref().unwrap().windows[0].used, 25.0);
        assert!(said("c3").account.as_ref().unwrap().disabled);
        assert_eq!(said("c3").usage, None);
        limits.take(via_proxy(
            vec![(max.clone(), Ok(Some(usage(11.0, 3, None))))],
            vec![],
        ));
        assert_eq!(limits.agents.len(), 1);
    }

    #[test]
    fn the_proxy_failing_keeps_its_accounts_once_it_has_answered() {
        let mut limits = Limits::default();
        let max = account("claude", "a1");
        // Never answered: the agents in its place.
        limits.take(Asked {
            via: Via::FellBack {
                url: "http://127.0.0.1:8317".into(),
                why: "can't reach it".into(),
            },
            answers: vec![Answer {
                agent: "claude".into(),
                account: None,
                said: Ok(Some(usage(50.0, 1, Some("max")))),
            }],
        });
        assert!(!limits.known().answered);
        assert_eq!(limits.agents[0].account, None);
        // Answered: its account takes the agent's place...
        limits.take(via_proxy(
            vec![(max.clone(), Ok(Some(usage(10.0, 2, None))))],
            vec![("codex", usage(5.0, 2, None))],
        ));
        assert_eq!(limits.agents.len(), 2);
        assert!(
            limits
                .agents
                .iter()
                .all(|s| s.agent != "claude" || s.account.is_some())
        );
        // ...and failing then, its accounts stay as they were, the agents of
        // other services asked as before.
        limits.take(Asked {
            via: Via::Failed {
                url: "http://127.0.0.1:8317".into(),
                why: "can't reach it".into(),
            },
            answers: vec![Answer {
                agent: "codex".into(),
                account: None,
                said: Ok(Some(usage(6.0, 3, None))),
            }],
        });
        assert_eq!(limits.agents.len(), 2);
        let claude = limits.agents.iter().find(|s| s.account.is_some()).unwrap();
        assert_eq!(claude.usage.as_ref().unwrap().windows[0].used, 10.0);
        assert!(limits.via.as_ref().unwrap().why().is_some());
        assert_eq!(limits.known().held, ["claude"]);
        // Another proxy, or none: forgotten, and asked again.
        limits.forget_proxy();
        assert_eq!(limits.agents.len(), 1);
        assert!(!limits.known().answered);
        assert!(limits.due(false));
    }

    #[test]
    fn a_round_asked_of_the_proxy_before_its_change_isnt_taken() {
        let mut limits = Limits::default();
        let (release, held) = std::sync::mpsc::channel::<()>();
        limits.ask(move || {
            held.recv().ok();
            via_proxy(
                vec![(account("claude", "a1"), Ok(Some(usage(10.0, 1, None))))],
                vec![],
            )
        });
        limits.forget_proxy();
        // Asked again at once, not after the old round...
        assert!(!limits.asking() && limits.due(false));
        release.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // ...whose answer, come since, isn't taken.
        assert!(!limits.arrived());
        assert!(limits.agents.is_empty());
        assert!(!limits.known().answered);
    }

    #[test]
    fn a_transcript_doesnt_count_for_a_service_the_proxy_holds() {
        let mut limits = Limits::default();
        limits.take(via_proxy(
            vec![(account("codex", "c3"), Ok(Some(usage(30.0, 1, None))))],
            vec![],
        ));
        assert!(!limits.written("codex", usage(90.0, 100, None)));
        assert_eq!(limits.agents.len(), 1);
        assert!(limits.written("pi", usage(1.0, 100, None)));
        // A running Codex's rollout isn't read while it holds Codex's
        // accounts, nor marked read: once it doesn't, it is, unchanged.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let line = serde_json::json!({"timestamp": "2026-10-25T09:00:00.000Z", "type": "event_msg",
            "payload": {"type": "token_count", "rate_limits": {"limit_id": "codex",
                "primary": {"used_percent": 20.0, "window_minutes": 300, "resets_at": 100}}}});
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let mut s = crate::model::tests::session("c1");
        s.agent = "codex".into();
        s.state = State::Idle;
        s.transcript_path = Some(path.display().to_string());
        assert!(!limits.read_transcripts(std::slice::from_ref(&s)));
        limits.forget_proxy();
        assert!(limits.read_transcripts(std::slice::from_ref(&s)));
        // Read before the proxy held Codex, unchanged since: once it no
        // longer does (turned off, or no Codex account listed now), read
        // again, whether a transcript was read in between or not.
        let codex = |limits: &mut Limits| {
            limits.take(via_proxy(
                vec![(account("codex", "c3"), Ok(Some(usage(30.0, 1, None))))],
                vec![],
            ))
        };
        let claude_only = |limits: &mut Limits| {
            limits.take(via_proxy(
                vec![(account("claude", "a1"), Ok(Some(usage(30.0, 1, None))))],
                vec![],
            ))
        };
        let read = |limits: &mut Limits| limits.read_transcripts(std::slice::from_ref(&s));
        for between in [true, false] {
            for release in [Limits::forget_proxy as fn(&mut Limits), claude_only] {
                let mut limits = Limits::default();
                assert!(read(&mut limits));
                codex(&mut limits);
                assert!(!limits.agents.iter().any(|s| s.account.is_none()));
                if between {
                    assert!(!read(&mut limits));
                }
                release(&mut limits);
                assert!(read(&mut limits), "between: {between}");
                assert!(
                    limits
                        .agents
                        .iter()
                        .any(|s| s.agent == "codex" && s.account.is_none())
                );
            }
        }
    }
}
