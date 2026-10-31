//! The operations behind the CLI and the TUI, so the two stay at parity.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::{Config, FitPreview};
use crate::db::Db;
use crate::model::{Project, QUIET_MS, Session, State};
use crate::note::{self, Note, Noted, Whose};
use crate::paths::{self, PROJECT_ENV, Paths};
use crate::stats::AgentStats;
use crate::tmux::{Pane, Tmux};
use crate::util::{self, now_ms};
use crate::{agent, launch, procs};

mod asking;
mod attach;
mod notes;
mod processes;
mod projects;
mod prompts;
mod sessions;
mod starting;

pub use self::{
    notes::NoteSaved,
    prompts::Undelivered,
    sessions::{Defaults, NewSession},
    starting::{StartCheck, Starting, start_check},
};

// What one job has that the others use.
use self::{processes::*, prompts::*, sessions::*};

/// What's said of a config roster's tmux didn't take: what tmux said first
/// (a long message is cut at its end), and that roster goes on.
fn config_notice(e: &anyhow::Error) -> String {
    format!("{e} (roster's tmux goes on without it)")
}

/// Window size for new sessions when roster isn't run from a terminal.
const DEFAULT_SIZE: (u16, u16) = (160, 48);

/// How long `stop` waits after SIGTERM before SIGKILL.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The TUI's preview size, in the database's meta table: `WxH`.
const PREVIEW_SIZE: &str = "preview_size";

/// Screen changes this soon after a start are the agent starting up, not a turn.
const BOOT_GRACE_MS: i64 = 5000;

/// Screen changes this soon after roster resized a window are the agent
/// redrawing for its new size, not activity.
const RESIZE_GRACE_MS: i64 = 3000;

/// The meta key holding the chats project's id.
const CHAT_PROJECT: &str = "chat_project";

pub struct Roster {
    pub paths: Paths,
    pub config: Config,
    pub db: Db,
    pub(crate) tmux: Tmux,
    exe: PathBuf,
    /// Sessions whose window this process resized, and until when their screen
    /// changes are the agent redrawing.
    resized: HashMap<String, i64>,
    /// The session only watched, attached (`watch_beside`): a refresh types
    /// no first prompt in and asks no title, and what it does is seen.
    watching: Option<String>,
    /// The panes `refresh` last listed, by tmux session, for what follows it
    /// (fitting screens, showing one) without listing them again.
    panes: Option<HashMap<String, Pane>>,
    /// The text `config` came from, to notice the file changing.
    pub(crate) config_text: String,
    /// What roster last refused to use, so it says so once, not each tick:
    /// the file's text, invalid, or why it couldn't be read.
    pub(crate) config_refused: Option<String>,
    /// First prompts that couldn't be typed in, by session, and how often
    /// this process tried each.
    prompt_tries: HashMap<String, u32>,
    /// Why first prompts weren't typed in, to say (`take_undelivered`): not
    /// errors of the refresh, which goes on for the other sessions.
    undelivered: Vec<Undelivered>,
    /// What went wrong that isn't worth failing for: roster's tmux not
    /// taking its config, on opening or as a start starts the server, and
    /// settings roster no longer has.
    pub(crate) notices: Vec<String>,
    /// The processes as last read, for the CPU each took since.
    sampler: procs::Sampler,
    /// The preview's size as this process last saved it (`PREVIEW_SIZE`).
    told_preview: Option<(u16, u16)>,
    /// Which of its sessions each process was started in, by pid and start,
    /// as read once from its environment.
    sessions_of: HashMap<(i32, u64), Option<String>>,
    /// How far each session's transcript has been read for the ends of its
    /// shells, by session.
    shells_read: HashMap<String, ShellsRead>,
}

/// How far a session's transcript has been read for the ends of what its
/// agent left running (`Adapter::shells_ended`), and those found, by id. An
/// id may have several, as codex gives a later command the id of one that's
/// over. A refresh reads only what was written since.
struct ShellsRead {
    path: String,
    upto: u64,
    ended: HashMap<String, Vec<Seen>>,
    /// The same ends again by what each ran, where said
    /// (`ShellEnd::also`), for a shell whose id went unsaid: one each.
    by_what: Vec<ByWhat>,
}

/// An end of a shell's, as read: when it was written (else read), whether
/// its agent was told it then, where, and when its command started, if said.
#[derive(Clone, Copy)]
struct Seen {
    at: i64,
    told: bool,
    line: u64,
    started: Option<i64>,
}

/// The ends written at `lines`, by whatever they're told by, taken: no
/// other shell's, as `taken` notes for every shell's (`ShellsFound`).
fn take(by_what: &mut [ByWhat], lines: &[u64], taken: &mut Vec<(String, u64)>) {
    for end in by_what
        .iter_mut()
        .filter(|e| !e.taken && lines.contains(&e.seen.line))
    {
        end.taken = true;
        taken.push((end.what.clone(), end.seen.line));
    }
}

/// An end by what its command ran (`ShellsRead::by_what`): with its id, and
/// whether a shell was told by it already, an end being one command's.
struct ByWhat {
    what: String,
    id: String,
    seen: Seen,
    taken: bool,
}

/// How long an end queued for its agent while it waits is left to the turn
/// the agent starts on it, which reports it (written a moment before the
/// report comes); only one it never reports is taken from its transcript.
/// Longer than a report may wait for the database (`report`'s busy
/// timeout), so a slow one still comes first.
const SHELL_END_GRACE_MS: i64 = 10_000;

/// How much later than a shell is known to have started its command may
/// say it did (ms): what an agent writes and what it ran are timed apart.
const SHELL_START_SLACK_MS: i64 = 1_000;

/// How much further than its shells' own mark a transcript is read with
/// none of their ends in it before that's saved with them, so the next
/// roster process starts reading there.
const SHELL_READ_SAVED: u64 = 1 << 20;

/// Which of `s`'s shells its transcript says have ended, each by when, read
/// on from where `read` left it; and how far it's read with none of their
/// ends in it, once that's worth saving (`SHELL_READ_SAVED`).
fn shells_ended(
    read: &mut HashMap<String, ShellsRead>,
    s: &Session,
    now: i64,
) -> crate::db::ShellsFound {
    let (Some(path), Some(first)) = (
        s.transcript_path.as_deref(),
        s.shells.values().map(|shell| shell.from).min(),
    ) else {
        return crate::db::ShellsFound::default();
    };
    let fresh = || ShellsRead {
        path: path.to_string(),
        upto: first.saturating_sub(agent::adapter(&s.agent).shell_slack()),
        ended: HashMap::new(),
        by_what: Vec::new(),
    };
    let known = read.entry(s.id.clone()).or_insert_with(fresh);
    // Another conversation's (cleared since).
    if known.path != path {
        *known = fresh();
    }
    if let Some((ends, upto)) = agent::adapter(&s.agent).shells_ended(Path::new(path), known.upto) {
        for end in ends {
            let seen = Seen {
                at: end.at.unwrap_or(now).min(now),
                told: end.told,
                line: end.line,
                started: end.started,
            };
            for what in end.also {
                let id = end.id.clone();
                let taken = false;
                known.by_what.push(ByWhat {
                    what,
                    id,
                    seen,
                    taken,
                });
            }
            known.ended.entry(end.id).or_default().push(seen);
        }
        known.upto = upto;
    }
    // An end of a shell's: written after its mark, of a command started by
    // the time it was and not before the call starting it, and not one
    // another shell took (`Shell::taken`).
    let slack = agent::adapter(&s.agent).shell_slack();
    let fits = |shell: &crate::model::Shell, end: &Seen| {
        end.line >= shell.from.saturating_sub(slack)
            && !shell.taken.contains(&end.line)
            && end.started.is_none_or(|t| {
                t <= shell.started + SHELL_START_SLACK_MS
                    && shell.called.is_none_or(|c| t >= c - SHELL_START_SLACK_MS)
            })
    };
    // By its id: when first, and whether told then or after.
    let mut found: Vec<(&String, i64, bool)> = Vec::new();
    let mut taken: Vec<(String, u64)> = Vec::new();
    for (id, shell) in s
        .shells
        .iter()
        .filter(|(_, shell)| shell.ends_as.is_empty())
    {
        let Some(ends) = known.ended.get(id) else {
            continue;
        };
        let own: Vec<&Seen> = ends.iter().filter(|end| fits(shell, end)).collect();
        let Some((at, told)) = own.iter().fold(None, |found, end| match found {
            None => Some((end.at, end.told)),
            Some((first, was)) => Some((first.min(end.at), was || end.told)),
        }) else {
            continue;
        };
        found.push((id, at, told));
        // Its, so no other shell's by what it ran.
        let lines: Vec<u64> = (known.by_what.iter())
            .filter(|end| end.id == *id && own.iter().any(|o| o.line == end.seen.line))
            .map(|end| end.seen.line)
            .collect();
        take(&mut known.by_what, &lines, &mut taken);
    }
    // One whose id went unsaid, by what it ran: the first end that fits,
    // not one that fits a shell known by its id, in the order they were
    // started; one named by the id codex gave its command first, and an end
    // naming the id of another's command left to that one.
    let mut unsaid: Vec<(&String, &crate::model::Shell)> = (s.shells.iter())
        .filter(|(_, shell)| !shell.ends_as.is_empty())
        .collect();
    unsaid.sort_by_key(|(id, shell)| (shell.called.unwrap_or(shell.started), id.as_str()));
    // Codex's id for its command, where its process said: the first of what
    // it ends as, its command's text last.
    let exact =
        |shell: &crate::model::Shell| (shell.ends_as.len() > 1).then(|| shell.ends_as[0].clone());
    let named: Vec<String> = unsaid
        .iter()
        .filter_map(|(_, shell)| exact(shell))
        .collect();
    for (id, shell) in unsaid {
        let own = exact(shell);
        let by_id = |end: &ByWhat| {
            (s.shells.get(&end.id))
                .is_some_and(|other| other.ends_as.is_empty() && fits(other, &end.seen))
        };
        let anothers = |line: u64| {
            (known.by_what.iter()).any(|e| {
                e.seen.line == line && named.contains(&e.what) && own.as_ref() != Some(&e.what)
            })
        };
        let usable = |end: &ByWhat| !end.taken && !by_id(end) && fits(shell, &end.seen);
        let end = (known.by_what.iter())
            .find(|end| own.as_ref() == Some(&end.what) && usable(end))
            .or_else(|| {
                (known.by_what.iter()).find(|end| {
                    shell.ends_as.contains(&end.what) && usable(end) && !anothers(end.seen.line)
                })
            });
        if let Some(end) = end {
            found.push((id, end.seen.at, end.seen.told));
            let line = end.seen.line;
            take(&mut known.by_what, &[line], &mut taken);
        }
    }
    // Waiting, an end only queued is the turn's it starts on it, for now.
    let ended = (found.iter())
        .filter(|(_, at, told)| !s.awaiting || *told || now - at >= SHELL_END_GRACE_MS)
        .map(|(id, at, _)| (id.to_string(), *at))
        .collect();
    let read = (found.is_empty() && known.upto >= first.saturating_add(SHELL_READ_SAVED))
        .then_some(known.upto);
    crate::db::ShellsFound { ended, read, taken }
}

impl Roster {
    pub fn open() -> Result<Roster> {
        Roster::open_with(true)
    }

    /// For roster's own helpers in the background (typing a first prompt in,
    /// an automatic title), whose output goes nowhere: roster's tmux config
    /// is left to the next roster you run, which says what's wrong with it,
    /// since a mistake is said only once.
    pub fn open_quietly() -> Result<Roster> {
        Roster::open_with(false)
    }

    fn open_with(apply_tmux_config: bool) -> Result<Roster> {
        let paths = Paths::from_env()?;
        let (config, config_text) = Config::load_text(&paths.config_file())?;
        fs::create_dir_all(&paths.state_dir)
            .with_context(|| format!("can't create {}", paths.state_dir.display()))?;
        let db = Db::open(&paths.db())?;
        backfill_models(&db)?;
        let tmux = Tmux::new(paths::tmux_socket(), paths.tmux_conf());
        // A changed detach key or user tmux.conf also applies to a running
        // server. One it doesn't take is said, and roster goes on.
        let mut notices = config.gone();
        if apply_tmux_config
            && let Some(e) = tmux.apply_config(
                &config.keys.detach,
                &paths.user_tmux_conf(),
                config.status_line(),
            )?
        {
            notices.push(config_notice(&e));
        }
        let exe = util::own_exe()?;
        Ok(Roster {
            paths,
            config,
            db,
            tmux,
            exe,
            resized: HashMap::new(),
            watching: None,
            prompt_tries: HashMap::new(),
            undelivered: Vec::new(),
            notices,
            panes: None,
            config_text,
            config_refused: None,
            sampler: procs::Sampler::default(),
            told_preview: None,
            sessions_of: HashMap::new(),
            shells_read: HashMap::new(),
        })
    }

    /// Brings the database up to date: applies reported events, notices agents
    /// that have exited, follows agents' titles and screens (correcting stale
    /// "working" states, watching agents without hooks), and types first
    /// prompts into agents that are ready for them.
    pub fn refresh(&mut self) -> Result<()> {
        let now = now_ms();
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
        let panes = self.tmux.panes()?;
        self.resized.retain(|_, until| *until > now);
        // Typed once every session is saved, each claimed just before it's
        // typed: a refresh that stops before then takes none of them.
        let mut ready = Vec::new();
        let mut titles = Vec::new();
        for mut s in self.db.sessions(false)? {
            let before = s.clone();
            match panes.get(&s.tmux_name()) {
                None => s.set_state(State::Stopped, now),
                Some(pane) if pane.dead => {
                    // Record the exit code when it stops, not on every pass, so
                    // `stop` can clear it; killed by a signal, 128 + it, as a
                    // shell says.
                    if s.state != State::Stopped {
                        // When it died, not when it was seen to have (nothing
                        // is seen while you're attached, or roster's closed),
                        // though not before the state it ended.
                        let at = match pane.dead_at.filter(|&at| at > 0) {
                            Some(at) => at.min(now).max(s.state_since.unwrap_or(at)),
                            None => now,
                        };
                        s.set_state(State::Stopped, at);
                        s.exit_status = pane
                            .dead_status
                            .or(pane.dead_signal.map(|signal| 128 + signal));
                    }
                }
                Some(pane) => {
                    // Running although recorded as stopped: idle until hooks say otherwise.
                    if s.state == State::Stopped {
                        s.set_state(State::Idle, now);
                    }
                    let adapter = agent::adapter(&s.agent);
                    if let Some(title) = adapter.title(&pane.title, s.conversation_id.as_deref()) {
                        s.title = Some(title);
                    }
                    if adapter.reports() {
                        // Its hooks tell; the screen only corrects a missed end of turn.
                        if s.needs_screen_check(now)
                            && let Some(hash) = self.screen_hash(&s, now)
                        {
                            if self.resized.contains_key(&s.id) {
                                s.redrawn(&hash, now);
                            } else {
                                s.observe_screen(&hash, now);
                                let since = s.state_since;
                                let transcript = s.transcript_path.clone();
                                let transcript = transcript.as_deref().map(Path::new);
                                // What the turn left running, which an end no
                                // hook reports (a review's, a failed turn's)
                                // doesn't say.
                                if s.stale(now)
                                    && let Some(path) = transcript
                                {
                                    s.add_shells(
                                        &adapter.shells_left(&s.id, &self.paths.db(), path),
                                        now,
                                    );
                                }
                                // Its transcript saying the turn ended since,
                                // not interrupted: there's its answer to read,
                                // left with only shells running too, as when
                                // its end is reported. Subagents still at work
                                // put that off, the mark theirs to give.
                                if s.correct_stale_working(
                                    now,
                                    adapter.subagents_outlive_interrupts(),
                                ) && s.subagents.is_empty()
                                    && let Some(path) = transcript
                                    && let Some(ended) = adapter.turn_ended(path)
                                    && since.is_none_or(|since| ended >= since)
                                {
                                    s.unread = true;
                                }
                            }
                        }
                    } else if let Some(hash) = self.screen_hash(&s, now) {
                        let idle_after = agent::idle_after_ms(&s.agent, &self.config);
                        if self.resized.contains_key(&s.id) {
                            s.redrawn(&hash, now);
                        } else {
                            s.observe_generic(&hash, now, idle_after);
                        }
                    }
                    if self.watching.is_none()
                        && s.pending_prompt.is_some()
                        && self.ready_for_prompt(&s, now)
                    {
                        ready.push((s.id.clone(), s.tmux_name()));
                    }
                }
            }
            if self.watching.as_ref() == Some(&s.id) {
                s.seen_as_it_goes(now);
            }
            if self.watching.is_none()
                && self.wants_auto_title(&s)
                && let Some(conversation) = &s.conversation_id
                && self.db.claim_auto_title(&s.id, conversation)?
            {
                titles.push(s.id.clone());
            }
            if s != before {
                // Only what changed: what another roster saved meanwhile stays.
                self.db.save_changes(&before, &s)?;
            }
        }
        self.panes = Some(panes);
        for id in titles {
            self.auto_title_in_background(&id);
        }
        for (id, name) in ready {
            if self
                .prompt_tries
                .get(&id)
                .is_some_and(|&n| n >= PROMPT_TRIES)
            {
                continue;
            }
            // Claimed in the database first, so only one roster types it.
            let Some(prompt) = self.db.take_pending_prompt(&id)? else {
                continue;
            };
            if let Err(untyped) = self.tmux.type_line(&name, &prompt) {
                self.untyped(&id, &prompt, untyped);
            }
        }
        Ok(())
    }

    /// What went wrong that isn't worth failing for (`notices`), to be said.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Makes `id`'s automatic title in a roster process of its own: it takes
    /// a model call, which nothing should wait for. A failure only means no
    /// title (`s t` asks again, and says why).
    fn auto_title_in_background(&self, id: &str) {
        self.in_background(&["retitle", "--auto", id]);
    }

    /// Runs roster with `args` in the background, its output nowhere.
    fn in_background(&self, args: &[&str]) {
        let child = std::process::Command::new(&self.exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        // Waited for on a thread, so it doesn't linger as a zombie.
        if let Ok(mut child) = child {
            std::thread::spawn(move || child.wait());
        }
    }

    /// The panes as `refresh` last listed them, else as they are now.
    fn panes(&self) -> Result<std::borrow::Cow<'_, HashMap<String, Pane>>> {
        Ok(match &self.panes {
            Some(panes) => std::borrow::Cow::Borrowed(panes),
            None => std::borrow::Cow::Owned(self.tmux.panes()?),
        })
    }

    /// Sizes the windows of agents to `size` (the TUI's preview) while no one
    /// is attached, so their screens fit it instead of being cut. By default
    /// only those drawing full screen: the alternate screen keeps no
    /// scrollback that a resize could garble, so agents on the normal screen
    /// (Claude's classic renderer, say) keep the terminal's size, and
    /// attaching sizes a window to the terminal again. With `fit_preview =
    /// "always"`, every agent, its window's size kept as an attach comes
    /// (`attach_terminal` gives it the terminal's height): its width never
    /// changes but with the preview's, so its history isn't wrapped anew.
    /// Each running agent is fitted once (`fitted` notes it by window and
    /// process; the caller clears it for a new size or after an attach), so
    /// a window someone else sized, another TUI for instance, isn't fought
    /// over. `sessions` are the live ones, as just loaded.
    pub fn fit_screens(
        &mut self,
        sessions: &[Session],
        size: (u16, u16),
        fitted: &mut HashSet<String>,
    ) -> Result<()> {
        // For sessions started from the CLI meanwhile, or before the TUI
        // runs again.
        if self.told_preview != Some(size) {
            self.db
                .set_meta(PREVIEW_SIZE, &format!("{}x{}", size.0, size.1))?;
            self.told_preview = Some(size);
        }
        let panes = self.panes()?.into_owned();
        for s in sessions.iter().filter(|s| s.deleted_at.is_none()) {
            let name = s.tmux_name();
            let Some(pane) = panes.get(&name) else {
                continue;
            };
            let mode = agent::fit_mode(&s.agent, &self.config);
            let fits = match mode {
                FitPreview::Never => false,
                FitPreview::Auto => pane.alternate,
                FitPreview::Always => true,
            };
            let key = format!("{name} {:?}", pane.pid);
            // Attached elsewhere, or the list beside it not closed yet:
            // fitted again once that's over.
            if pane.clients > 0 || pane.beside {
                fitted.remove(&key);
            }
            if pane.dead || pane.clients > 0 || pane.beside || !fits {
                continue;
            }
            if fitted.insert(key) && pane.size != size {
                match mode {
                    FitPreview::Always => self.tmux.resize_fixed(&name, size)?,
                    _ => self.tmux.resize(&name, size)?,
                }
                self.resized
                    .insert(s.id.clone(), now_ms() + RESIZE_GRACE_MS);
            }
        }
        Ok(())
    }

    /// The TUI's preview size as it last fitted agents to it.
    fn preview_size(&self) -> Option<(u16, u16)> {
        util::parse_size(&self.db.meta(PREVIEW_SIZE).ok()??)
    }

    /// `refresh` for one-shot commands. The TUI polls anyway; a single command has
    /// to watch for a moment when only the screen can tell whether a session that
    /// looks busy still is: until its screen changes (working) or has stayed still
    /// long enough (idle).
    pub fn settle(&mut self) -> Result<()> {
        self.refresh()?;
        let start = now_ms();
        let longest = self
            .db
            .sessions(false)?
            .iter()
            .map(|s| agent::idle_after_ms(&s.agent, &self.config))
            .fold(QUIET_MS, i64::max);
        loop {
            let now = now_ms();
            let unresolved = self.db.sessions(false)?.iter().any(|s| {
                let watched = if agent::adapter(&s.agent).reports() {
                    s.needs_screen_check(now)
                } else {
                    s.state == State::Working
                };
                watched && s.screen_changed_at.is_some_and(|t| t <= start)
            });
            if !unresolved || now - start > longest + 1000 {
                return Ok(());
            }
            sleep(Duration::from_millis(500));
            self.refresh()?;
        }
    }

    /// What the session's agent can tell about it, from its transcript; None
    /// if it can't.
    pub fn agent_stats(&self, s: &Session) -> Option<AgentStats> {
        let path = Path::new(s.transcript_path.as_deref()?);
        let mut transcript = agent::transcript_of(s, path)?;
        transcript.counting();
        transcript.update().ok()?;
        Some(self.with_window(transcript.stats(s.running_model.as_deref()), s))
    }

    /// `stats` with the context window the agent reported for the session's
    /// model, when its transcript doesn't tell. A window smaller than what's in
    /// use is the wrong one.
    pub fn with_window(&self, mut stats: AgentStats, s: &Session) -> AgentStats {
        if stats.context_limit.is_none()
            && let (Some(used), Some(model)) = (stats.context_used, &s.running_model)
        {
            stats.context_limit = self
                .db
                .learned_window(model)
                .ok()
                .flatten()
                .filter(|w| *w >= used);
        }
        stats
    }

    /// Marks a session unread, or read: then everything on its screen so far
    /// counts as seen.
    pub fn set_unread(&mut self, id: &str, unread: bool) -> Result<()> {
        // Events reported before this must not be applied after it.
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
        let before = s.clone();
        if unread {
            s.unread = true;
        } else {
            let now = now_ms();
            let hash = self.screen_hash(&s, now);
            s.mark_seen(hash, now);
        }
        self.db.save_changes(&before, &s)
    }

    /// The hash of the session's screen, with the size it was captured at.
    /// Last seen at another size, by whichever roster (one that closed the
    /// list beside it, a terminal resized), its screens for a while are the
    /// agent redrawing.
    fn screen_hash(&mut self, s: &Session, now: i64) -> Option<String> {
        let (screen, (width, height)) = self.tmux.capture_sized(&s.tmux_name()).ok()?;
        let hash = format!("{width}x{height} {}", util::fnv1a(screen.as_bytes()));
        let size = |hash: &str| hash.split_once(' ').map(|(size, _)| size.to_string());
        if (s.screen_hash.as_deref()).is_some_and(|was| size(was) != size(&hash)) {
            self.resized.insert(s.id.clone(), now + RESIZE_GRACE_MS);
        }
        Some(hash)
    }

    /// The session's current screen, also after its agent exited.
    pub fn screen(&self, s: &Session, ansi: bool) -> Result<String> {
        let panes = self.panes()?;
        let Some(pane) = panes.get(&s.tmux_name()) else {
            bail!("{} isn't running, so there's no screen to show", s.label());
        };
        // tmux writes "Pane is dead" on the bottom line, scrolling the agent's
        // last screen up by one line.
        let history = if pane.dead { 1 } else { 0 };
        self.tmux.capture(&s.tmux_name(), ansi, history)
    }

    fn live_pid(&self, tmux_name: &str) -> Result<Option<i32>> {
        Ok(self
            .tmux
            .panes()?
            .get(tmux_name)
            .filter(|p| !p.dead)
            .and_then(|p| p.pid))
    }
}

/// Fills in the models of sessions with none recorded: from recent hook
/// events, else from the end of their Claude transcript. (What each alias
/// turned out to be is learned as start reports are folded, with what the
/// start asked for: the session's model now may have been edited since.)
fn backfill_models(db: &Db) -> Result<()> {
    db.backfill_models_from_events()?;
    for mut s in db.sessions(true)? {
        if s.running_model.is_some() || s.agent != agent::CLAUDE {
            continue;
        }
        if let Some(model) = s
            .transcript_path
            .as_deref()
            .and_then(|p| agent::claude::last_model_in(Path::new(p)))
        {
            let before = s.clone();
            s.running_model = Some(model);
            db.save_changes(&before, &s)?;
        }
    }
    Ok(())
}

/// A name for a new project in `directory`: the directory's, unless taken.
pub(crate) fn new_project_name(directory: &Path, taken: &[&str]) -> String {
    let base: String = util::directory_name(directory).chars().take(60).collect();
    let base = if base.is_empty() {
        "project".to_string()
    } else {
        base
    };
    std::iter::once(base.clone())
        .chain((2..).map(|i| format!("{base}-{i}")))
        .find(|n| !taken.contains(&n.as_str()))
        .expect("unbounded")
}

/// What removing project `p` asks first, in the TUI and the CLI: its note
/// and its `archived` sessions go with it, for good, and their notes.
pub(crate) fn project_removal_question<'a>(
    p: &Project,
    archived: impl IntoIterator<Item = &'a Session>,
) -> String {
    let name = match p.note {
        Some(_) => format!("{} and its note", p.name),
        None => p.name.clone(),
    };
    let archived: Vec<&Session> = archived.into_iter().collect();
    let notes = archived.iter().filter(|s| s.note.is_some()).count();
    let with = match (archived.len(), notes) {
        (_, 0) => String::new(),
        (1, _) => ", with its note".into(),
        (_, 1) => ", one with a note".into(),
        (_, n) => format!(", {n} with notes"),
    };
    match archived.len() {
        0 => format!("Remove project {name}?"),
        1 => format!("Remove project {name}? Its archived session goes for good{with}."),
        n => format!("Remove project {name}? Its {n} archived sessions go for good{with}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Shell;

    /// What's found of ended shells and how far it was read.
    fn pair(found: crate::db::ShellsFound) -> (Vec<(String, i64)>, Option<u64>) {
        (found.ended, found.read)
    }

    /// A Claude session waiting (or not) on shells `ids`, its transcript at
    /// `path`, as long as `from` when they were reported.
    fn on_shells(path: &Path, ids: &[&str], from: u64, awaiting: bool) -> Session {
        let mut s = crate::model::tests::session("a");
        s.transcript_path = Some(path.to_string_lossy().into_owned());
        s.awaiting = awaiting;
        for id in ids {
            let shell = Shell {
                what: None,
                started: 0,
                from,
                call: None,
                called: None,
                ends_as: Vec::new(),
                taken: Vec::new(),
            };
            s.shells.insert(id.to_string(), shell);
        }
        s
    }

    fn notice(id: &str, at: &str) -> String {
        let content = format!("<task-notification>\n<task-id>{id}</task-id>\n</task-notification>");
        serde_json::json!({"type": "queue-operation", "operation": "enqueue",
            "content": content, "timestamp": at})
        .to_string()
            + "\n"
    }

    #[test]
    fn an_end_told_by_what_it_ran_is_one_shells_whichever_roster_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout.jsonl");
        let ran = |id: &str, started: i64, ms: i64| {
            serde_json::json!({"type": "event_msg", "payload": {"type": "item_completed",
                "item": {"type": "CommandExecution", "process_id": id, "command": ["sh", "-c", "sleep 30"]},
                "started_at_ms": started, "completed_at_ms": ms}})
            .to_string()
                + "\n"
        };
        std::fs::write(&rollout, ran("7001", 10_100, 40_000)).unwrap();
        let what = {
            let (ends, _) = agent::adapter("codex").shells_ended(&rollout, 0).unwrap();
            ends[0].also.first().cloned()
        };
        let cut = |called: i64| Shell {
            started: called + 2_000,
            called: Some(called),
            ends_as: what.clone().into_iter().collect(),
            ..Default::default()
        };
        let mut s = on_shells(&rollout, &[], 0, false);
        s.agent = "codex".into();
        s.shells.insert("cut-c1".into(), cut(10_000));
        s.shells.insert("cut-c2".into(), cut(10_001));
        // Its id given since to a command started after it ended: that one's
        // isn't it, so it ends the one cut short.
        let later = Shell {
            started: 41_500,
            called: Some(41_000),
            ..Default::default()
        };
        s.shells.insert("7001".into(), later);
        // Its process naming another id than its end does: by what it ran.
        s.shells
            .get_mut("cut-c1")
            .unwrap()
            .ends_as
            .insert(0, "exec-other".into());
        let found = shells_ended(&mut HashMap::new(), &s, 50_000);
        assert_eq!(found.ended, [("cut-c1".to_string(), 40_000)]);
        // Taken, it's no other's, read by another roster process too.
        assert!(s.shells_ended(&found.ended));
        assert!(s.ends_taken(&found.taken));
        assert!(
            shells_ended(&mut HashMap::new(), &s, 50_000)
                .ended
                .is_empty()
        );
    }

    #[test]
    fn an_end_naming_a_shells_command_is_that_shells_not_anothers_by_what_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout.jsonl");
        let ran = |item: &str, started: i64, ms: i64| {
            serde_json::json!({"type": "event_msg", "payload": {"type": "item_completed",
                "item": {"type": "CommandExecution", "id": item, "process_id": "7001",
                    "command": ["sh", "-c", "sleep 30"]},
                "started_at_ms": started, "completed_at_ms": ms}})
            .to_string()
                + "\n"
        };
        // Two the same cut short, each by its own process's id, its text after.
        std::fs::write(&rollout, ran("exec-b", 10_100, 20_000)).unwrap();
        let hash = {
            let (ends, _) = agent::adapter("codex").shells_ended(&rollout, 0).unwrap();
            ends[0].also[1].clone()
        };
        let mut s = on_shells(&rollout, &[], 0, false);
        s.agent = "codex".into();
        for (key, item, called) in [("cut-a", "exec-a", 1_000), ("cut-b", "exec-b", 10_000)] {
            let shell = Shell {
                started: 12_000,
                called: Some(called),
                ends_as: vec![item.to_string(), hash.clone()],
                ..Default::default()
            };
            s.shells.insert(key.into(), shell);
        }
        let mut read = HashMap::new();
        // The second's end: the second's, though the first, called first,
        // runs the same.
        let found = shells_ended(&mut read, &s, 50_000);
        assert_eq!(found.ended, [("cut-b".to_string(), 20_000)]);
        assert!(s.shells_ended(&found.ended));
        s.ends_taken(&found.taken);
        let text = std::fs::read_to_string(&rollout).unwrap();
        std::fs::write(&rollout, format!("{text}{}", ran("exec-a", 1_100, 31_000))).unwrap();
        let found = shells_ended(&mut read, &s, 50_000);
        assert_eq!(found.ended, [("cut-a".to_string(), 31_000)]);
    }

    #[test]
    fn shells_ends_are_read_on_from_where_the_last_read_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        let at = |text: &str| crate::util::iso_ms(text).unwrap();
        let (t1, t2) = ("2026-10-29T10:00:01Z", "2026-10-29T10:00:02Z");
        // An end written before its shell was reported, a little.
        let early = notice("b1", t1);
        std::fs::write(&path, format!("{early}{}\n", "x".repeat(100))).unwrap();
        let mut read = HashMap::new();
        let later = at(t1) + SHELL_END_GRACE_MS;
        let s = on_shells(&path, &["b1"], early.len() as u64 + 50, false);
        assert_eq!(
            pair(shells_ended(&mut read, &s, later)),
            (vec![("b1".to_string(), at(t1))], None)
        );
        // Read on from there: an end seen before its shell was known counts
        // once it is.
        std::fs::write(
            &path,
            format!("{early}{}\n{}", "x".repeat(100), notice("b2", t2)),
        )
        .unwrap();
        let s = on_shells(&path, &["b3"], 0, false);
        assert_eq!(pair(shells_ended(&mut read, &s, later)), (Vec::new(), None));
        let s = on_shells(&path, &["b2"], 0, false);
        assert_eq!(
            shells_ended(&mut read, &s, later).ended,
            [("b2".to_string(), at(t2))]
        );
        // Waiting on it, a fresh end only queued is left to the turn its
        // agent starts on it; one it was told already isn't.
        let s = on_shells(&path, &["b2"], 0, true);
        assert!(shells_ended(&mut read, &s, at(t2) + 1).ended.is_empty());
        assert_eq!(shells_ended(&mut read, &s, later + 1_000).ended.len(), 1);
        let told = serde_json::json!({"type": "attachment", "timestamp": t2, "attachment": {
            "type": "queued_command", "prompt": "<task-notification>\n<task-id>b5</task-id>\n</task-notification>"}});
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{told}\n")).unwrap();
        let s = on_shells(&path, &["b5"], 0, true);
        assert_eq!(
            shells_ended(&mut read, &s, at(t2) + 1).ended,
            [("b5".to_string(), at(t2))]
        );
        // A shell with an end known, even one held back, isn't read past.
        let text = std::fs::read_to_string(&path).unwrap();
        let filler = format!("{}\n", "z".repeat(SHELL_READ_SAVED as usize));
        std::fs::write(&path, format!("{text}{filler}")).unwrap();
        let s = on_shells(&path, &["b2"], 0, true);
        assert_eq!(
            pair(shells_ended(&mut read, &s, at(t2) + 1)),
            (Vec::new(), None)
        );
        // Another conversation (cleared): read from its shells' mark there.
        let other = dir.path().join("d.jsonl");
        std::fs::write(&other, notice("b1", t1)).unwrap();
        let s = on_shells(&other, &["b2"], 0, false);
        assert_eq!(pair(shells_ended(&mut read, &s, later)), (Vec::new(), None));
        // Codex's are looked for from where each was seen running: an end
        // of its id before that is another command's, its id given again.
        let rollout = dir.path().join("rollout.jsonl");
        let end = |id: &str, ms: i64| {
            serde_json::json!({"type": "event_msg", "payload": {"type": "item_completed",
                "item": {"type": "CommandExecution", "process_id": id}, "completed_at_ms": ms}})
            .to_string()
                + "\n"
        };
        let before = end("7001", 1_000);
        std::fs::write(&rollout, &before).unwrap();
        let mut s = on_shells(&rollout, &["7001"], before.len() as u64, false);
        s.agent = "codex".into();
        let mut codex = HashMap::new();
        assert!(shells_ended(&mut codex, &s, later).ended.is_empty());
        std::fs::write(&rollout, format!("{before}{}", end("7001", 2_000))).unwrap();
        assert_eq!(
            shells_ended(&mut codex, &s, later).ended,
            [("7001".to_string(), 2_000)]
        );
        // Each one's from its own mark: another's, earlier, isn't its.
        let mut s = on_shells(&rollout, &["7001", "7002"], before.len() as u64, false);
        s.agent = "codex".into();
        let mark = std::fs::metadata(&rollout).unwrap().len();
        let text = std::fs::read_to_string(&rollout).unwrap();
        std::fs::write(&rollout, format!("{text}{}", end("7002", 3_000))).unwrap();
        s.shells.get_mut("7002").unwrap().from = mark + end("7002", 3_000).len() as u64;
        let mut codex = HashMap::new();
        assert_eq!(
            shells_ended(&mut codex, &s, later).ended,
            [("7001".to_string(), 2_000)]
        );
        let text = std::fs::read_to_string(&rollout).unwrap();
        std::fs::write(&rollout, format!("{text}{}", end("7002", 4_000))).unwrap();
        assert_eq!(
            shells_ended(&mut codex, &s, later).ended,
            [("7001".to_string(), 2_000), ("7002".to_string(), 4_000)]
        );
        // One cut short, known by what it runs: an end of one started after
        // it was, or before its call, isn't its; nor is an end of a shell
        // known by its id; and an end is one shell's.
        let ran = |id: &str, started: i64, ms: i64| {
            serde_json::json!({"type": "event_msg", "payload": {"type": "item_completed",
                "item": {"type": "CommandExecution", "process_id": id, "command": ["sh", "-c", "sleep 30"]},
                "started_at_ms": started, "completed_at_ms": ms}})
            .to_string()
                + "\n"
        };
        let what = |rollout: &Path| {
            let (ends, _) = agent::adapter("codex").shells_ended(rollout, 0).unwrap();
            ends[0].also.first().cloned()
        };
        std::fs::write(&rollout, ran("7003", 60_000, 70_000)).unwrap();
        let cut = |s: &mut Session, id: &str, called: i64| {
            let shell = crate::model::Shell {
                started: called + 12_000,
                called: Some(called),
                ends_as: what(&rollout).into_iter().collect(),
                ..Default::default()
            };
            s.shells.insert(id.to_string(), shell);
        };
        let mut s = on_shells(&rollout, &[], 0, false);
        s.agent = "codex".into();
        cut(&mut s, "cut-c1", 45_000);
        let by_id = Shell {
            started: 50_000,
            ..Default::default()
        };
        s.shells.insert("7004".into(), by_id);
        let mut codex = HashMap::new();
        // Started after it was seen cut short.
        assert!(shells_ended(&mut codex, &s, later).ended.is_empty());
        let text = std::fs::read_to_string(&rollout).unwrap();
        let more = [
            // Started before its call: another command's.
            ran("7005", 40_000, 80_000),
            // A shell's by its id.
            ran("7004", 46_000, 81_000),
            // Its own.
            ran("7006", 46_000, 82_000),
        ];
        std::fs::write(&rollout, format!("{text}{}", more.concat())).unwrap();
        let mut ended = shells_ended(&mut codex, &s, later).ended;
        ended.sort();
        assert_eq!(
            ended,
            [("7004".to_string(), 81_000), ("cut-c1".to_string(), 82_000)]
        );
        // Another the same, cut short at once: an end each, none of those
        // taken, by what it ran or by its id.
        s.shells.remove("7004");
        s.shells.remove("cut-c1");
        cut(&mut s, "cut-c2", 45_001);
        assert!(shells_ended(&mut codex, &s, later).ended.is_empty());
        let text = std::fs::read_to_string(&rollout).unwrap();
        std::fs::write(&rollout, format!("{text}{}", ran("7007", 46_000, 90_000))).unwrap();
        assert_eq!(
            shells_ended(&mut codex, &s, later).ended,
            [("cut-c2".to_string(), 90_000)]
        );
        // Read far past its shells' mark with no end: that's to be saved.
        let long = format!("{}\n", "y".repeat(SHELL_READ_SAVED as usize));
        std::fs::write(&other, &long).unwrap();
        let s = on_shells(&other, &["b4"], 0, false);
        let mut read = HashMap::new();
        assert_eq!(
            pair(shells_ended(&mut read, &s, later)),
            (Vec::new(), Some(long.len() as u64))
        );
    }
}
