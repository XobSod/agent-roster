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

pub use self::{
    notes::NoteSaved,
    prompts::Undelivered,
    sessions::{Defaults, NewSession},
};

// What one job has that others use, as when they were one file.
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
        let panes = self.tmux.panes()?;
        self.resized.retain(|_, until| *until > now);
        // Typed once every session is saved, each claimed just before it's
        // typed: a refresh that stops before then takes none of them.
        let mut ready = Vec::new();
        let mut titles = Vec::new();
        for mut s in self.db.sessions(false)? {
            let before = s.clone();
            let redrawing = self.resized.contains_key(&s.id);
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
                            && let Some(hash) = self.screen_hash(&s)
                        {
                            if redrawing {
                                s.redrawn(&hash, now);
                            } else {
                                s.observe_screen(&hash, now);
                                s.correct_stale_working(
                                    now,
                                    adapter.subagents_outlive_interrupts(),
                                );
                            }
                        }
                    } else if let Some(hash) = self.screen_hash(&s) {
                        let idle_after = agent::idle_after_ms(&s.agent, &self.config);
                        if redrawing {
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
            let hash = self.screen_hash(&s);
            s.mark_seen(hash, now_ms());
        }
        self.db.save_changes(&before, &s)
    }

    fn screen_hash(&self, s: &Session) -> Option<String> {
        let screen = self.tmux.capture(&s.tmux_name(), false, 0).ok()?;
        Some(util::fnv1a(screen.as_bytes()))
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
