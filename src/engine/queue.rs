//! Prompts sent to a session: queued, then typed into its agent one at a
//! time, once it's ready and no one is attached, by whichever roster process
//! gets to each first; one in the background while nothing else runs.

use super::*;
use crate::model::{Queued, SENT};

/// How long the roster typing what's queued in the background goes on: past
/// it, what's left waits for the next roster.
const DELIVER_QUEUED_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

/// How many refreshes in a row may fail (the database busy, tmux not
/// answering) before the roster typing what's queued in the background gives
/// up.
const DELIVER_QUEUED_FAILURES: u32 = 30;

/// The lock the roster typing what's queued in the background holds, so
/// there's one.
const DELIVER_QUEUED_LOCK: &str = "deliver-queued.lock";

/// What sending a prompt came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    /// Typed into its agent, or given it as it started.
    Typed,
    /// Queued, `ahead` before it, waiting for what `waiting` says.
    Queued { ahead: usize, waiting: String },
}

impl Roster {
    /// The session this runs in (its agent's, by `ROSTER_SESSION`), if it's
    /// a live one.
    pub fn own_session(&self) -> Result<Option<Session>> {
        match std::env::var("ROSTER_SESSION") {
            Ok(id) if !id.is_empty() => {
                Ok((self.db.sessions(false)?.into_iter()).find(|s| s.id == id))
            }
            _ => Ok(None),
        }
    }

    /// Sends `prompt` to session `id` from the session this runs in, if any:
    /// queued, and typed in at once if its agent is ready for it. A stopped
    /// one is started first, only if `start` and starting it asks nothing.
    pub fn send(&mut self, id: &str, prompt: &str, start: bool) -> Result<Sent> {
        self.refresh()?;
        let s = self.live_session(id)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("nothing to send");
        }
        let sender = self.own_session()?;
        if sender.as_ref().is_some_and(|me| me.id == s.id) {
            bail!("a session can't send to itself");
        }
        check_queued_fits(&s, prompt).with_context(|| format!("nothing sent to {}", s.label()))?;
        let stopped = s.state == State::Stopped;
        if stopped && !start {
            bail!(
                "{} is stopped: --start starts it, and the prompt goes in once it's ready",
                s.label()
            );
        }
        // Started only if that asks nothing: what it asks is yours to
        // answer, not a sender's.
        if stopped {
            match start_check(&s, Starting::Start) {
                StartCheck::Go => {}
                StartCheck::Refused(why) => bail!("nothing sent to {}: {why}", s.label()),
                StartCheck::Confirm(question) | StartCheck::PromptAgain { question, .. } => bail!(
                    "nothing sent to {}: `roster start` it first, as starting it asks this: {question}",
                    s.label()
                ),
            }
        }
        let from = sender.as_ref().map(|me| (me.id.clone(), me.label()));
        let from = from
            .as_ref()
            .map(|(id, label)| (id.as_str(), label.as_str()));
        let (queued, ahead) = self
            .db
            .enqueue(&s.id, prompt, from, now_ms(), self.config.queue_limit())
            .with_context(|| format!("nothing sent to {}", s.label()))?;
        // Not started, not sent: sent again, it isn't queued twice. Started
        // by another roster meanwhile, it's queued for that start.
        if stopped && let Err(e) = self.start(&s.id) {
            self.refresh()?;
            if self.db.session(&s.id)?.state == State::Stopped {
                // Gone already: given its agent by that other start.
                if self.db.drop_queued(&s.id, Some(queued))? == 0 {
                    return Ok(Sent::Typed);
                }
                return Err(e.context(format!("nothing sent to {}", s.label())));
            }
        }
        self.refresh()?;
        if !self.db.queue(&s.id)?.iter().any(|q| q.id == queued) {
            return Ok(Sent::Typed);
        }
        self.deliver_queued_in_background();
        let waiting = match stopped {
            true => "it's starting".into(),
            false => self.waiting_for(&self.db.session(&s.id)?)?,
        };
        Ok(Sent::Queued { ahead, waiting })
    }

    /// What a prompt queued for `s` waits for, to say.
    fn waiting_for(&self, s: &Session) -> Result<String> {
        let attached =
            (self.panes()?.get(&s.tmux_name())).is_some_and(|p| p.clients > 0 || p.beside);
        Ok(match s.state {
            State::Stopped => "it's stopped".into(),
            _ if s.pending_prompt.is_some() => "its first prompt goes in first".into(),
            _ if attached => "a terminal is attached: what's typed there goes first".into(),
            // At its first screen, unheard of since it started, given nothing.
            _ if agent::adapter(&s.agent).takes_prompt()
                && !s.waits_since_started()
                && s.given_prompt.is_none() =>
            {
                format!(
                    "{} says it's ready only after a turn: one you type into it",
                    util::capitalize(&s.agent)
                )
            }
            State::Working => "it's working".into(),
            State::NeedsInput => "it asks something first".into(),
            State::Idle => "its agent isn't ready for it yet".into(),
        })
    }

    /// Types the first of session `s`'s queue into its agent, claimed first
    /// with the turn it begins, so no other roster types it or another into
    /// that turn; the next waits for it to end. Not while a terminal is
    /// attached, as one may be since the panes were listed. Recorded, with
    /// who sent it. Tried a few times in each start of its agent.
    pub(super) fn type_queued(&mut self, s: &Session) -> Result<()> {
        let (id, name, started) = (s.id.as_str(), &s.tmux_name(), s.started_at);
        let tries = self.send_tries(id, started);
        if tries >= PROMPT_TRIES {
            return Ok(());
        }
        let alone = |r: &Self| {
            let panes = r.tmux.panes().ok();
            (panes.as_ref().and_then(|panes| panes.get(name)))
                .is_some_and(|p| p.clients == 0 && !p.beside)
        };
        if !alone(self) {
            return Ok(());
        }
        let begin = |s: &mut Session| s.pending_prompt.is_none() && self.begin_turn(s, now_ms());
        let Some(turn) = self.db.take_queued(id, begin)? else {
            return Ok(());
        };
        // Attached while the claim waited for the database.
        if !alone(self) {
            self.db.give_back_queued(&turn)?;
            return Ok(());
        }
        let q = &turn.prompt;
        let Err(untyped) = self.tmux.type_line(name, &q.prompt) else {
            self.send_tries.remove(id);
            return self.record_sent(id, q, now_ms());
        };
        // In already, only its Enter missing: not typed twice, and nothing
        // more typed onto its line.
        let tries = match untyped.pasted {
            true => PROMPT_TRIES,
            false => {
                self.db.give_back_queued(&turn)?;
                tries + 1
            }
        };
        self.send_tries.insert(id.to_string(), (started, tries));
        let label = self.db.session(id).map_or(id.to_string(), |s| s.label());
        let what = match untyped.pasted {
            true => "it's in, but Enter didn't get to it",
            false => "it stays queued",
        };
        self.undelivered.push(Undelivered {
            id: id.to_string(),
            message: format!(
                "couldn't type a prompt sent to {label} ({what}): {:#}",
                untyped.error
            ),
            again: tries < PROMPT_TRIES,
        });
        Ok(())
    }

    /// How often this roster tried typing what's queued into session `id`
    /// in its agent's start `started`.
    fn send_tries(&self, id: &str, started: Option<i64>) -> u32 {
        match self.send_tries.get(id) {
            Some(&(at, tries)) if at == started => tries,
            _ => 0,
        }
    }

    /// Session `id` given `q`, sent to it, at `now`: recorded, with who sent
    /// it.
    pub(super) fn record_sent(&self, id: &str, q: &Queued, now: i64) -> Result<()> {
        let said = serde_json::json!({
            "from": q.sender,
            "label": q.sender_label,
            "prompt": q.prompt,
        });
        self.db.insert_event(id, SENT, now, Some(&said.to_string()))
    }

    /// Where the prompt `queued` (its id in a queue) is written for
    /// `$EDITOR`, among the notes being edited.
    pub fn queued_file(&self, queued: i64) -> PathBuf {
        (self.paths.state_dir.join("notes")).join(format!("queued-{queued}.md"))
    }

    /// Types what's queued in a roster process of its own (`roster
    /// deliver-queued`), for while nothing else refreshes; one at a time.
    pub fn deliver_queued_in_background(&self) {
        self.in_background(&["deliver-queued"]);
    }

    /// Whether anything queued can go in yet: for a session that runs, and
    /// that this roster hasn't given up typing into; a stopped one's waits
    /// for its start.
    fn deliverable(&self) -> Result<bool> {
        let queued = self.db.queued_sessions()?;
        let sessions = self.db.sessions(false)?;
        let tried = |s: &Session| self.send_tries(&s.id, s.started_at) >= PROMPT_TRIES;
        Ok((sessions.iter())
            .any(|s| queued.contains(&s.id) && s.state != State::Stopped && !tried(s)))
    }

    /// `roster deliver-queued`: refreshes, so what's queued is typed in as
    /// each agent is ready, till nothing that can go in is left, or
    /// `DELIVER_QUEUED_TIMEOUT`. Another one at it already, this one leaves it
    /// to that.
    pub fn deliver_queued(&mut self) -> Result<()> {
        let lock = self.paths.state_dir.join(DELIVER_QUEUED_LOCK);
        let deadline = Instant::now() + DELIVER_QUEUED_TIMEOUT;
        loop {
            let Some(held) = try_lock(&lock)? else {
                return Ok(());
            };
            let mut failed = 0;
            while self.deliverable()? && Instant::now() < deadline {
                match self.refresh() {
                    Ok(()) => failed = 0,
                    Err(_) if failed < DELIVER_QUEUED_FAILURES => failed += 1,
                    Err(e) => return Err(e),
                }
                // No one to say them to.
                self.take_undelivered();
                sleep(Duration::from_secs(1));
            }
            drop(held);
            // Sent between the last look and letting go: no other took it up,
            // this one held the lock.
            if !self.deliverable()? || Instant::now() >= deadline {
                return Ok(());
            }
        }
    }
}

/// A prompt that fits a queue of `s`'s, or why not: one for an agent given
/// it on its command line, if it's first as its agent starts, fits that.
pub fn check_queued_fits(s: &Session, prompt: &str) -> Result<()> {
    if agent::adapter(&s.agent).takes_prompt() && prompt.len() > LONGEST_ARGUMENT {
        bail!(
            "{} bytes are too long for {}'s command line, where it may be given, which takes {LONGEST_ARGUMENT} at most",
            prompt.len(),
            s.agent
        );
    }
    Ok(())
}

/// The lock at `path`, held till what's returned goes; None if another
/// process holds it.
fn try_lock(path: &Path) -> Result<Option<fs::File>> {
    use std::os::fd::AsRawFd;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("can't open {}", path.display()))?;
    // SAFETY: flock only acts on the descriptor, which `file` keeps open.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(locked.then_some(file))
}
