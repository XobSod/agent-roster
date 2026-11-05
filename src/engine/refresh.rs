//! The refresh, session by session: what's seen of it (`observe`), what
//! that makes it at the time (`decide`, which changes nothing and reads only
//! a stale turn's transcript), saved (`save_decided`); then what's to be
//! done (`act`), once every one is.

use super::*;
use crate::agent::Backgrounded;

impl Roster {
    /// Brings the database up to date: applies reported events, notices agents
    /// that have exited, follows agents' titles and screens (correcting stale
    /// "working" states, watching agents without hooks), and types first
    /// prompts into agents that are ready for them.
    pub fn refresh(&mut self) -> Result<()> {
        let now = now_ms();
        self.fold_reports(now)?;
        let panes = self.tmux.panes()?;
        self.resized.retain(|_, until| *until > now);
        let sessions = self.db.sessions(false)?;
        // Another roster's too: the one that closed the list beside an agent
        // may have saved its new size before the agent redrew for it. Read
        // after the sessions: a roster saves the grace before the new size,
        // so a new size read comes with its grace.
        for (id, until) in self.db.redraw_graces(now).unwrap_or_default() {
            let grace = self.resized.entry(id).or_insert(until);
            *grace = (*grace).max(until);
        }
        // Typed once every session is saved, each claimed just before it's
        // typed: a refresh that stops before then takes none of them.
        let mut due = Due::default();
        let queued = self.db.queued_sessions()?;
        let mut transcripts = OnDisk {
            db: self.paths.db(),
        };
        let mut wait_until = None;
        for before in sessions {
            let pane = panes.get(&before.tmux_name());
            let queued = queued.contains(&before.id);
            let seen = self.observe(&before, pane, queued, now, &mut wait_until);
            let watching = self.watching.as_deref();
            let (s, ready) = decide(before.clone(), seen, watching, now, &mut transcripts);
            if self.save_decided(&before, &s)? {
                due.titles.push(s.id.clone());
            }
            match ready {
                Some(Ready::FirstPrompt) => due.prompts.push((s.id.clone(), s.tmux_name())),
                Some(Ready::Queued) => due.sends.push(s),
                None => {}
            }
        }
        self.panes = Some(panes);
        self.act(due)
    }

    /// Applies the events reported since (`Db::fold_events`), then settles
    /// the subagents and shells agents started, whose ends no report may say.
    fn fold_reports(&mut self, now: i64) -> Result<()> {
        self.db.fold_events(now)?;
        // A subagent its agent says is finished is, and one unheard of for
        // long is taken for gone.
        self.db.settle_subagents(now, &|s, id, heard| {
            (s.transcript_path.as_deref()).is_some_and(|path| {
                agent::adapter(&s.agent).subagent_finished(Path::new(path), id, heard)
            })
        })?;
        // A shell whose end its agent was told mid-turn, which no hook says.
        let read = &mut self.shells_read;
        let mut with_shells = HashSet::new();
        self.db.settle_shells(&mut |s| {
            with_shells.insert(s.id.clone());
            shells_ended(read, s, now)
        })?;
        read.retain(|id, _| with_shells.contains(id));
        Ok(())
    }

    /// What's seen at `now` of session `s` as read, in `pane` as listed
    /// (None: its tmux session is gone), with prompts `queued` for it or
    /// not: only what `decide` goes by, so a dead pane's exit is waited for
    /// only if it's news, and a screen captured only where it's looked at.
    /// Nothing changes but what seeing a screen notes (`screen_hash`: the
    /// size it was seen at, and a redraw grace for a new one).
    fn observe(
        &mut self,
        s: &Session,
        pane: Option<&Pane>,
        queued: bool,
        now: i64,
        wait_until: &mut Option<Instant>,
    ) -> Seen {
        let pane = match pane {
            None => PaneSeen::Gone,
            Some(pane) if pane.dead => {
                // Its exit asked only as it stops, the only time `decide`
                // records it. Started again meanwhile, it's the next
                // refresh's.
                let pane = if s.state == State::Stopped {
                    None
                } else if self.ending.contains(&s.id) {
                    Some(pane.clone())
                } else {
                    self.exited(&s.tmux_name(), pane, wait_until)
                };
                PaneSeen::Dead(pane)
            }
            Some(pane) => {
                let adapter = agent::adapter(&s.agent);
                let title = adapter.title(&pane.title, s.conversation_id.as_deref());
                // Captured where `decide` looks at it: for an agent with
                // hooks, only as they've gone quiet on a turn, which the
                // session as read tells (one found running, stopped, is
                // idle, not at work).
                let looked_at = !adapter.reports() || s.needs_screen_check(now);
                let screen = if looked_at {
                    self.screen_hash(s, now)
                } else {
                    None
                };
                let screen = screen.map(|hash| Screen {
                    redrawing: self.resized.contains_key(&s.id),
                    hash,
                });
                let idle_after = (!adapter.reports() && screen.is_some())
                    .then(|| agent::idle_after_ms(&s.agent, &self.config));
                PaneSeen::Live(Live {
                    clients: pane.clients,
                    beside: pane.beside,
                    title,
                    screen,
                    idle_after,
                })
            }
        };
        Seen { pane, queued }
    }

    /// Saves session `s` as `decide` made it of `before`, its automatic
    /// title claimed first if it's due one: whether it was, for `act` to
    /// make. Claimed here, not in `decide`: whether it's due takes its
    /// transcript being there (`wants_auto_title`).
    fn save_decided(&self, before: &Session, s: &Session) -> Result<bool> {
        let mut claimed = false;
        if self.watching.is_none()
            && self.wants_auto_title(s)
            && let Some(conversation) = &s.conversation_id
            && self.db.claim_auto_title(&s.id, conversation)?
        {
            claimed = true;
        }
        if s != before {
            // Only what changed: what another roster saved meanwhile stays,
            // and a state it saved since this one read it.
            self.db.save_observed(before, s)?;
        }
        Ok(claimed)
    }

    /// Does what's `due` once every session is saved: makes the titles
    /// claimed, types first prompts in, then what's queued.
    fn act(&mut self, due: Due) -> Result<()> {
        for id in due.titles {
            self.auto_title_in_background(&id);
        }
        for (id, name) in due.prompts {
            if self
                .prompt_tries
                .get(&id)
                .is_some_and(|&n| n >= PROMPT_TRIES)
            {
                continue;
            }
            // Claimed in the database first, with the turn it begins, so only
            // one roster types it, and nothing else goes into that turn.
            let begin = |s: &mut Session| self.begin_turn(s, now_ms());
            let Some(turn) = self.db.take_pending_prompt(&id, begin)? else {
                continue;
            };
            if let Err(untyped) = self.tmux.type_line(&name, &turn.prompt) {
                self.untyped(&id, &turn, untyped);
            }
        }
        for s in due.sends {
            self.type_queued(&s)?;
        }
        Ok(())
    }
}

/// What a refresh does once every session is saved (`Roster::act`).
#[derive(Default)]
struct Due {
    /// The sessions whose automatic title it claimed, to be made.
    titles: Vec<String>,
    /// Those ready for their first prompt, by id and tmux session.
    prompts: Vec<(String, String)>,
    /// Those ready for the first of what's queued for them.
    sends: Vec<Session>,
}

/// What a refresh knows of a session as it read it (`Roster::observe`):
/// all `decide` goes by, but for what it reads of its transcript.
struct Seen {
    pane: PaneSeen,
    /// Prompts are queued for it (`Db::queued_sessions`).
    queued: bool,
}

/// A session's pane, as a refresh saw it.
enum PaneSeen {
    /// None listed: its tmux session is gone.
    Gone,
    /// Dead: as tmux has it once its exit is known (`Roster::exited`). None
    /// if it's running again meanwhile, or wasn't asked, the session
    /// stopped already.
    Dead(Option<Pane>),
    Live(Live),
}

/// A live pane, as a refresh saw it.
struct Live {
    /// Clients attached to its session, and whether the list is beside it
    /// (`Pane`).
    clients: u32,
    beside: bool,
    /// What its agent says it's working on, from its terminal title or its
    /// own record of it (`Adapter::title`).
    title: Option<String>,
    /// Its screen, where it's looked at: always for an agent without hooks,
    /// for one with them only as they've gone quiet on a turn
    /// (`Session::needs_screen_check`).
    screen: Option<Screen>,
    /// How long its agent shows a still screen before it's idle
    /// (`agent::idle_after_ms`): for one without hooks, its screen
    /// captured, the only one it's gone by.
    idle_after: Option<i64>,
}

/// A screen as captured (`Roster::screen_hash`).
struct Screen {
    hash: String,
    /// In a redraw grace (`Roster::resized`): its agent redrawing for a new
    /// size, not at work.
    redrawing: bool,
}

/// What `decide` finds a session ready for, typed in once every session is
/// saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ready {
    FirstPrompt,
    /// The first of what's queued for it.
    Queued,
}

/// What `decide` reads of a session's transcript: only where a turn its
/// hooks went quiet on looks over, its screen still, which isn't known
/// till the screen is compared. Read with what's seen (`observe`), it would
/// be read every refresh.
trait Transcripts {
    /// What the session's last turn left running (`Adapter::shells_left`).
    fn shells_left(&mut self, s: &Session, path: &Path) -> Vec<Backgrounded>;
    /// When its last turn ended (`Adapter::turn_ended`).
    fn turn_ended(&mut self, s: &Session, path: &Path) -> Option<i64>;
}

/// Transcripts as their agents read them, what's running looked up with
/// the database at `db` (`procs::running`).
struct OnDisk {
    db: PathBuf,
}

impl Transcripts for OnDisk {
    fn shells_left(&mut self, s: &Session, path: &Path) -> Vec<Backgrounded> {
        agent::adapter(&s.agent).shells_left(&s.id, &self.db, path)
    }

    fn turn_ended(&mut self, s: &Session, path: &Path) -> Option<i64> {
        agent::adapter(&s.agent).turn_ended(path)
    }
}

/// Session `s` as it should be at `now`, by what's `seen` of it, this
/// roster only watching session `watching` (`Roster::watching`) or not;
/// and what it's ready for. Nothing's changed, and nothing read but
/// through `transcripts`, as a stale turn comes to need it.
fn decide(
    mut s: Session,
    seen: Seen,
    watching: Option<&str>,
    now: i64,
    transcripts: &mut impl Transcripts,
) -> (Session, Option<Ready>) {
    let mut ready = None;
    match seen.pane {
        PaneSeen::Gone => s.set_state(State::Stopped, now),
        PaneSeen::Dead(pane) => {
            // Record the exit code when it stops, not on every pass, so
            // `stop` can clear it; killed by a signal, 128 + it, as a
            // shell says.
            if s.state != State::Stopped
                && let Some(pane) = pane
            {
                let status = (pane.dead_status).or(pane.dead_signal.map(|signal| 128 + signal));
                // When it died, not when it was seen to have (nothing
                // is seen while you're attached, or roster's closed),
                // though not before the state it ended.
                let at = match pane.dead_at.filter(|&at| at > 0) {
                    Some(at) => at.min(now).max(s.state_since.unwrap_or(at)),
                    None => now,
                };
                s.set_state(State::Stopped, at);
                s.exit_status = status;
            }
        }
        PaneSeen::Live(pane) => {
            // Running although recorded as stopped: idle until hooks say otherwise.
            if s.state == State::Stopped {
                s.set_state(State::Idle, now);
            }
            if let Some(title) = pane.title {
                s.title = Some(title);
            }
            let adapter = agent::adapter(&s.agent);
            if adapter.reports() {
                // Its hooks tell; the screen only corrects a missed end of turn.
                if s.needs_screen_check(now)
                    && let Some(screen) = pane.screen
                {
                    if screen.redrawing {
                        s.redrawn(&screen.hash, now);
                    } else {
                        s.observe_screen(&screen.hash, now);
                        let since = s.state_since;
                        let transcript = s.transcript_path.clone();
                        let transcript = transcript.as_deref().map(Path::new);
                        // What the turn left running, which an end no
                        // hook reports (a review's, a failed turn's)
                        // doesn't say.
                        if s.stale(now)
                            && let Some(path) = transcript
                        {
                            s.add_shells(&transcripts.shells_left(&s, path), now);
                        }
                        // Its transcript saying the turn ended since,
                        // not interrupted: there's its answer to read,
                        // left with only shells running too, as when
                        // its end is reported. Subagents still at work
                        // put that off, the mark theirs to give.
                        let corrected =
                            s.correct_stale_working(now, adapter.subagents_outlive_interrupts());
                        if corrected
                            && s.subagents.is_empty()
                            && let Some(path) = transcript
                            && let Some(ended) = transcripts.turn_ended(&s, path)
                            && since.is_none_or(|since| ended >= since)
                        {
                            s.turn_over_unread();
                        }
                        // Its turn over, unheard: a prompt sent it is
                        // of no turn after.
                        if corrected && s.state == State::Idle {
                            s.sent = None;
                        }
                    }
                }
            } else if let Some(screen) = pane.screen {
                if screen.redrawing {
                    s.redrawn(&screen.hash, now);
                } else if let Some(idle_after) = pane.idle_after {
                    s.observe_generic(&screen.hash, now, idle_after);
                }
            }
            if watching.is_none() && s.pending_prompt.is_some() && ready_for_prompt(&s, now) {
                ready = Some(Ready::FirstPrompt);
            }
            // What's sent to it, once it's ready as for a first prompt,
            // that gone, and no one attached: what's typed then is
            // theirs.
            if watching.is_none()
                && s.pending_prompt.is_none()
                && seen.queued
                && pane.clients == 0
                && !pane.beside
                && ready_for_prompt(&s, now)
            {
                ready = Some(Ready::Queued);
            }
        }
    }
    if watching == Some(s.id.as_str()) {
        s.seen_as_it_goes(now);
    }
    (s, ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::State::{Idle, Stopped, Working};
    use crate::model::{Subagent, tests::session};

    /// How long the agents without hooks here show a still screen before
    /// they're idle.
    const IDLE_AFTER: i64 = 1_000;

    /// Transcripts saying what they're given, noting what's asked of them.
    #[derive(Default)]
    struct Canned {
        left: Vec<Backgrounded>,
        ended: Option<i64>,
        asked: Vec<&'static str>,
    }

    impl Transcripts for Canned {
        fn shells_left(&mut self, _: &Session, _: &Path) -> Vec<Backgrounded> {
            self.asked.push("shells left");
            self.left.clone()
        }

        fn turn_ended(&mut self, _: &Session, _: &Path) -> Option<i64> {
            self.asked.push("turn ended");
            self.ended
        }
    }

    /// Session "a", a working Claude one (`session`), as `f` leaves it.
    fn with(f: impl FnOnce(&mut Session)) -> Session {
        let mut s = session("a");
        f(&mut s);
        s
    }

    /// The same, of an agent without hooks.
    fn generic(f: impl FnOnce(&mut Session)) -> Session {
        with(|s| {
            s.agent = "aider".into();
            f(s);
        })
    }

    /// At work since 0, its hooks quiet since, its screen `h` as still.
    fn stale(s: &mut Session) {
        s.state_since = Some(0);
        s.last_event_at = Some(0);
        s.screen_hash = Some("h".into());
        s.screen_changed_at = Some(0);
    }

    /// Idle since 0 at its prompt, seen up to 500.
    fn at_prompt(s: &mut Session) {
        s.state = Idle;
        s.state_since = Some(0);
        s.baseline_at = Some(500);
        s.screen_hash = Some("prompt".into());
        s.screen_changed_at = Some(0);
    }

    /// Idle since 0, its agent said to wait since it started.
    fn waits(s: &mut Session) {
        s.state = Idle;
        s.state_since = Some(0);
        s.started_at = Some(100);
        s.session_at = Some(200);
    }

    /// The same, with a first prompt to type in.
    fn first(s: &mut Session) {
        waits(s);
        s.pending_prompt = Some("fix it".into());
    }

    /// Its pane as `pane`, nothing queued for it.
    fn seen(pane: PaneSeen) -> Seen {
        Seen {
            pane,
            queued: false,
        }
    }

    /// Its pane live, no one attached and the list not beside it, its
    /// screen `hash` if captured, in a redraw grace or not.
    fn screen(hash: Option<&str>, redrawing: bool) -> Seen {
        seen(PaneSeen::Live(Live {
            clients: 0,
            beside: false,
            title: None,
            screen: hash.map(|hash| Screen {
                hash: hash.into(),
                redrawing,
            }),
            idle_after: Some(IDLE_AFTER),
        }))
    }

    /// The same, not in a redraw grace.
    fn live(hash: Option<&str>) -> Seen {
        screen(hash, false)
    }

    /// Its pane live with prompts queued for it, as `f` leaves the pane.
    fn queued(f: impl FnOnce(&mut Live)) -> Seen {
        let mut seen = live(None);
        seen.queued = true;
        if let PaneSeen::Live(pane) = &mut seen.pane {
            f(pane);
        }
        seen
    }

    /// Its pane dead, with what tmux says of its end.
    fn died(status: Option<i32>, signal: Option<i32>, at: Option<i64>) -> Seen {
        seen(PaneSeen::Dead(Some(Pane {
            dead: true,
            dead_status: status,
            dead_signal: signal,
            dead_at: at,
            pid: Some(1),
            size: (80, 24),
            clients: 0,
            alternate: false,
            title: String::new(),
            beside: false,
        })))
    }

    #[test]
    fn a_session_is_as_what_is_seen_of_it_says() {
        let since = |at| move |s: &mut Session| s.state_since = Some(at);
        // What it is, what's seen of it at each time in turn, whom this
        // roster only watches; and its state after, since when, whether
        // it's unread, its exit status, and what it's ready for.
        type Want = (State, Option<i64>, bool, Option<i32>, Option<Ready>);
        type Case = (
            &'static str,
            Session,
            Vec<(i64, Seen)>,
            Option<&'static str>,
            Want,
        );
        let cases: Vec<Case> = vec![
            (
                "its pane gone, it's stopped",
                with(since(1_000)),
                vec![(10_000, seen(PaneSeen::Gone))],
                None,
                (Stopped, Some(10_000), false, None, None),
            ),
            (
                "its pane dead, it's stopped as it died, with its exit status",
                with(since(1_000)),
                vec![(10_000, died(Some(2), None, Some(5_000)))],
                None,
                (Stopped, Some(5_000), false, Some(2), None),
            ),
            (
                "killed by a signal, 128 + it",
                with(since(1_000)),
                vec![(10_000, died(None, Some(9), Some(5_000)))],
                None,
                (Stopped, Some(5_000), false, Some(137), None),
            ),
            (
                "not stopped before the state it ended began",
                with(since(6_000)),
                vec![(10_000, died(Some(0), None, Some(5_000)))],
                None,
                (Stopped, Some(6_000), false, Some(0), None),
            ),
            (
                "nor after it's seen dead",
                with(since(1_000)),
                vec![(10_000, died(Some(1), None, Some(20_000)))],
                None,
                (Stopped, Some(10_000), false, Some(1), None),
            ),
            (
                "no time of death said (0), it's stopped as it's seen",
                with(since(1_000)),
                vec![(10_000, died(Some(1), None, Some(0)))],
                None,
                (Stopped, Some(10_000), false, Some(1), None),
            ),
            (
                "stopped already, its exit isn't recorded again: `stop` clears it",
                with(|s| {
                    s.state = Stopped;
                    s.state_since = Some(1_000);
                }),
                vec![(10_000, died(Some(2), None, Some(5_000)))],
                None,
                (Stopped, Some(1_000), false, None, None),
            ),
            (
                "its dead pane running again meanwhile, it's the next refresh's",
                with(since(1_000)),
                vec![(10_000, seen(PaneSeen::Dead(None)))],
                None,
                (Working, Some(1_000), false, None, None),
            ),
            (
                "stopped, its pane live, it's idle",
                with(|s| {
                    s.state = Stopped;
                    s.state_since = Some(1_000);
                }),
                vec![(10_000, live(None))],
                None,
                (Idle, Some(10_000), false, None, None),
            ),
            (
                "its hooks quiet on a turn, its screen still, it's idle, not unread",
                with(stale),
                vec![(QUIET_MS, live(Some("h")))],
                None,
                (Idle, Some(QUIET_MS), false, None, None),
            ),
            (
                "its screen changed since, it's at work",
                with(stale),
                vec![(QUIET_MS, live(Some("h2")))],
                None,
                (Working, Some(0), false, None, None),
            ),
            (
                "its hooks heard lately, its screen isn't gone by",
                with(|s| {
                    stale(s);
                    s.last_event_at = Some(1_000);
                }),
                vec![(QUIET_MS, live(Some("h")))],
                None,
                (Working, Some(0), false, None, None),
            ),
            (
                "redrawn in a redraw grace, its screen is as still as before",
                with(stale),
                vec![
                    (QUIET_MS, screen(Some("h2"), true)),
                    (QUIET_MS + 500, live(Some("h2"))),
                ],
                None,
                (Idle, Some(QUIET_MS + 500), false, None, None),
            ),
            (
                "without hooks, its screen changing, it's at work",
                generic(at_prompt),
                vec![(1_000, live(Some("answering")))],
                None,
                (Working, Some(1_000), false, None, None),
            ),
            (
                "still for less than idle_after, it's at work still",
                generic(at_prompt),
                vec![
                    (1_000, live(Some("answering"))),
                    (1_000 + IDLE_AFTER - 1, live(Some("answering"))),
                ],
                None,
                (Working, Some(1_000), false, None, None),
            ),
            (
                "then still for idle_after, it's idle, unread as it changed past its baseline",
                generic(at_prompt),
                vec![
                    (1_000, live(Some("answering"))),
                    (1_500, live(Some("answer"))),
                    (1_500 + IDLE_AFTER, live(Some("answer"))),
                ],
                None,
                (Idle, Some(1_500 + IDLE_AFTER), true, None, None),
            ),
            (
                "changing only up to its baseline (starting up), it ends idle, not unread",
                generic(|s| {
                    at_prompt(s);
                    s.baseline_at = Some(5_000);
                }),
                vec![
                    (1_000, live(Some("banner"))),
                    (1_000 + IDLE_AFTER, live(Some("banner"))),
                ],
                None,
                (Idle, Some(1_000 + IDLE_AFTER), false, None, None),
            ),
            (
                "without hooks, redrawn in a redraw grace, it's no turn",
                generic(at_prompt),
                vec![
                    (1_000, screen(Some("prompt, wider"), true)),
                    (1_000 + IDLE_AFTER, live(Some("prompt, wider"))),
                ],
                None,
                (Idle, Some(0), false, None, None),
            ),
            (
                "watched, it's read",
                with(|s| {
                    waits(s);
                    s.unread = true;
                }),
                vec![(1_000, live(None))],
                Some("a"),
                (Idle, Some(0), false, None, None),
            ),
            (
                "watched, a turn without hooks seen as it goes isn't unread",
                generic(at_prompt),
                vec![
                    (1_000, live(Some("answering"))),
                    (1_500, live(Some("answer"))),
                    (1_500 + IDLE_AFTER, live(Some("answer"))),
                ],
                Some("a"),
                (Idle, Some(1_500 + IDLE_AFTER), false, None, None),
            ),
            (
                "another watched, it isn't read",
                with(|s| {
                    waits(s);
                    s.unread = true;
                }),
                vec![(1_000, live(None))],
                Some("b"),
                (Idle, Some(0), true, None, None),
            ),
            (
                "idle, said to wait since it started, it's ready for its first prompt",
                with(first),
                vec![(1_000, live(None))],
                None,
                (Idle, Some(0), false, None, Some(Ready::FirstPrompt)),
            ),
            (
                "not as said in a start before",
                with(|s| {
                    first(s);
                    s.session_at = Some(50);
                }),
                vec![(1_000, live(None))],
                None,
                (Idle, Some(0), false, None, None),
            ),
            (
                "nor while this roster only watches one, attached",
                with(first),
                vec![(1_000, live(None))],
                Some("b"),
                (Idle, Some(0), false, None, None),
            ),
            (
                "without hooks, idle past its start's baseline, it's ready",
                generic(|s| {
                    at_prompt(s);
                    s.pending_prompt = Some("fix it".into());
                }),
                vec![(1_000, live(Some("prompt")))],
                None,
                (Idle, Some(0), false, None, Some(Ready::FirstPrompt)),
            ),
            (
                "not up to it, starting up",
                generic(|s| {
                    at_prompt(s);
                    s.pending_prompt = Some("fix it".into());
                }),
                vec![(500, live(Some("prompt")))],
                None,
                (Idle, Some(0), false, None, None),
            ),
            (
                "ready as for a first prompt, none, no one attached, it's ready for what's queued",
                with(waits),
                vec![(1_000, queued(|_| {}))],
                None,
                (Idle, Some(0), false, None, Some(Ready::Queued)),
            ),
            (
                "its first prompt goes first",
                with(first),
                vec![(1_000, queued(|_| {}))],
                None,
                (Idle, Some(0), false, None, Some(Ready::FirstPrompt)),
            ),
            (
                "not with a terminal attached",
                with(waits),
                vec![(1_000, queued(|pane| pane.clients = 1))],
                None,
                (Idle, Some(0), false, None, None),
            ),
            (
                "nor with the list beside it",
                with(waits),
                vec![(1_000, queued(|pane| pane.beside = true))],
                None,
                (Idle, Some(0), false, None, None),
            ),
            (
                "nor while this roster only watches it",
                with(waits),
                vec![(1_000, queued(|_| {}))],
                Some("a"),
                (Idle, Some(0), false, None, None),
            ),
            (
                "its pane dead, it's ready for nothing",
                with(first),
                vec![(1_000, died(Some(0), None, Some(900)))],
                None,
                (Stopped, Some(900), false, Some(0), None),
            ),
        ];
        for (what, s, steps, watching, want) in cases {
            let (mut s, mut ready) = (s, None);
            let mut transcripts = Canned::default();
            for (now, seen) in steps {
                (s, ready) = decide(s, seen, watching, now, &mut transcripts);
            }
            let got = (s.state, s.state_since, s.unread, s.exit_status, ready);
            assert_eq!(got, want, "{what}");
            assert!(transcripts.asked.is_empty(), "{what}");
        }
    }

    #[test]
    fn a_transcript_is_read_only_as_a_turn_its_screen_says_is_over_is_corrected() {
        let now = 500 + QUIET_MS;
        let quiet = |agent: &str, transcript: Option<&str>| {
            with(|s| {
                s.agent = agent.into();
                s.transcript_path = transcript.map(Into::into);
                s.state_since = Some(500);
                s.last_event_at = Some(500);
                s.screen_hash = Some("h".into());
                s.screen_changed_at = Some(500);
            })
        };
        let shell = Backgrounded {
            id: "shell-1".into(),
            ..Default::default()
        };
        let with_subagent = {
            let mut s = quiet("codex", Some("rollout.jsonl"));
            let subagent = Subagent {
                kind: None,
                started: 0,
                heard: 0,
            };
            s.subagents.insert("sub-1".into(), subagent);
            s
        };
        // What it is, its screen as captured, what its transcript says
        // it left running and when its turn ended; and what was read, its
        // state after, whether it's unread, and whether it's left at work
        // on what its turn left.
        type Want = (Vec<&'static str>, State, bool, bool);
        type Case = (
            &'static str,
            Session,
            &'static str,
            Vec<Backgrounded>,
            Option<i64>,
            Want,
        );
        let cases: Vec<Case> = vec![
            (
                "its screen changed: nothing's read",
                quiet("claude", Some("t.jsonl")),
                "h2",
                Vec::new(),
                Some(600),
                (Vec::new(), Working, false, false),
            ),
            (
                "no transcript: nothing's read",
                quiet("claude", None),
                "h",
                Vec::new(),
                Some(600),
                (Vec::new(), Idle, false, false),
            ),
            (
                "its turn ended since: there's its answer to read",
                quiet("claude", Some("t.jsonl")),
                "h",
                Vec::new(),
                Some(600),
                (vec!["shells left", "turn ended"], Idle, true, false),
            ),
            (
                "a turn ended before it began isn't its",
                quiet("claude", Some("t.jsonl")),
                "h",
                Vec::new(),
                Some(400),
                (vec!["shells left", "turn ended"], Idle, false, false),
            ),
            (
                "shells left running keep it at work, its answer unread",
                quiet("codex", Some("rollout.jsonl")),
                "h",
                vec![shell],
                Some(600),
                (vec!["shells left", "turn ended"], Working, true, true),
            ),
            (
                "subagents outliving the interrupt put its end off",
                with_subagent,
                "h",
                Vec::new(),
                Some(600),
                (vec!["shells left"], Working, false, true),
            ),
        ];
        for (what, s, hash, left, ended, want) in cases {
            let mut transcripts = Canned {
                left,
                ended,
                asked: Vec::new(),
            };
            let (s, _) = decide(s, live(Some(hash)), None, now, &mut transcripts);
            let got = (transcripts.asked, s.state, s.unread, s.awaiting);
            assert_eq!(got, want, "{what}");
        }
    }
}
