//! Projects, sessions, reported events, and how events and screen changes move a
//! session between states.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::note::{Note, Noted, Whose};

/// How long a "working" session must go without hook events and without screen
/// changes before it counts as idle. Claude reports no event when a turn is
/// interrupted with Esc; while it really works, its spinner changes the screen
/// several times a second.
pub const QUIET_MS: i64 = 4000;

/// How long a subagent may go unheard of before it's taken for gone: one
/// stopped (killed) may never say it stopped. Past the longest a tool call
/// of Claude's may take (10 minutes, a command's), not to forget one in it.
pub const SUBAGENT_QUIET_MS: i64 = 15 * 60_000;

/// One of a session's subagents at work, as its agent said it started.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Subagent {
    /// What kind it is (Claude's agent type: Explore, general-purpose, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// When it started, and when it was last heard of (ms).
    pub started: i64,
    pub heard: i64,
}

/// A command a session's agent left running in the background (Claude's
/// shells), as its agent said it did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shell {
    /// What it does, as the agent put it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub what: Option<String>,
    /// When it started (ms).
    pub started: i64,
    /// How far into its agent's transcript nothing of its end was written
    /// (bytes), give or take the little before it was reported: as long as
    /// the transcript was then, or as far as it's been read since.
    pub from: u64,
}

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
    pub note: Option<Note>,
}

impl Project {
    /// Its note, as `Roster`'s note functions take it.
    pub fn noted(&self) -> Noted {
        Noted {
            whose: Whose::Project(self.id),
            label: format!("project {}", self.name),
            note: self.note.clone(),
        }
    }

    /// Its defaults for new sessions, e.g. "claude · opus (Opus 5.5) · high": an
    /// alias with what it last turned out to be (`models`); without an agent
    /// of its own, `default_agent`.
    pub fn defaults_label(
        &self,
        default_agent: &str,
        models: &std::collections::HashMap<String, String>,
    ) -> String {
        let agent = self.agent.as_deref().unwrap_or(default_agent);
        let learned = |m: &str| models.get(&crate::agent::learned_key(agent, m));
        let model = self.model.as_ref().map(|m| match learned(m) {
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
    /// What the agent says it's working on (Claude's terminal title, the name
    /// pi's session was given).
    pub title: Option<String>,
    /// Passed to the agent on every start; None means its own default.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub state_since: Option<i64>,
    /// A first prompt to type into the agent once it's ready.
    pub pending_prompt: Option<String>,
    /// When roster last started its agent, and when its agent last said it
    /// waits for a prompt on its conversation (a `session` report), in ms.
    pub started_at: Option<i64>,
    pub session_at: Option<i64>,
    /// The model and effort the agent runs with: what it reported, else
    /// what its start asked for (`Roster::start`).
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
    /// The subagent whose question it needs input for (its agent's id for
    /// it); None while it needs input for its own, or none.
    pub asked_by: Option<String>,
    /// A first prompt its agent was started with on its command line
    /// (`Adapter::takes_prompt`), until the agent reports its first turn:
    /// one it may never have taken (it exited first), kept to give again,
    /// asked for (`Roster::give_prompt_again`), never on its own.
    pub given_prompt: Option<String>,
    /// The subagents its agent started that haven't said they stopped, by
    /// its agent's id for each: for agents that say when one starts
    /// (Claude's SubagentStart and SubagentStop).
    pub subagents: BTreeMap<String, Subagent>,
    /// The commands its agent left running in the background that haven't
    /// ended, by its agent's id for each.
    pub shells: BTreeMap<String, Shell>,
    /// Its agent's own turn ended while subagents or shells it started ran
    /// on: it's working on, till the last ends.
    pub awaiting: bool,
    /// What the user wrote about it (saved apart from the rest, so a save of
    /// the session doesn't overwrite a note saved meanwhile).
    pub note: Option<Note>,
}

/// The kind of event roster records with a title it made for a session's
/// conversation (`Roster::auto_title`), in roster's own terms (`Report`):
/// folded in with the agents' reports, so no save of the session overwrites it.
pub const AUTO_TITLE: &str = "auto-title";

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
    /// A session of `agent` in `project`, in its directory, stopped and with
    /// nothing known about it yet.
    pub fn new(id: &str, project: &Project, name: &str, agent: &str, now: i64) -> Session {
        Session {
            id: id.to_string(),
            project_id: project.id,
            project: project.name.clone(),
            name: name.to_string(),
            agent: agent.to_string(),
            workdir: project.origin.clone(),
            conversation_id: None,
            transcript_path: None,
            state: State::Stopped,
            unread: false,
            exit_status: None,
            last_event_at: None,
            screen_hash: None,
            screen_changed_at: None,
            created_at: now,
            deleted_at: None,
            baseline_at: None,
            title: None,
            model: None,
            effort: None,
            state_since: Some(now),
            pending_prompt: None,
            started_at: None,
            session_at: None,
            running_model: None,
            running_effort: None,
            title_override: None,
            turns: 0,
            worked_ms: 0,
            fork_from: None,
            asked_by: None,
            given_prompt: None,
            subagents: BTreeMap::new(),
            shells: BTreeMap::new(),
            awaiting: false,
            note: None,
        }
    }

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

    /// Its note, as `Roster`'s note functions take it.
    pub fn noted(&self) -> Noted {
        Noted {
            whose: Whose::Session(self.id.clone()),
            label: self.label(),
            note: self.note.clone(),
        }
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
        if state != State::NeedsInput {
            self.asked_by = None;
        }
        // Its agent gone, what it ran went with it.
        if state == State::Stopped {
            self.subagents.clear();
            self.shells.clear();
            self.awaiting = false;
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

    /// The effort to show: what the agent runs with, reported or asked for at
    /// its start. One started on its default runs on it whatever an edit says
    /// meanwhile, unknown until it's reported; stopped, it's what starts next.
    pub fn effort_label(&self) -> Option<String> {
        self.running_effort
            .clone()
            .or_else(|| self.effort.clone().filter(|_| self.state == State::Stopped))
    }

    /// The title: yours if you set one, else the agent's.
    pub fn title(&self) -> Option<&str> {
        self.title_override.as_deref().or(self.title.as_deref())
    }

    /// The title, else the session's name.
    pub fn display_title(&self) -> &str {
        self.title().unwrap_or(&self.name)
    }

    /// Whether its agent has said it waits for a prompt since roster last
    /// started it: what it said in a run before says nothing of this one,
    /// which may be at a screen it shows first.
    pub fn waits_since_started(&self) -> bool {
        let started = self.started_at.unwrap_or(i64::MIN);
        self.session_at.is_some_and(|at| at >= started)
    }

    /// Why it can't be started giving its kept first prompt again
    /// (`Roster::give_prompt_again`): None if it can.
    pub fn prompt_again_refused(&self) -> Option<String> {
        if self.given_prompt.is_none() {
            Some(format!(
                "{} has no first prompt to give again",
                self.label()
            ))
        } else if self.state != State::Stopped {
            Some(format!(
                "{} is running: stop it, then give it its first prompt again",
                self.label()
            ))
        } else {
            None
        }
    }

    /// Applies a reported event, with what its payload says (`report`).
    pub fn apply(&mut self, e: &Event, report: &crate::agent::Report) {
        if self.deleted_at.is_some() {
            return;
        }
        if e.kind == AUTO_TITLE {
            // Unless it's on another conversation by now, or has a title.
            if report.conversation_id.is_some()
                && report.conversation_id == self.conversation_id
                && self.title().is_none()
            {
                self.title = report.title.clone().flatten();
            }
            return;
        }
        // On another conversation than it was.
        let moved_on = report
            .conversation_id
            .as_ref()
            .is_some_and(|id| self.conversation_id.as_ref() != Some(id));
        if let Some(id) = &report.conversation_id {
            self.conversation_id = Some(id.clone());
        }
        if let Some(path) = &report.transcript_path {
            self.transcript_path = Some(path.clone());
        }
        // A cleared conversation is a new task.
        if report.cleared {
            self.title = None;
        }
        // A copy that has moved on to another conversation isn't to be made
        // of the original again.
        if report.cleared || moved_on {
            self.fork_from = None;
        }
        if let Some(model) = &report.model {
            self.running_model = Some(model.clone());
        }
        if let Some(effort) = &report.effort {
            self.running_effort = Some(effort.clone());
        }
        // The agent starting again on the conversation without a name for it
        // (pi, which roster made the title of) leaves the title it has.
        if let Some(title) = &report.title
            && (title.is_some() || moved_on || e.kind != "session")
        {
            self.title = title.clone();
        }
        // Its agent reporting at all is its first turn begun: the prompt it
        // was given, taken.
        if matches!(
            e.kind.as_str(),
            "session" | "prompt" | "working" | "idle" | "needs-input"
        ) {
            self.given_prompt = None;
        }
        // A subagent it said started, heard of again; one whose start
        // wasn't heard (its report lost) at work all the same.
        if let Some(id) = &report.subagent {
            match self.subagents.get_mut(id) {
                Some(subagent) => subagent.heard = subagent.heard.max(e.at),
                None if report.subagents_start_said
                    && matches!(e.kind.as_str(), "working" | "prompt" | "needs-input") =>
                {
                    let subagent = Subagent {
                        kind: report.subagent_kind.clone(),
                        started: e.at,
                        heard: e.at,
                    };
                    self.subagents.insert(id.clone(), subagent);
                }
                None => {}
            }
        }
        // A command it left running in the background, and those it says
        // have ended (told so, or stopping one itself).
        for id in &report.shells_ended {
            self.shells.remove(id);
        }
        if let Some(shell) = &report.shell {
            let left = Shell {
                what: shell.what.clone(),
                started: e.at - shell.ran.max(0),
                from: shell.from,
            };
            self.shells.insert(shell.id.clone(), left);
        }
        match e.kind.as_str() {
            "subagent-start" => {
                if let Some(id) = &report.subagent {
                    self.subagents.insert(
                        id.clone(),
                        Subagent {
                            kind: report.subagent_kind.clone(),
                            started: e.at,
                            heard: e.at,
                        },
                    );
                }
            }
            "subagent-stop" => match &report.subagent {
                Some(id) => self.subagent_stopped(id, e.at),
                None => {
                    self.end_awaited_turn(e.at);
                }
            },
            // A subagent at work, or given its task, is inside the session's
            // turn, or after it (one left running): either way not a turn of
            // its own. Its question is the session's to answer, though, and
            // once it works on, it's answered.
            "working" | "prompt" if report.subagent.is_some() => {
                if self.state == State::NeedsInput && self.asked_by == report.subagent {
                    self.set_state(State::Working, e.at);
                }
            }
            // A prompt you submitted starts a turn: one of its own while it
            // waited on subagents or shells, working as it was. One it's
            // given itself (told that something it left running ended)
            // takes up what was left to read: it's no longer for you till
            // this turn ends.
            "working" | "prompt" => {
                if e.kind == "prompt" && self.awaiting {
                    self.turns += 1;
                }
                self.set_state(State::Working, e.at);
                self.awaiting = false;
                if report.woken {
                    self.unread = false;
                }
            }
            // Its own turn ended: with subagents or shells left running it
            // works on, as it takes their results up when they're done.
            "idle" => {
                self.forget_quiet_subagents(e.at);
                if self.subagents.is_empty() && self.shells.is_empty() {
                    self.set_state(State::Idle, e.at);
                    self.unread = true;
                } else {
                    // A subagent's question still waits on you.
                    if !(self.state == State::NeedsInput && self.asked_by.is_some()) {
                        self.set_state(State::Working, e.at);
                    }
                    self.awaiting = true;
                    self.left_on_shells();
                }
            }
            "needs-input" => {
                // Whose question it is: its own waits for you whatever a
                // subagent asks meanwhile.
                let its_own = self.state == State::NeedsInput && self.asked_by.is_none();
                self.set_state(State::NeedsInput, e.at);
                if !its_own {
                    self.asked_by = report.subagent.clone();
                }
                // Its own question: it's in a turn of its own again.
                if report.subagent.is_none() {
                    self.awaiting = false;
                }
                self.unread = true;
            }
            // A compaction you asked for: work, though not a turn, while it
            // runs, waiting as it was once done; what else it was at (a
            // question, subagents left running) stays.
            "compacting" => {
                if report.subagent.is_none() && self.state == State::Idle {
                    let turns = self.turns;
                    self.set_state(State::Working, e.at);
                    self.turns = turns;
                }
            }
            "compacted" => {
                if report.subagent.is_none() && self.state == State::Working && !self.awaiting {
                    self.set_state(State::Idle, e.at);
                }
            }
            // A compaction, which Claude and codex make mid-turn when the
            // context is full, leaves it as it was.
            "session" if report.compacted => {}
            // The agent (re)started or cleared its conversation and waits
            // for a prompt: what it left running is gone or not its now.
            "session" => {
                self.subagents.clear();
                self.shells.clear();
                self.awaiting = false;
                self.set_state(State::Idle, e.at);
                self.session_at = Some(e.at);
            }
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
    /// becomes what later screens are compared with, not activity; the first
    /// screen seen, as `observe_generic` takes one, still for as long as it's
    /// been seen.
    pub fn redrawn(&mut self, hash: &str, now: i64) {
        self.screen_hash = Some(hash.to_string());
        self.screen_changed_at.get_or_insert(now);
    }

    /// True if the session claims to be working but no hook has reported anything
    /// for a while, so only the screen can tell whether it still is: not
    /// while what it left running works on after its turn, its screen still
    /// then.
    pub fn needs_screen_check(&self, now: i64) -> bool {
        self.state == State::Working
            && !self.awaiting
            && self.last_event_at.is_none_or(|t| now - t >= QUIET_MS)
    }

    /// Its subagent `id` stopped (at `at`): its question goes with it, and
    /// its own turn over with none left, it's idle.
    pub fn subagent_stopped(&mut self, id: &str, at: i64) {
        if self.subagents.remove(id).is_some() {
            self.left_on_shells();
        }
        if self.state == State::NeedsInput && self.asked_by.as_deref() == Some(id) {
            self.set_state(State::Working, at);
        }
        self.end_awaited_turn(at);
    }

    /// Its shells `ended`, each by when, as its agent wrote though no hook
    /// said (mid-turn): its own turn over with nothing left running, it's
    /// idle, from when the last ended, though not before its turn did.
    /// Whether any did.
    pub fn shells_ended(&mut self, ended: &[(String, i64)]) -> bool {
        let gone: Vec<i64> = (ended.iter())
            .filter(|(id, _)| self.shells.remove(id).is_some())
            .map(|(_, at)| *at)
            .collect();
        let Some(last) = gone.into_iter().max() else {
            return false;
        };
        let turn_over = self.last_event_at.max(self.state_since).unwrap_or(last);
        self.end_awaited_turn(last.max(turn_over));
        true
    }

    /// Its agent's transcript read up to `upto` (bytes) with none of the
    /// ends of its shells `ids` in it. Whether that's further than known.
    pub fn shells_read_to(&mut self, upto: u64, ids: &[String]) -> bool {
        let mut further = false;
        for (_, shell) in
            (self.shells.iter_mut()).filter(|(id, shell)| ids.contains(id) && shell.from < upto)
        {
            shell.from = upto;
            further = true;
        }
        further
    }

    /// Its own turn over and its last subagent gone, only shells left
    /// running: it may be waiting for you, a server it started for you
    /// running on, or for them, as it is for its subagents, and which can't
    /// be told. Unread, then, but working.
    fn left_on_shells(&mut self) {
        if self.awaiting && self.subagents.is_empty() && !self.shells.is_empty() {
            self.unread = true;
        }
    }

    /// What it has at work as of `now`, beside its own turn: its subagents
    /// running and its shells.
    pub fn at_work(&self, now: i64) -> usize {
        self.subagents_running(now) + self.shells.len()
    }

    /// Its subagents running as of `now`: those heard of lately.
    pub fn subagents_running(&self, now: i64) -> usize {
        (self.subagents.values())
            .filter(|s| now - s.heard <= SUBAGENT_QUIET_MS)
            .count()
    }

    /// Forgets the subagents not heard of for a while (`SUBAGENT_QUIET_MS`),
    /// a question one of them asked with it, as if it stopped; its own turn
    /// over, with none left, it's idle. Whether anything changed.
    pub fn forget_quiet_subagents(&mut self, now: i64) -> bool {
        let before = self.subagents.len();
        self.subagents
            .retain(|_, s| now - s.heard <= SUBAGENT_QUIET_MS);
        let forgot = self.subagents.len() != before;
        if forgot {
            self.left_on_shells();
        }
        let asker_gone =
            (self.asked_by.as_ref()).is_some_and(|id| !self.subagents.contains_key(id));
        let unasked = self.state == State::NeedsInput && asker_gone;
        if unasked {
            self.set_state(State::Working, now);
        }
        // Waiting on none (forgotten, or kept so by a save that raced
        // another's): it's over.
        let ended = self.end_awaited_turn(now);
        forgot || unasked || ended
    }

    /// Its own turn over and the last of what it left running ended: idle,
    /// their results yours to read (and Claude's, which takes them up in a
    /// turn of its own next, working again). Whether it was.
    fn end_awaited_turn(&mut self, at: i64) -> bool {
        if !self.awaiting || !self.subagents.is_empty() || !self.shells.is_empty() {
            return false;
        }
        self.awaiting = false;
        if self.state == State::Working {
            self.set_state(State::Idle, at);
            self.unread = true;
        }
        true
    }

    /// Turns a stale "working" into idle once the screen has stopped changing.
    /// Doesn't mark it unread: for Claude this only happens after an interrupt,
    /// which the user did while attached. Subagents its agent's interrupt
    /// leaves running (`subagents_outlive`, codex's), and shells, which every
    /// interrupt does, keep it working, its own turn over, as at the end of a
    /// turn they outlive.
    pub fn correct_stale_working(&mut self, now: i64, subagents_outlive: bool) -> bool {
        let still = self.screen_changed_at.is_some_and(|t| now - t >= QUIET_MS);
        if self.needs_screen_check(now) && still {
            // Interrupted (Esc), its subagents said nothing: gone with the
            // turn, or ones left running take theirs up in a turn of their own.
            if !subagents_outlive {
                self.subagents.clear();
            }
            if !self.subagents.is_empty() || !self.shells.is_empty() {
                self.awaiting = true;
                return true;
            }
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

    /// Watched as it goes (attached): what it does counts as seen, its
    /// screen still telling what it's doing.
    pub fn seen_as_it_goes(&mut self, now: i64) {
        self.unread = false;
        self.baseline_at = Some(self.baseline_at.map_or(now, |b| b.max(now)));
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
    use crate::agent::Report;

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

    /// A working Claude session in project 1, "demo", in /tmp.
    pub fn session(id: &str) -> Session {
        let demo = Project {
            id: 1,
            name: "demo".into(),
            origin: "/tmp".into(),
            agent: None,
            model: None,
            effort: None,
            note: None,
        };
        let mut s = Session::new(id, &demo, &format!("s-{id}"), "claude", 0);
        s.state = State::Working;
        s.state_since = None;
        s
    }

    fn event(kind: &str, at: i64) -> Event {
        Event {
            id: 1,
            session_id: "a".into(),
            kind: kind.into(),
            at,
            payload: None,
        }
    }

    /// Applies an event that reported nothing more than its kind.
    fn apply(s: &mut Session, kind: &str, at: i64) {
        s.apply(&event(kind, at), &Report::default());
    }

    #[test]
    fn turn_end_and_questions_mark_unread() {
        let mut s = session("a");
        apply(&mut s, "working", 10);
        assert_eq!((s.state, s.unread), (State::Working, false));
        apply(&mut s, "needs-input", 20);
        assert_eq!((s.state, s.unread), (State::NeedsInput, true));
        s.unread = false;
        apply(&mut s, "working", 30);
        apply(&mut s, "idle", 40);
        assert_eq!(
            (s.state, s.unread, s.last_event_at),
            (State::Idle, true, Some(40))
        );
    }

    #[test]
    fn any_report_of_its_agents_is_its_first_prompt_taken() {
        for kind in ["session", "prompt", "working", "idle", "needs-input"] {
            let mut s = session("a");
            s.given_prompt = Some("fix it".into());
            apply(&mut s, kind, 10);
            assert_eq!(s.given_prompt, None, "{kind}");
        }
        // A title roster made, or a kind it doesn't know, isn't its agent's.
        for kind in [AUTO_TITLE, "unknown"] {
            let mut s = session("a");
            s.given_prompt = Some("fix it".into());
            apply(&mut s, kind, 10);
            assert_eq!(s.given_prompt.as_deref(), Some("fix it"), "{kind}");
        }
    }

    #[test]
    fn its_first_prompt_is_given_again_only_stopped_and_kept() {
        let mut s = session("a");
        s.state = State::Stopped;
        assert!(
            s.prompt_again_refused()
                .unwrap()
                .contains("no first prompt")
        );
        s.given_prompt = Some("fix it".into());
        assert_eq!(s.prompt_again_refused(), None);
        s.state = State::Idle;
        assert!(s.prompt_again_refused().unwrap().contains("is running"));
    }

    #[test]
    fn session_start_is_idle_but_not_unread() {
        let mut s = session("a");
        let report = Report {
            conversation_id: Some("c-1".into()),
            transcript_path: Some("/t/c-1.jsonl".into()),
            ..Report::default()
        };
        s.apply(&event("session", 10), &report);
        assert_eq!((s.state, s.unread), (State::Idle, false));
        assert_eq!(s.conversation_id.as_deref(), Some("c-1"));
        assert_eq!(s.transcript_path.as_deref(), Some("/t/c-1.jsonl"));
    }

    #[test]
    fn only_a_session_reported_since_its_start_says_it_waits() {
        let mut s = session("a");
        assert!(!s.waits_since_started());
        apply(&mut s, "session", 10);
        assert!(s.waits_since_started());
        // Started again: what its last run said isn't this one's.
        s.started_at = Some(20);
        assert!(!s.waits_since_started());
        apply(&mut s, "session", 20);
        assert!(s.waits_since_started());
    }

    #[test]
    fn clear_switches_the_conversation() {
        let mut s = session("a");
        s.conversation_id = Some("c-1".into());
        s.title = Some("old task".into());
        let report = Report {
            conversation_id: Some("c-2".into()),
            cleared: true,
            ..Report::default()
        };
        s.apply(&event("session", 10), &report);
        assert_eq!(s.conversation_id.as_deref(), Some("c-2"));
        assert_eq!(s.title, None);
    }

    #[test]
    fn starting_again_keeps_the_title_a_conversation_has() {
        let mut s = session("a");
        s.agent = "pi".into();
        s.conversation_id = Some("c-1".into());
        s.title = Some("Parser fixes".into());
        // pi again on c-1, which it has no name for: roster's title stays.
        let again = Report {
            conversation_id: Some("c-1".into()),
            title: Some(None),
            ..Report::default()
        };
        s.apply(&event("session", 10), &again);
        assert_eq!(s.title.as_deref(), Some("Parser fixes"));
        // On another (/resume): that one's title, none.
        let other = Report {
            conversation_id: Some("c-2".into()),
            title: Some(None),
            ..Report::default()
        };
        s.apply(&event("session", 20), &other);
        assert_eq!(s.title, None);
        // A name taken away in pi is taken away.
        s.title = Some("Named".into());
        s.apply(
            &event("info", 30),
            &Report {
                title: Some(None),
                ..Report::default()
            },
        );
        assert_eq!(s.title, None);
    }

    #[test]
    fn a_copy_that_moves_on_isnt_forked_again() {
        let mut s = session("a");
        s.conversation_id = Some("copy".into());
        s.fork_from = Some("original".into());
        // The copy starting: still to be made until it has a file.
        let started = Report {
            conversation_id: Some("copy".into()),
            ..Report::default()
        };
        s.apply(&event("session", 10), &started);
        assert_eq!(s.fork_from.as_deref(), Some("original"));
        let fresh = Report {
            conversation_id: Some("new".into()),
            cleared: true,
            ..Report::default()
        };
        s.apply(&event("session", 20), &fresh);
        assert_eq!(s.fork_from, None);
    }

    #[test]
    fn a_made_title_is_for_its_conversation_only() {
        let made = |conversation: &str| Report {
            conversation_id: Some(conversation.into()),
            title: Some(Some("Parser fixes".into())),
            ..Report::default()
        };
        let mut s = session("a");
        s.conversation_id = Some("c-2".into());
        s.apply(&event(AUTO_TITLE, 10), &made("c-1"));
        assert_eq!(s.title, None);
        s.apply(&event(AUTO_TITLE, 10), &made("c-2"));
        assert_eq!(s.title.as_deref(), Some("Parser fixes"));
        assert_eq!(s.last_event_at, None);
        // One it got meanwhile stays.
        s.title = Some("Named".into());
        s.apply(&event(AUTO_TITLE, 20), &made("c-2"));
        assert_eq!(s.title.as_deref(), Some("Named"));
    }

    #[test]
    fn remembers_the_model_and_effort_reported() {
        let mut s = session("a");
        s.model = Some("opus".into());
        assert_eq!(s.model_label().as_deref(), Some("opus"));
        let model = Report {
            model: Some("claude-opus-5-5[1m]".into()),
            ..Report::default()
        };
        s.apply(&event("session", 10), &model);
        let effort = Report {
            effort: Some("xhigh".into()),
            ..Report::default()
        };
        s.apply(&event("idle", 20), &effort);
        assert_eq!(s.model_label().as_deref(), Some("Opus 5.5 (1M)"));
        assert_eq!(s.effort_label().as_deref(), Some("xhigh"));
        // Edited, it runs on as it was until its next start.
        s.model = Some("haiku".into());
        s.effort = Some("low".into());
        assert_eq!(s.model_label().as_deref(), Some("Opus 5.5 (1M)"));
        assert_eq!(s.effort_label().as_deref(), Some("xhigh"));
    }

    #[test]
    fn an_effort_edited_shows_from_the_next_start() {
        // Started on its agent's default, which it doesn't report (codex).
        let mut s = session("a");
        s.state = State::Idle;
        s.effort = Some("low".into());
        assert_eq!(s.effort_label(), None);
        s.state = State::Stopped;
        assert_eq!(s.effort_label().as_deref(), Some("low"));
        // Started with it.
        s.running_effort = Some("low".into());
        s.state = State::Working;
        assert_eq!(s.effort_label().as_deref(), Some("low"));
    }

    #[test]
    fn counts_turns_and_time_worked() {
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        apply(&mut s, "working", 1_000);
        apply(&mut s, "working", 2_000);
        apply(&mut s, "needs-input", 4_000);
        apply(&mut s, "working", 9_000);
        apply(&mut s, "idle", 10_000);
        assert_eq!((s.turns, s.worked_ms), (1, 4_000));
        apply(&mut s, "working", 20_000);
        assert_eq!((s.turns, s.worked(25_000)), (2, 9_000));
    }

    #[test]
    fn a_compaction_you_asked_for_works_till_its_done() {
        let subagent = |id: &str| Report {
            subagent: Some(id.into()),
            ..Report::default()
        };
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        // /compact: no prompt, but at work, though no turn; then waiting
        // again, nothing new to read.
        apply(&mut s, "compacting", 1_000);
        assert_eq!((s.state, s.turns), (State::Working, 0));
        apply(&mut s, "compacted", 5_000);
        assert_eq!((s.state, s.unread, s.turns), (State::Idle, false, 0));
        // With subagents left running: working on till they're done.
        apply(&mut s, "prompt", 6_000);
        s.apply(&event("subagent-start", 6_100), &subagent("b1"));
        apply(&mut s, "idle", 7_000);
        apply(&mut s, "compacting", 8_000);
        apply(&mut s, "compacted", 9_000);
        assert_eq!((s.state, s.awaiting), (State::Working, true));
        // A question of one of them stays.
        s.apply(&event("needs-input", 9_500), &subagent("b1"));
        apply(&mut s, "compacting", 10_000);
        apply(&mut s, "compacted", 11_000);
        assert_eq!(s.state, State::NeedsInput);
        s.apply(&event("subagent-stop", 12_000), &subagent("b1"));
        assert_eq!((s.state, s.unread), (State::Idle, true));
        // A subagent's isn't the session's.
        s.apply(&event("compacting", 13_000), &subagent("b2"));
        assert_eq!(s.state, State::Idle);
        // One that failed (no end said): the still screen ends it.
        apply(&mut s, "compacting", 14_000);
        s.observe_screen("still", 14_000);
        assert!(s.correct_stale_working(14_000 + QUIET_MS, false));
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn a_compaction_mid_turn_leaves_it_working_in_the_same_turn() {
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        apply(&mut s, "working", 1_000);
        let compacted = Report {
            compacted: true,
            ..Report::default()
        };
        s.apply(&event("session", 2_000), &compacted);
        assert_eq!(s.state, State::Working);
        apply(&mut s, "working", 3_000);
        apply(&mut s, "idle", 4_000);
        assert_eq!((s.turns, s.worked_ms), (1, 3_000));
        // A start is still one: waiting for a prompt.
        apply(&mut s, "working", 5_000);
        s.apply(&event("session", 6_000), &Report::default());
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn a_subagent_neither_starts_nor_ends_a_turn_but_its_question_is_asked() {
        let subagent = |id: &str| Report {
            subagent: Some(id.into()),
            ..Report::default()
        };
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        s.transcript_path = Some("/s/root.jsonl".into());
        // One left running after the turn: still idle, no turn more.
        s.apply(&event("working", 1_000), &subagent("a1"));
        s.apply(&event("prompt", 1_500), &subagent("a1"));
        assert_eq!((s.state, s.turns), (State::Idle, 0));
        // Inside a turn, its question waits on you; another subagent at work
        // doesn't answer it, its own working on does.
        apply(&mut s, "working", 2_000);
        s.apply(&event("needs-input", 3_000), &subagent("a1"));
        assert_eq!(s.state, State::NeedsInput);
        assert_eq!(s.transcript_path.as_deref(), Some("/s/root.jsonl"));
        s.apply(&event("working", 3_500), &subagent("a2"));
        assert_eq!(s.state, State::NeedsInput);
        s.apply(&event("working", 4_000), &subagent("a1"));
        assert_eq!((s.state, s.asked_by.as_deref()), (State::Working, None));
        // The session's own question no subagent answers, nor takes over.
        apply(&mut s, "needs-input", 5_000);
        s.apply(&event("needs-input", 5_500), &subagent("a1"));
        s.apply(&event("working", 6_000), &subagent("a1"));
        assert_eq!((s.state, s.asked_by.as_deref()), (State::NeedsInput, None));
        apply(&mut s, "working", 7_000);
        assert_eq!(s.state, State::Working);
        assert_eq!(s.turns, 1);
    }

    #[test]
    fn subagents_left_running_after_its_turn_keep_it_working() {
        let subagent = |id: &str| Report {
            subagent: Some(id.into()),
            ..Report::default()
        };
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        apply(&mut s, "prompt", 1_000);
        // One in the background, one it waits on within its turn.
        s.apply(&event("subagent-start", 2_000), &subagent("b1"));
        s.apply(&event("subagent-start", 2_500), &subagent("f1"));
        assert_eq!(s.subagents_running(3_000), 2);
        s.apply(&event("subagent-stop", 3_000), &subagent("f1"));
        assert_eq!((s.state, s.awaiting), (State::Working, false));
        // Its turn ends with one still at work: working on, no turn more,
        // nothing to read yet, its screen not asked.
        apply(&mut s, "idle", 4_000);
        assert_eq!(
            (s.state, s.awaiting, s.unread, s.turns),
            (State::Working, true, false, 1)
        );
        assert!(!s.needs_screen_check(60_000));
        // Heard of at work, then stopped: idle, to read.
        s.apply(&event("working", 50_000), &subagent("b1"));
        assert_eq!(s.subagents["b1"].heard, 50_000);
        s.apply(&event("subagent-stop", 60_000), &subagent("b1"));
        assert_eq!((s.state, s.awaiting, s.unread), (State::Idle, false, true));
        assert_eq!(s.worked_ms, 59_000);
        // One that never says it stopped is taken for gone after a while.
        apply(&mut s, "prompt", 70_000);
        s.apply(&event("subagent-start", 71_000), &subagent("b2"));
        apply(&mut s, "idle", 72_000);
        assert!(!s.forget_quiet_subagents(71_000 + SUBAGENT_QUIET_MS));
        assert_eq!(s.state, State::Working);
        assert_eq!(s.subagents_running(71_001 + SUBAGENT_QUIET_MS), 0);
        assert!(s.forget_quiet_subagents(71_001 + SUBAGENT_QUIET_MS));
        assert_eq!((s.state, s.unread), (State::Idle, true));
        // Taken up again while one works on: its own turn, then waiting again.
        s.apply(&event("subagent-start", 200_000), &subagent("b3"));
        apply(&mut s, "idle", 201_000);
        assert!(s.awaiting);
        apply(&mut s, "prompt", 202_000);
        assert!(!s.awaiting);
        apply(&mut s, "idle", 203_000);
        assert!(s.awaiting);
        // Its own question: a turn of its own again.
        apply(&mut s, "needs-input", 204_000);
        assert!(!s.awaiting);
        // Restarted, or stopped: what it ran is gone.
        apply(&mut s, "session", 205_000);
        assert!(s.subagents.is_empty());
        s.apply(&event("subagent-start", 206_000), &subagent("b4"));
        s.set_state(State::Stopped, 207_000);
        assert!(s.subagents.is_empty());
        // What kind each is, and when it started, kept.
        let explore = Report {
            subagent: Some("k1".into()),
            subagent_kind: Some("Explore".into()),
            ..Report::default()
        };
        s.apply(&event("subagent-start", 208_000), &explore);
        assert_eq!(
            s.subagents["k1"],
            Subagent {
                kind: Some("Explore".into()),
                started: 208_000,
                heard: 208_000,
            }
        );
        // One whose start's report was lost, from an agent that says
        // subagents start: at work once it's heard of.
        let lost = Report {
            subagent: Some("l1".into()),
            subagents_start_said: true,
            ..Report::default()
        };
        apply(&mut s, "session", 209_000);
        apply(&mut s, "prompt", 209_500);
        s.apply(&event("working", 210_000), &lost);
        assert_eq!(s.subagents["l1"].started, 210_000);
        s.apply(&event("subagent-stop", 211_000), &lost);
        assert!(s.subagents.is_empty());
        // Subagents it never said started (codex's, as roster takes its
        // hooks) aren't waited for.
        let mut c = session("c");
        apply(&mut c, "prompt", 1_000);
        c.apply(&event("working", 2_000), &subagent("x"));
        apply(&mut c, "idle", 3_000);
        assert_eq!((c.state, c.subagents_running(3_000)), (State::Idle, 0));
    }

    #[test]
    fn shells_left_running_after_its_turn_keep_it_working_but_unread() {
        use crate::agent::Backgrounded;
        let left = |id: &str| Report {
            shell: Some(Backgrounded {
                id: id.into(),
                what: Some("Run the tests".into()),
                ran: 0,
                from: 500,
            }),
            ..Report::default()
        };
        let told = |ids: &[&str]| Report {
            shells_ended: ids.iter().map(|id| id.to_string()).collect(),
            woken: true,
            ..Report::default()
        };
        let mut s = session("a");
        s.state = State::Idle;
        s.state_since = Some(0);
        apply(&mut s, "prompt", 1_000);
        s.apply(&event("working", 2_000), &left("b1"));
        let shell = Shell {
            what: Some("Run the tests".into()),
            started: 2_000,
            from: 500,
        };
        assert_eq!(s.shells["b1"], shell);
        assert_eq!(s.at_work(2_000), 1);
        // Its turn ends with it running: working on, its screen not asked,
        // and unread, as it may be waiting for you as well.
        apply(&mut s, "idle", 3_000);
        assert_eq!(
            (s.state, s.awaiting, s.unread, s.turns),
            (State::Working, true, true, 1)
        );
        assert!(!s.needs_screen_check(60_000));
        // Told it ended, it goes on by itself: no longer yours to read.
        s.apply(&event("prompt", 50_000), &told(&["b1"]));
        assert!(s.shells.is_empty());
        assert_eq!(
            (s.state, s.awaiting, s.unread, s.turns),
            (State::Working, false, false, 2)
        );
        apply(&mut s, "idle", 51_000);
        assert_eq!((s.state, s.unread), (State::Idle, true));

        // Read while it runs: read till its next turn ends, whatever settles.
        apply(&mut s, "prompt", 60_000);
        s.apply(&event("working", 61_000), &left("b2"));
        apply(&mut s, "idle", 62_000);
        s.unread = false;
        assert!(!s.shells_ended(&[("b9".into(), 63_000)]));
        assert!(!s.forget_quiet_subagents(63_000));
        assert!(!s.unread);
        // Its end told while it waited, which only its transcript says (its
        // turn's report lost): idle, to read, from when it ended.
        assert!(s.shells_ended(&[("b2".into(), 64_000)]));
        assert_eq!(
            (s.state, s.awaiting, s.unread, s.state_since),
            (State::Idle, false, true, Some(64_000))
        );

        // Ended mid-turn: its turn's end is the end.
        s.unread = false;
        apply(&mut s, "prompt", 65_000);
        s.apply(&event("working", 66_000), &left("b3"));
        assert!(s.shells_ended(&[("b3".into(), 67_000)]));
        assert_eq!((s.state, s.unread), (State::Working, false));
        apply(&mut s, "idle", 68_000);
        assert_eq!((s.state, s.unread), (State::Idle, true));
        // Caught only after its turn ended (no roster running meanwhile):
        // idle from its turn's end, not from when it was caught.
        s.unread = false;
        apply(&mut s, "prompt", 69_000);
        s.apply(&event("working", 69_500), &left("b4"));
        apply(&mut s, "idle", 70_000);
        assert!(s.shells_ended(&[("b4".into(), 69_800)]));
        assert_eq!((s.state, s.state_since), (State::Idle, Some(70_000)));

        // With subagents too, it waits on them: not unread till only shells
        // are left.
        let subagent = Report {
            subagent: Some("f1".into()),
            ..Report::default()
        };
        s.unread = false;
        apply(&mut s, "prompt", 70_000);
        s.apply(&event("working", 71_000), &left("b5"));
        s.apply(&event("subagent-start", 72_000), &subagent);
        apply(&mut s, "idle", 73_000);
        assert_eq!(
            (s.state, s.awaiting, s.unread),
            (State::Working, true, false)
        );
        s.apply(&event("subagent-stop", 74_000), &subagent);
        assert_eq!(
            (s.state, s.awaiting, s.unread),
            (State::Working, true, true)
        );
        // How far its transcript's been read with none of their ends in it.
        let read = ["b5".to_string()];
        assert!(s.shells_read_to(9_000, &read));
        assert!(!s.shells_read_to(8_000, &read));
        assert!(!s.shells_read_to(10_000, &["b0".to_string()]));
        assert_eq!(s.shells["b5"].from, 9_000);
        // One it stopped itself is gone, as are all once its agent is.
        let stopped = Report {
            shells_ended: vec!["b5".into()],
            ..Report::default()
        };
        s.apply(&event("working", 75_000), &stopped);
        assert!(s.shells.is_empty());
        s.apply(&event("working", 76_000), &left("b6"));
        apply(&mut s, "session", 77_000);
        assert!(s.shells.is_empty());
        s.apply(&event("working", 78_000), &left("b7"));
        s.set_state(State::Stopped, 79_000);
        assert!(s.shells.is_empty());

        // Put there after running a while: it started then.
        let mut ran = left("b8");
        ran.shell.as_mut().unwrap().ran = 5_000;
        s.apply(&event("working", 90_000), &ran);
        assert_eq!(s.shells["b8"].started, 85_000);
    }

    #[test]
    fn an_interrupt_leaves_its_shells_running_and_it_working_on_them() {
        use crate::agent::Backgrounded;
        let mut s = session("a");
        s.apply(
            &event("working", 1_000),
            &Report {
                shell: Some(Backgrounded {
                    id: "b1".into(),
                    ..Backgrounded::default()
                }),
                ..Report::default()
            },
        );
        s.subagents.insert("f1".into(), Subagent::default());
        s.observe_screen("still", 1_000);
        // Esc: no hook, its screen still. Claude's subagents end with its
        // turn; its shells run on, and it takes their results up itself.
        assert!(s.correct_stale_working(1_000 + QUIET_MS, false));
        assert_eq!(
            (s.state, s.awaiting, s.unread),
            (State::Working, true, false)
        );
        assert!(s.subagents.is_empty());
        assert!(!s.needs_screen_check(1_000 + 2 * QUIET_MS));
    }

    #[test]
    fn what_subagents_ask_and_leave_is_settled() {
        let subagent = |id: &str| Report {
            subagent: Some(id.into()),
            ..Report::default()
        };
        let mut s = session("a");
        s.state = State::Idle;
        // Its only subagent stopping within its turn doesn't end it.
        apply(&mut s, "prompt", 1_000);
        s.apply(&event("subagent-start", 1_100), &subagent("f1"));
        s.apply(&event("subagent-stop", 1_200), &subagent("f1"));
        assert_eq!((s.state, s.unread), (State::Working, false));
        // Two left running: one stopping leaves it waiting on the other.
        s.apply(&event("subagent-start", 1_300), &subagent("b1"));
        s.apply(&event("subagent-start", 1_400), &subagent("b2"));
        apply(&mut s, "idle", 2_000);
        s.apply(&event("subagent-stop", 3_000), &subagent("b1"));
        assert_eq!((s.state, s.awaiting), (State::Working, true));
        // A prompt meanwhile is a turn of its own.
        apply(&mut s, "prompt", 3_500);
        assert_eq!(s.turns, 2);
        apply(&mut s, "idle", 3_600);
        // b2 asks: its own turn ending meanwhile leaves the question.
        s.apply(&event("needs-input", 4_000), &subagent("b2"));
        assert!(s.awaiting);
        apply(&mut s, "idle", 4_500);
        assert_eq!(
            (s.state, s.asked_by.as_deref()),
            (State::NeedsInput, Some("b2"))
        );
        // b2 stops (denied): its question goes, and nothing's left to wait on.
        s.apply(&event("subagent-stop", 5_000), &subagent("b2"));
        assert_eq!((s.state, s.unread, s.awaiting), (State::Idle, true, false));
        // Interrupted with one at work (Esc: no hooks): the screen ends the
        // turn, and its subagent with it, so its next turn ends as it should.
        apply(&mut s, "prompt", 6_000);
        s.apply(&event("subagent-start", 6_100), &subagent("f2"));
        s.observe_screen("still", 6_200);
        let mut outliving = s.clone();
        assert!(s.correct_stale_working(6_200 + QUIET_MS, false));
        assert!(s.subagents.is_empty());
        // Codex's run on: working on, its own turn over, till it stops.
        assert!(outliving.correct_stale_working(6_200 + QUIET_MS, true));
        assert_eq!(
            (outliving.state, outliving.awaiting),
            (State::Working, true)
        );
        assert!(!outliving.needs_screen_check(6_200 + 2 * QUIET_MS));
        outliving.apply(&event("subagent-stop", 30_000), &subagent("f2"));
        assert_eq!((outliving.state, outliving.unread), (State::Idle, true));
        apply(&mut s, "prompt", 20_000);
        apply(&mut s, "idle", 21_000);
        assert_eq!((s.state, s.unread), (State::Idle, true));
        // b3 asks, then is heard of no more: its question goes with it.
        apply(&mut s, "prompt", 21_100);
        s.apply(&event("subagent-start", 21_200), &subagent("b3"));
        apply(&mut s, "idle", 21_300);
        s.apply(&event("needs-input", 21_400), &subagent("b3"));
        assert!(s.forget_quiet_subagents(21_400 + SUBAGENT_QUIET_MS + 1));
        assert_eq!(
            (s.state, s.asked_by.as_deref(), s.awaiting),
            (State::Idle, None, false)
        );
        // Kept waiting on none (a save that raced another's): settled.
        s.state = State::Working;
        s.awaiting = true;
        assert!(s.forget_quiet_subagents(22_000));
        assert_eq!((s.state, s.awaiting), (State::Idle, false));
        assert!(!s.forget_quiet_subagents(23_000));
        // Asked by one gone already, none forgotten now: its question goes,
        // and that's a change, to save.
        s.state = State::NeedsInput;
        s.asked_by = Some("b4".into());
        assert!(s.forget_quiet_subagents(24_000));
        assert_eq!(s.state, State::Working);
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
        apply(&mut s, "bogus", 10);
        assert_eq!((s.state, s.last_event_at), (State::Working, None));
        s.deleted_at = Some(1);
        apply(&mut s, "idle", 20);
        assert_eq!(s.state, State::Working);
    }

    #[test]
    fn stale_working_becomes_idle_only_when_screen_is_still() {
        let mut s = session("a");
        apply(&mut s, "working", 0);
        s.observe_screen("h1", 1000);
        // Hook event too recent.
        assert!(!s.correct_stale_working(3000, false));
        // Screen still changing.
        s.observe_screen("h2", 4500);
        assert!(!s.correct_stale_working(6000, false));
        // Unchanged for QUIET_MS.
        s.observe_screen("h2", 8000);
        assert!(s.correct_stale_working(4500 + QUIET_MS, false));
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
    fn generic_startup_redrawn_for_a_new_size_ends_idle() {
        // Its first screens seen only as redraws (resized as it started).
        let mut s = generic(0);
        s.redrawn("banner", 1000);
        s.redrawn("banner+prompt", 1500);
        s.observe_generic("banner+prompt", 2500, 1000);
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
        apply(&mut s, "needs-input", 0);
        s.observe_screen("h", 0);
        assert!(!s.correct_stale_working(60_000, false));
        assert_eq!(s.state, State::NeedsInput);
    }
}
