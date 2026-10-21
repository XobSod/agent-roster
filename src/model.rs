//! Projects, sessions, reported events, and how events and screen changes move a
//! session between states.

use std::path::PathBuf;

/// How long a "working" session must go without hook events and without screen
/// changes before it counts as idle. Claude reports no event when a turn is
/// interrupted with Esc; while it really works, its spinner changes the screen
/// several times a second.
pub const QUIET_MS: i64 = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    NeedsInput,
    Idle,
    Stopped,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::NeedsInput => "needs-input",
            State::Idle => "idle",
            State::Stopped => "stopped",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "working" => State::Working,
            "needs-input" => State::NeedsInput,
            "idle" => State::Idle,
            _ => State::Stopped,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub origin: PathBuf,
    /// Defaults for new sessions; None means the agent's own default.
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Project {
    /// Its defaults for new sessions, e.g. "claude · opus (Opus 5.5) · high": an
    /// alias with what it last turned out to be (`models`).
    pub fn defaults_label(&self, models: &std::collections::HashMap<String, String>) -> String {
        let agent = self.agent.as_deref().unwrap_or(crate::agent::CLAUDE);
        let model = self.model.as_ref().map(|m| match models.get(m) {
            Some(name) if name != m => format!("{m} ({name})"),
            _ => m.clone(),
        });
        [Some(agent.to_string()), model, self.effort.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// How many of `sessions` are its: "2 live · 1 archived", "1 live", "none".
    pub fn sessions_label(&self, sessions: &[Session]) -> String {
        let mine = sessions.iter().filter(|s| s.project_id == self.id);
        let (archived, live): (Vec<&Session>, Vec<&Session>) =
            mine.partition(|s| s.deleted_at.is_some());
        let parts: Vec<String> = [(live.len(), "live"), (archived.len(), "archived")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: String,
    pub project_id: i64,
    /// Name of the project (joined in from `projects`).
    pub project: String,
    pub name: String,
    pub agent: String,
    pub workdir: PathBuf,
    pub conversation_id: Option<String>,
    /// Where the agent keeps the conversation; if it exists, the next start resumes.
    pub transcript_path: Option<String>,
    pub state: State,
    pub unread: bool,
    pub exit_status: Option<i32>,
    pub last_event_at: Option<i64>,
    pub screen_hash: Option<String>,
    pub screen_changed_at: Option<i64>,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
    /// Screen activity up to this time has been seen by the user (or is the agent
    /// starting up); only later activity can make a session unread.
    pub baseline_at: Option<i64>,
    /// What the agent says it's working on (Claude's terminal title).
    pub title: Option<String>,
    /// Passed to the agent on every start; None means its own default.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// When `state` last changed.
    pub state_since: Option<i64>,
    /// A first prompt to type into the agent once it's ready.
    pub pending_prompt: Option<String>,
    /// The model and effort the agent reports using (Claude's hook payloads).
    pub running_model: Option<String>,
    pub running_effort: Option<String>,
    /// A title the user set (or had generated); wins over the agent's.
    pub title_override: Option<String>,
    /// Turns started and time spent working, counted by roster for any agent.
    pub turns: i64,
    pub worked_ms: i64,
    /// For an adopted copy: the conversation it was copied from, forked again on
    /// each start until the copy has a transcript of its own.
    pub fork_from: Option<String>,
}

/// A status change reported by an agent hook through `roster report <kind>`.
#[derive(Clone, Debug)]
pub struct Event {
    pub id: i64,
    pub session_id: String,
    pub kind: String,
    pub at: i64,
    pub payload: Option<String>,
}

impl Session {
    /// Name of this session's tmux session on roster's private server.
    pub fn tmux_name(&self) -> String {
        format!("r-{}", self.id)
    }

    /// Whether the name is one roster made up (`<agent>-<n>`): it says nothing
    /// a header would need. A chosen one (a role, say) does.
    pub fn has_default_name(&self) -> bool {
        self.name
            .strip_prefix(self.agent.as_str())
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }

    /// `project/name`, how sessions are shown and addressed.
    pub fn label(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }

    /// Changes the state, noting when it changed, the time spent working, and
    /// turns (working again after idling; after a question, it's the same turn).
    pub fn set_state(&mut self, state: State, at: i64) {
        if self.state == state {
            return;
        }
        if self.state == State::Working {
            self.worked_ms += (at - self.state_since.unwrap_or(at)).max(0);
        }
        if state == State::Working && self.state == State::Idle {
            self.turns += 1;
        }
        self.state = state;
        self.state_since = Some(at);
    }

    /// Time spent working, including a turn in progress.
    pub fn worked(&self, now: i64) -> i64 {
        let current = if self.state == State::Working {
            now - self.state_since.unwrap_or(now)
        } else {
            0
        };
        self.worked_ms + current.max(0)
    }

    /// The list's order: by attention (archived last), then, within each, by
    /// when the session got there, oldest or newest first as `order` says.
    pub fn list_order(&self, other: &Session, order: &crate::config::Order) -> std::cmp::Ordering {
        use crate::config::First;
        let tier = |s: &Session| (s.deleted_at.is_some(), s.attention());
        tier(self).cmp(&tier(other)).then_with(|| {
            let first = match (self.deleted_at, self.state, self.unread) {
                (Some(_), _, _) => order.archived,
                (None, State::NeedsInput, _) | (None, State::Idle, true) => order.needs_input,
                (None, State::Idle, false) => order.idle,
                (None, State::Stopped, _) => order.stopped,
                (None, State::Working, _) => order.working,
            };
            let since = |s: &Session| s.deleted_at.or(s.state_since).unwrap_or(s.created_at);
            match first {
                First::Oldest => since(self).cmp(&since(other)),
                First::Newest => since(other).cmp(&since(self)),
            }
        })
    }

    /// Attention order: blocked on the user, then unread, idle, stopped (idle
    /// without a running agent), and working last: handed off.
    pub fn attention(&self) -> u8 {
        match (self.state, self.unread) {
            (State::NeedsInput, _) => 0,
            (State::Idle, true) => 1,
            (State::Idle, false) => 2,
            (State::Stopped, _) => 3,
            (State::Working, _) => 4,
        }
    }

    /// The model to show: what the agent reports, as it names it, else what was
    /// asked for.
    pub fn model_label(&self) -> Option<String> {
        self.running_model
            .as_deref()
            .map(crate::util::model_name)
            .or_else(|| self.model.clone())
    }

    /// The effort to show: what was asked for, else what the agent reports.
    pub fn effort_label(&self) -> Option<String> {
        self.effort.clone().or_else(|| self.running_effort.clone())
    }

    /// The user's title, else the agent's, else the session's name.
    /// The title: yours if you set one, else the agent's.
    pub fn title(&self) -> Option<&str> {
        self.title_override.as_deref().or(self.title.as_deref())
    }

    pub fn display_title(&self) -> &str {
        self.title().unwrap_or(&self.name)
    }

    /// Applies a reported event.
    pub fn apply(&mut self, e: &Event) {
        if self.deleted_at.is_some() {
            return;
        }
        // Claude's hook payloads carry the current conversation ID, which changes
        // after /clear.
        if let Some(payload) = e
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        {
            if let Some(id) = payload.get("session_id").and_then(|v| v.as_str()) {
                self.conversation_id = Some(id.to_string());
            }
            if let Some(path) = payload.get("transcript_path").and_then(|v| v.as_str()) {
                self.transcript_path = Some(path.to_string());
            }
            // A cleared conversation is a new task.
            if payload.get("source").and_then(|v| v.as_str()) == Some("clear") {
                self.title = None;
            }
            // SessionStart names the model, Stop the effort.
            if let Some(model) = payload.get("model").and_then(|v| v.as_str()) {
                self.running_model = Some(model.to_string());
            }
            if let Some(effort) = payload.pointer("/effort/level").and_then(|v| v.as_str()) {
                self.running_effort = Some(effort.to_string());
            }
        }
        match e.kind.as_str() {
            // A prompt you submitted starts a turn.
            "working" | "prompt" => self.set_state(State::Working, e.at),
            "idle" => {
                self.set_state(State::Idle, e.at);
                self.unread = true;
            }
            "needs-input" => {
                self.set_state(State::NeedsInput, e.at);
                self.unread = true;
            }
            // The agent (re)started or cleared its conversation and waits for a prompt.
            "session" => self.set_state(State::Idle, e.at),
            _ => return,
        }
        self.last_event_at = Some(e.at);
    }

    /// Records a capture of the screen, noting when it last changed.
    pub fn observe_screen(&mut self, hash: &str, now: i64) {
        if self.screen_hash.as_deref() != Some(hash) {
            self.screen_hash = Some(hash.to_string());
            self.screen_changed_at = Some(now);
        }
    }

    /// The screen changed because roster resized the window: the agent's redraw
    /// becomes what later screens are compared with, not activity.
    pub fn redrawn(&mut self, hash: &str) {
        self.screen_hash = Some(hash.to_string());
    }

    /// True if the session claims to be working but no hook has reported anything
    /// for a while, so only the screen can tell whether it still is.
    pub fn needs_screen_check(&self, now: i64) -> bool {
        self.state == State::Working && self.last_event_at.is_none_or(|t| now - t >= QUIET_MS)
    }

    /// Turns a stale "working" into idle once the screen has stopped changing.
    /// Doesn't mark it unread: for Claude this only happens after an interrupt,
    /// which the user did while attached.
    pub fn correct_stale_working(&mut self, now: i64) -> bool {
        let still = self.screen_changed_at.is_some_and(|t| now - t >= QUIET_MS);
        if self.needs_screen_check(now) && still {
            self.set_state(State::Idle, now);
            return true;
        }
        false
    }

    /// Screen-only status for agents without hooks: a changing screen means
    /// working, a screen still for `idle_after` ms means idle. A turn that ends
    /// after activity past the baseline marks the session unread. The first
    /// capture is only a baseline: there's nothing to compare it with.
    pub fn observe_generic(&mut self, hash: &str, now: i64, idle_after: i64) {
        if self.screen_hash.as_deref() != Some(hash) {
            let first = self.screen_hash.is_none();
            self.screen_hash = Some(hash.to_string());
            self.screen_changed_at = Some(now);
            if !first {
                if self.baseline_at.is_none_or(|b| now > b) {
                    self.last_event_at = Some(now);
                }
                self.set_state(State::Working, now);
            }
        } else if self.state == State::Working
            && self
                .screen_changed_at
                .is_some_and(|t| now - t >= idle_after)
        {
            self.set_state(State::Idle, now);
            if self
                .last_event_at
                .is_some_and(|t| self.baseline_at.is_none_or(|b| t > b))
            {
                self.unread = true;
            }
        }
    }

    /// The user has just seen the session (detached from it, or marked it read):
    /// its current screen becomes the baseline.
    pub fn mark_seen(&mut self, screen_hash: Option<String>, now: i64) {
        self.unread = false;
        self.baseline_at = Some(now);
        if let Some(hash) = screen_hash {
            self.screen_hash = Some(hash);
            self.screen_changed_at = Some(now);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn default_names_are_the_agent_and_a_number() {
        let mut s = session("a");
        for (name, default) in [
            ("claude-1", true),
            ("claude-12", true),
            ("claude-", false),
            ("claude-1b", false),
            ("tester", false),
            ("aider-1", false),
        ] {
            s.name = name.into();
            assert_eq!(s.has_default_name(), default, "{name}");
        }
        s.agent = "aider".into();
        s.name = "aider-1".into();
        assert!(s.has_default_name());
    }

    pub fn session(id: &str) -> Session {
        Session {
            id: id.into(),
            project_id: 1,
            project: "demo".into(),
            name: format!("s-{id}"),
            agent: "claude".into(),
            workdir: "/tmp".into(),
            conversation_id: None,
            transcript_path: None,
            state: State::Working,
            unread: false,
            exit_status: None,
            last_event_at: None,
            screen_hash: None,
            screen_changed_at: None,
            created_at: 0,
            deleted_at: None,
            baseline_at: None,
            title: None,
            model: None,
            effort: None,
            state_since: None,
            pending_prompt: None,
            running_model: None,
            running_effort: None,
            title_override: None,
            turns: 0,
            worked_ms: 0,
            fork_from: None,
        }
    }

    fn event(kind: &str, at: i64, payload: Option<&str>) -> Event {
        Event {
            id: 1,
            session_id: "a".into(),
            kind: kind.into(),
            at,
            payload: payload.map(String::from),
        }
    }

    #[test]
    fn turn_end_and_questions_mark_unread() {
        let mut s = session("a");
        s.apply(&event("working", 10, None));
        assert_eq!((s.state, s.unread), (State::Working, false));
        s.apply(&event("needs-input", 20, None));
        assert_eq!((s.state, s.unread), (State::NeedsInput, true));
        s.unread = false;
        s.apply(&event("working", 30, None));
        s.apply(&event("idle", 40, None));
        assert_eq!(
            (s.state, s.unread, s.last_event_at),
            (State::Idle, true, Some(40))
        );
    }

    #[test]
    fn session_start_is_idle_but_not_unread() {
        let mut s = session("a");
        s.apply(&event(
            "session",
            10,
            Some(r#"{"session_id":"c-1","transcript_path":"/t/c-1.jsonl","source":"startup"}"#),
        ));
        assert_eq!((s.state, s.unread), (State::Idle, false));
        assert_eq!(s.conversation_id.as_deref(), Some("c-1"));
        assert_eq!(s.transcript_path.as_deref(), Some("/t/c-1.jsonl"));
    }

    #[test]
    fn clear_switches_the_conversation() {
        let mut s = session("a");
        s.conversation_id = Some("c-1".into());
        s.title = Some("old task".into());
        s.apply(&event(
            "session",
            10,
            Some(r#"{"session_id":"c-2","source":"clear"}"#),
        ));
        assert_eq!(s.conversation_id.as_deref(), Some("c-2"));
        assert_eq!(s.title, None);
    }

    #[test]
    fn remembers_the_model_and_effort_claude_reports() {
        let mut s = session("a");
        s.model = Some("opus".into());
        assert_eq!(s.model_label().as_deref(), Some("opus"));
        s.apply(&event(
            "session",
            10,
            Some(r#"{"model":"claude-opus-5-5[1m]"}"#),
        ));
        s.apply(&event("idle", 20, Some(r#"{"effort":{"level":"xhigh"}}"#)));
        assert_eq!(s.model_label().as_deref(), Some("Opus 5.5 (1M)"));
        assert_eq!(s.effort_label().as_deref(), Some("xhigh"));
    }

    #[test]
    fn counts_turns_and_time_worked() {
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        s.apply(&event("working", 1_000, None));
        s.apply(&event("working", 2_000, None));
        s.apply(&event("needs-input", 4_000, None));
        s.apply(&event("working", 9_000, None));
        s.apply(&event("idle", 10_000, None));
        assert_eq!((s.turns, s.worked_ms), (1, 4_000));
        s.apply(&event("working", 20_000, None));
        assert_eq!((s.turns, s.worked(25_000)), (2, 9_000));
    }

    #[test]
    fn a_users_title_wins() {
        let mut s = session("a");
        assert_eq!(s.display_title(), "s-a");
        s.title = Some("Claude's".into());
        assert_eq!(s.display_title(), "Claude's");
        s.title_override = Some("Mine".into());
        assert_eq!(s.display_title(), "Mine");
    }

    #[test]
    fn ignores_unknown_kinds_and_deleted_sessions() {
        let mut s = session("a");
        s.apply(&event("bogus", 10, None));
        assert_eq!((s.state, s.last_event_at), (State::Working, None));
        s.deleted_at = Some(1);
        s.apply(&event("idle", 20, None));
        assert_eq!(s.state, State::Working);
    }

    #[test]
    fn stale_working_becomes_idle_only_when_screen_is_still() {
        let mut s = session("a");
        s.apply(&event("working", 0, None));
        s.observe_screen("h1", 1000);
        // Hook event too recent.
        assert!(!s.correct_stale_working(3000));
        // Screen still changing.
        s.observe_screen("h2", 4500);
        assert!(!s.correct_stale_working(6000));
        // Unchanged for QUIET_MS.
        s.observe_screen("h2", 8000);
        assert!(s.correct_stale_working(4500 + QUIET_MS));
        assert_eq!((s.state, s.unread), (State::Idle, false));
    }

    /// A generic session as `start` leaves it: working, startup output ignored
    /// until `boot_end`.
    fn generic(boot_end: i64) -> Session {
        let mut s = session("g");
        s.baseline_at = Some(boot_end);
        s
    }

    #[test]
    fn generic_startup_ends_idle_but_not_unread() {
        let mut s = generic(5000);
        s.observe_generic("banner", 0, 1000);
        s.observe_generic("banner+prompt", 2000, 1000);
        assert_eq!(s.state, State::Working);
        s.observe_generic("banner+prompt", 3500, 1000);
        assert_eq!((s.state, s.unread), (State::Idle, false));
    }

    #[test]
    fn generic_turns_are_working_then_idle_and_unread() {
        let mut s = generic(0);
        s.observe_generic("prompt", 1000, 1000);
        s.state = State::Idle;
        s.observe_generic("answering", 2000, 1000);
        assert_eq!(s.state, State::Working);
        s.observe_generic("answer", 2500, 1000);
        s.observe_generic("answer", 3000, 1000);
        assert_eq!(s.state, State::Working);
        s.observe_generic("answer", 3500, 1000);
        assert_eq!((s.state, s.unread), (State::Idle, true));
    }

    #[test]
    fn generic_activity_seen_while_attached_isnt_unread() {
        let mut s = generic(0);
        s.observe_generic("prompt", 1000, 1000);
        s.state = State::Idle;
        // The user typed while attached; on detach the refresh sees the change...
        s.observe_generic("typed + answer", 9000, 1000);
        // ...and the detach makes it the baseline.
        s.mark_seen(Some("typed + answer".into()), 9000);
        s.observe_generic("typed + answer", 10_500, 1000);
        assert_eq!((s.state, s.unread), (State::Idle, false));
        // Output after detaching is unseen.
        s.observe_generic("more output", 11_000, 1000);
        s.observe_generic("more output", 12_500, 1000);
        assert_eq!((s.state, s.unread), (State::Idle, true));
    }

    #[test]
    fn only_working_sessions_get_corrected() {
        let mut s = session("a");
        s.apply(&event("needs-input", 0, None));
        s.observe_screen("h", 0);
        assert!(!s.correct_stale_working(60_000));
        assert_eq!(s.state, State::NeedsInput);
    }
}
