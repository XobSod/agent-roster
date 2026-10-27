//! The operations behind the CLI and the TUI, so the two stay at parity.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::db::Db;
use crate::model::{Project, QUIET_MS, Session, State};
use crate::note::{self, Note, Noted, Whose};
use crate::paths::{self, PROJECT_ENV, Paths};
use crate::stats::AgentStats;
use crate::tmux::{Pane, Tmux};
use crate::util::{self, now_ms};
use crate::{agent, launch};

/// A note of `text`, its pin on a line it has (`note::tidy` may take lines
/// off its end); None if there's nothing but blanks.
fn new_note(text: &str, pin: Option<usize>) -> Option<Note> {
    let text = note::tidy(text);
    (!text.is_empty()).then(|| {
        let last = text.lines().count().saturating_sub(1);
        Note {
            pin: pin.map(|p| p.min(last)),
            text,
            at: now_ms(),
        }
    })
}

/// What's said of a config roster's tmux didn't take: what tmux said first
/// (a long message is cut at its end), and that roster goes on.
fn config_notice(e: &anyhow::Error) -> String {
    format!("{e} (roster's tmux goes on without it)")
}

/// Window size for new sessions when roster isn't run from a terminal.
const DEFAULT_SIZE: (u16, u16) = (160, 48);
/// How long `stop` waits after SIGTERM before SIGKILL.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
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
}

/// A first prompt `refresh` couldn't type into session `id`, and why;
/// `again`: this roster tries it again.
pub struct Undelivered {
    pub id: String,
    pub message: String,
    pub again: bool,
}

/// How often one roster tries a first prompt that never got into its agent;
/// after that it stays, for another roster or the agent's next start.
const PROMPT_TRIES: u32 = 3;

/// How the files a first prompt waits in for the launch script are named,
/// before the session's id (`Roster::prompt_file`).
const PROMPT_FILE: &str = "first-prompt-";

/// The longest argument Linux gives a program: under 128 KB
/// (`MAX_ARG_STRLEN`, its terminating NUL in it). A first prompt on the
/// agent's command line can't be longer.
const LONGEST_ARGUMENT: usize = 128 * 1024 - 1;

/// Removes the first prompts' files in `dir` a launch script never got to
/// (its shell gone, its env file failing): session `id`'s, which is starting
/// again, left by this roster or by one that's gone (another starting it now
/// has its own, about to be read), and any an hour old.
fn sweep_prompts(dir: &Path, id: &str) {
    let this_sessions = format!("{PROMPT_FILE}{id}.");
    let gone = |pid: &str| match pid.parse::<i32>() {
        Ok(pid) if pid as u32 == std::process::id() => true,
        Ok(pid) if pid > 0 => {
            // SAFETY: signal 0 only asks whether the process is there.
            let asked = unsafe { libc::kill(pid, 0) };
            asked != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        }
        _ => true,
    };
    let an_hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with(PROMPT_FILE)) else {
            continue;
        };
        let old = (entry.metadata().and_then(|m| m.modified())).is_ok_and(|t| t < an_hour_ago);
        let left = name.strip_prefix(&this_sessions).is_some_and(gone);
        if left || old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A first prompt that fits `agent`'s command line, or why not.
fn check_prompt_fits(agent: &str, prompt: &str) -> Result<()> {
    if prompt.len() > LONGEST_ARGUMENT {
        bail!(
            "the first prompt is too long for {agent}'s command line: {} bytes, where it takes {LONGEST_ARGUMENT} at most",
            prompt.len()
        );
    }
    Ok(())
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
        let exe = std::env::current_exe().context("can't find roster's own path")?;
        Ok(Roster {
            paths,
            config,
            db,
            tmux,
            exe,
            resized: HashMap::new(),
            prompt_tries: HashMap::new(),
            undelivered: Vec::new(),
            notices,
            panes: None,
            config_text,
            config_refused: None,
        })
    }

    /// Brings the database up to date: applies reported events, notices agents
    /// that have exited, follows agents' titles and screens (correcting stale
    /// "working" states, watching agents without hooks), and types first
    /// prompts into agents that are ready for them.
    pub fn refresh(&mut self) -> Result<()> {
        let now = now_ms();
        self.db.fold_events(now)?;
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
                        s.set_state(State::Stopped, now);
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
                                s.redrawn(&hash);
                            } else {
                                s.observe_screen(&hash, now);
                                s.correct_stale_working(now);
                            }
                        }
                    } else if let Some(hash) = self.screen_hash(&s) {
                        let idle_after = agent::idle_after_ms(&s.agent, &self.config);
                        if redrawing {
                            s.redrawn(&hash);
                        } else {
                            s.observe_generic(&hash, now, idle_after);
                        }
                    }
                    if s.pending_prompt.is_some() && self.ready_for_prompt(&s, now) {
                        ready.push((s.id.clone(), s.tmux_name()));
                    }
                }
            }
            if self.wants_auto_title(&s)
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

    /// A first prompt that couldn't be typed into session `id`: one that never
    /// got into its agent stays (this roster tries it a few times, then
    /// leaves it to another roster or the agent's next start); one that did
    /// (only its Enter missing) doesn't, so it isn't typed twice.
    fn untyped(&mut self, id: &str, prompt: &str, untyped: crate::tmux::Untyped) {
        let tries = self.prompt_tries.entry(id.to_string()).or_default();
        *tries += 1;
        let kept = !untyped.pasted && self.db.restore_pending_prompt(id, prompt).is_ok();
        let again = kept && *tries < PROMPT_TRIES;
        let label = self.db.session(id).map_or(id.to_string(), |s| s.label());
        let what = match (untyped.pasted, kept, again) {
            (true, ..) => "it's in, but Enter didn't get to it",
            (false, _, true) => "trying again",
            (false, true, false) => "it stays, for its next start",
            (false, false, _) => "and it's lost",
        };
        self.undelivered.push(Undelivered {
            id: id.to_string(),
            message: format!(
                "couldn't type the first prompt into {label} ({what}): {:#}",
                untyped.error
            ),
            again,
        });
    }

    /// First prompts `refresh` couldn't type in since this was last asked.
    pub fn take_undelivered(&mut self) -> Vec<Undelivered> {
        std::mem::take(&mut self.undelivered)
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

    /// Types session `id`'s first prompt in once its agent is ready, in a
    /// roster process of its own (`roster deliver`): for while you're
    /// attached, when nothing else refreshes.
    pub fn deliver_in_background(&self, id: &str) {
        self.in_background(&["deliver", id]);
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

    /// Sizes the windows of agents drawing full screen to `size` (the TUI's
    /// preview) while no one is attached, so their screens fit it instead of
    /// being cut. Only the alternate screen is safe to resize: it keeps no
    /// scrollback that a resize could garble, so agents on the normal screen
    /// (Claude's classic renderer, say) keep the terminal's size. Attaching
    /// sizes a window to the terminal again, as before. Each running agent is
    /// fitted once (`fitted` notes it by window and process; the caller clears
    /// it for a new size or after an attach), so a window someone else sized,
    /// another TUI for instance, isn't fought over.
    /// `sessions` are the live ones, as just loaded.
    pub fn fit_screens(
        &mut self,
        sessions: &[Session],
        size: (u16, u16),
        fitted: &mut HashSet<String>,
    ) -> Result<()> {
        let panes = self.panes()?.into_owned();
        for s in sessions.iter().filter(|s| s.deleted_at.is_none()) {
            let name = s.tmux_name();
            let Some(pane) = panes.get(&name) else {
                continue;
            };
            if pane.dead
                || !pane.alternate
                || pane.clients > 0
                || !agent::fits_preview(&s.agent, &self.config)
            {
                continue;
            }
            if fitted.insert(format!("{name} {:?}", pane.pid)) && pane.size != size {
                self.tmux.resize(&name, size)?;
                self.resized
                    .insert(s.id.clone(), now_ms() + RESIZE_GRACE_MS);
            }
        }
        Ok(())
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

    // Projects

    pub fn add_project(&self, name: &str, path: &Path, defaults: Defaults) -> Result<Project> {
        let project = self.checked_project(0, name, path, defaults)?;
        self.db.add_project(&project, now_ms())
    }

    /// Renames, moves or changes the defaults of a project. Its existing sessions
    /// keep their directory.
    pub fn update_project(
        &self,
        id: i64,
        name: &str,
        path: &Path,
        defaults: Defaults,
    ) -> Result<Project> {
        let mut project = self.checked_project(id, name, path, defaults)?;
        let current = self.db.projects()?.into_iter().find(|p| p.id == id);
        if self.chat_project_id()? == Some(id)
            && current.as_ref().is_some_and(|p| p.origin != project.origin)
        {
            bail!("the chats project's directory is set by `chats_dir` in config.toml");
        }
        // Its note is saved apart (`save_note`), and kept.
        project.note = current.and_then(|p| p.note);
        self.db.save_project(&project)?;
        Ok(project)
    }

    fn checked_project(
        &self,
        id: i64,
        name: &str,
        path: &Path,
        defaults: Defaults,
    ) -> Result<Project> {
        validate_name("project", name)?;
        let origin =
            fs::canonicalize(path).with_context(|| format!("can't find {}", path.display()))?;
        if !origin.is_dir() {
            bail!("{} is not a directory", origin.display());
        }
        let agent = defaults
            .agent
            .as_deref()
            .unwrap_or(self.config.default_agent());
        agent::known(agent, &self.config)?;
        if let Some(effort) = &defaults.effort {
            agent::check_effort(agent, effort)?;
        }
        Ok(Project {
            id,
            name: name.to_string(),
            origin,
            agent: defaults.agent,
            model: defaults.model,
            effort: defaults.effort,
            note: None,
        })
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        self.db.projects()
    }

    /// Where chats run: `chats_dir` in config.toml, else roster's state directory.
    pub fn chats_dir(&self) -> PathBuf {
        match &self.config.chats_dir {
            Some(dir) => PathBuf::from(util::expand_tilde(dir)),
            None => self.paths.chats_dir(),
        }
    }

    /// The built-in project chats belong to, if it exists yet.
    pub fn chat_project_id(&self) -> Result<Option<i64>> {
        let Some(id) = self.db.meta(CHAT_PROJECT)?.and_then(|v| v.parse().ok()) else {
            return Ok(None);
        };
        Ok(self.db.projects()?.iter().any(|p| p.id == id).then_some(id))
    }

    /// The project for chats, sessions that need no project of their own:
    /// created on first use (named "chats" unless that's taken), in one
    /// directory for all of them, so Claude asks to trust it only once. Its
    /// directory follows `chats_dir`; it can be renamed and given defaults.
    pub fn chat_project(&mut self) -> Result<Project> {
        let dir = self.chats_dir();
        fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
        let origin = fs::canonicalize(&dir)?;
        let projects = self.db.projects()?;
        if let Some(id) = self.chat_project_id()?
            && let Some(p) = projects.iter().find(|p| p.id == id)
        {
            let mut p = p.clone();
            // `chats_dir` changed: new chats run there; older ones keep theirs.
            if p.origin != origin {
                p.origin = origin;
                self.db.save_project(&p)?;
            }
            return Ok(p);
        }
        let taken: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        let name = new_project_name(Path::new("chats"), &taken);
        let p = self.add_project(&name, &origin, Defaults::default())?;
        self.db.set_meta(CHAT_PROJECT, &p.id.to_string())?;
        Ok(p)
    }

    /// What removing project `name` asks first (`project_removal_question`);
    /// an error if it can't be removed.
    pub fn removal_question(&self, name: &str) -> Result<String> {
        let project = self
            .db
            .project(name)?
            .with_context(|| format!("no project {name:?}"))?;
        let archived = self.db.purged_with(&project)?;
        Ok(project_removal_question(&project, &archived))
    }

    /// Removes a project that has no live sessions; returns the archived sessions
    /// purged with it.
    pub fn remove_project(&mut self, name: &str) -> Result<Vec<Session>> {
        let project = self
            .db
            .project(name)?
            .with_context(|| format!("no project {name:?}"))?;
        self.db.remove_project(&project)
    }

    // Sessions

    pub fn sessions(&self, include_archived: bool) -> Result<Vec<Session>> {
        self.db.sessions(include_archived)
    }

    /// Finds a live (or, with `archived`, an archived) session by id,
    /// `project/name`, name, or id prefix of at least 4 characters.
    pub fn resolve(&self, addr: &str, archived: bool) -> Result<Session> {
        match self.find(addr, archived)? {
            Some(s) => Ok(s),
            None => bail!(
                "no {}session {addr:?}",
                if archived { "archived " } else { "" }
            ),
        }
    }

    /// `resolve` among live sessions, else archived ones. An address that
    /// matches several live sessions says so instead of looking further.
    pub fn resolve_any(&self, addr: &str) -> Result<Session> {
        match self.find(addr, false)? {
            Some(s) => Ok(s),
            None => self.resolve(addr, true),
        }
    }

    /// The session `addr` names, None if none does, an error if several do.
    fn find(&self, addr: &str, archived: bool) -> Result<Option<Session>> {
        let pool: Vec<Session> = self
            .db
            .sessions(true)?
            .into_iter()
            .filter(|s| s.deleted_at.is_some() == archived)
            .collect();
        let rules: [&dyn Fn(&Session) -> bool; 4] = [
            &|s| s.id == addr,
            &|s| s.label() == addr,
            &|s| s.name == addr,
            &|s| addr.len() >= 4 && s.id.starts_with(addr),
        ];
        for rule in rules {
            let found: Vec<&Session> = pool.iter().filter(|s| rule(s)).collect();
            match found[..] {
                [] => continue,
                [s] => return Ok(Some(s.clone())),
                _ => {
                    let labels: Vec<String> = found
                        .iter()
                        .map(|s| format!("{} ({})", s.label(), s.id))
                        .collect();
                    bail!("{addr:?} matches several sessions: {}", labels.join(", "));
                }
            }
        }
        Ok(None)
    }

    /// Creates a session in `project` and starts its agent.
    pub fn new_session(&mut self, req: NewSession) -> Result<Session> {
        self.create_session(req, |_| {})
    }

    /// Creates a session and starts it; `customize` adjusts it before it's saved
    /// (adopting sets its conversation and directory).
    pub(crate) fn create_session(
        &mut self,
        req: NewSession,
        customize: impl FnOnce(&mut Session),
    ) -> Result<Session> {
        let project = self.db.project(&req.project)?.with_context(|| {
            format!(
                "no project {:?} (add it with `roster project add`)",
                req.project
            )
        })?;
        let agent = req
            .agent
            .or_else(|| project.agent.clone())
            .unwrap_or_else(|| self.config.default_agent().to_string());
        let adapter = agent::known(&agent, &self.config)?;
        if let Some(effort) = &req.effort {
            agent::check_effort(&agent, effort)?;
        }
        let taken = self.db.live_names(project.id)?;
        let name = match req.name.as_deref() {
            Some(name) => {
                validate_name("session", name)?;
                if taken.iter().any(|t| t == name) {
                    bail!(
                        "project {:?} already has a session named {name:?}",
                        project.name
                    );
                }
                name.to_string()
            }
            None => (1..)
                .map(|i| format!("{agent}-{i}"))
                .find(|n| !taken.contains(n))
                .expect("unbounded"),
        };
        // Only for agents that take them; else the agent's in config.toml.
        let (model, effort) = if adapter.choices().is_some() {
            let chosen = self.config.agent(&agent);
            (req.model.or(chosen.model), req.effort.or(chosen.effort))
        } else {
            (None, None)
        };
        let prompt = req
            .prompt
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        // One its command line can't take: no session that can't start.
        if let Some(p) = prompt.as_ref().filter(|_| adapter.takes_prompt()) {
            check_prompt_fits(&agent, p)?;
        }
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let mut s = Session::new(&id, &project, &name, &agent, now_ms());
        s.model = model;
        s.effort = effort;
        s.pending_prompt = prompt;
        customize(&mut s);
        self.db.insert_session(&s)?;
        self.start(&s.id)?;
        self.db.session(&s.id)
    }

    /// Starts a stopped session: sources the env files and launches its agent,
    /// resuming the conversation if it has one.
    pub fn start(&mut self, id: &str) -> Result<()> {
        // A first prompt this roster gave up on is tried again once started.
        self.prompt_tries.remove(id);
        self.refresh()?;
        let mut s = self.live_session(id)?;
        let before = s.clone();
        if s.state != State::Stopped {
            bail!("{} is already running", s.label());
        }
        let cx = agent::Context {
            roster: &self.exe,
            state_dir: &self.paths.state_dir,
        };
        let launch = agent::launch(&s, &self.config, &cx)?;
        // Too long, it stays, and isn't lost to a failing start.
        if let Some(text) = &launch.prompt {
            check_prompt_fits(&s.agent, text)?;
        }
        let prompt = launch.prompt.as_ref().map(|_| self.prompt_file(&s.id));
        let argv = self.wrapped(&s, prompt.as_deref(), &launch.argv)?;
        // What an agent's hooks need to report (`roster report`).
        let env = [
            ("ROSTER_SESSION", s.id.clone()),
            ("ROSTER_DB", self.paths.db().to_string_lossy().into_owned()),
            ("ROSTER_EXE", self.exe.to_string_lossy().into_owned()),
        ];
        let name = s.tmux_name();
        // No panes, no server: tmux exits with its last session (unless the
        // user's tmux.conf turns exit-empty off; checking then finds nothing).
        let (has_pane, no_server) = {
            let panes = self.panes()?;
            (panes.contains_key(&name), panes.is_empty())
        };
        self.sweep_prompts(&s.id);
        // Written once nothing but starting it is left to fail.
        if let (Some(file), Some(text)) = (&prompt, &launch.prompt) {
            self.write_prompt(file, text)?;
        }
        let started = if has_pane {
            self.tmux.respawn(&name, &s.workdir, &env, &argv)
        } else {
            let (w, h) = util::terminal_size().unwrap_or(DEFAULT_SIZE);
            // A row less for the status line, so attaching doesn't resize it.
            let size = (w, h - u16::from(self.config.status_line() && h > 1));
            self.tmux.new_session(&name, &s.workdir, size, &env, &argv)
        };
        // Not started: no script to read the prompt's file and remove it.
        if let Err(e) = started {
            if let Some(file) = &prompt {
                let _ = fs::remove_file(file);
            }
            return Err(e);
        }
        // The session runs either way, so what this finds is only said.
        if !has_pane
            && no_server
            && let Some(e) = self.tmux.check_config()
        {
            self.notices.push(config_notice(&e));
        }
        let now = now_ms();
        if launch.conversation_id.is_some() {
            s.conversation_id = launch.conversation_id;
        }
        // What it runs with now, until its agent says: what this start asked
        // for, a model as it was learned to be, else as asked (not unknown,
        // which the models of its old reports would fill in). With no model
        // asked for, a resumed conversation keeps its own, so the one it
        // reported stays; a new one reports its own as it starts.
        if let Some(model) = &s.model {
            let learned = format!("model:{}", agent::learned_key(&s.agent, model));
            s.running_model = Some(self.db.meta(&learned)?.unwrap_or_else(|| model.clone()));
        }
        s.running_effort = s.effort.clone();
        // Handed to it on its command line: nothing left to type in.
        if agent::adapter(&s.agent).takes_prompt() {
            s.pending_prompt = None;
        }
        s.set_state(State::Working, now);
        s.unread = false;
        s.exit_status = None;
        s.screen_hash = None;
        s.screen_changed_at = None;
        if agent::adapter(&s.agent).reports() {
            // Counts as a hook event, so the screen isn't consulted right away.
            s.last_event_at = Some(now);
        } else {
            s.last_event_at = None;
            s.baseline_at = Some(now + BOOT_GRACE_MS);
        }
        self.db.save_changes(&before, &s)
    }

    /// What lists each agent's models (`Adapter::models`), to run anywhere,
    /// a thread included (see `asker`).
    pub fn model_lister(&self) -> impl Fn(&str) -> Vec<agent::Model> + Send + 'static {
        let config = self.agents_config();
        let asker = self.asker();
        move |name: &str| agent::adapter(name).models(&config, &|q| asker.run(name, q, None))
    }

    /// What asks every agent that can be started what its account has used
    /// of its limits (`Adapter::usage`), all at once, to run anywhere, a
    /// thread included (see `asker`): those that say, in the order agents are
    /// offered (the default first). The env file is sourced once for all of
    /// them, as the first asks: they're asked every few minutes, and an env
    /// file may ask a password manager.
    pub fn usage_reader(
        &self,
    ) -> impl Fn() -> Vec<(String, Result<crate::usage::Usage>)> + Send + 'static {
        let config = self.agents_config();
        let agents = agent::available(&self.config);
        let asker = self.asker();
        // Nothing is asked with account_limits off, whoever calls this.
        let allowed = self.config.account_limits();
        move || {
            if !allowed {
                return Vec::new();
            }
            // Sourced as the first agent asks; what it started ends with
            // the round, as this goes.
            let env: std::sync::OnceLock<std::result::Result<launch::Environment, String>> =
                std::sync::OnceLock::new();
            let ask = |name: &str, question: agent::Question<'_>| match env
                .get_or_init(|| asker.environment().map_err(|e| format!("{e:#}")))
            {
                Ok(env) => asker.run(name, question, Some(env)),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            };
            std::thread::scope(|scope| {
                let asking: Vec<_> = agents
                    .iter()
                    .map(|name| {
                        let (config, ask) = (&config, &ask);
                        let said = scope
                            .spawn(move || agent::adapter(name).usage(config, &|q| ask(name, q)));
                        (name, said)
                    })
                    .collect();
                let mut answers: Vec<_> = asking
                    .into_iter()
                    .filter_map(|(name, said)| asked(name, said.join()))
                    .collect();
                // The env file failing is said as that, not as what each
                // agent couldn't do for it.
                if let Some(Err(e)) = env.get() {
                    for (_, said) in &mut answers {
                        if said.is_err() {
                            *said = Err(anyhow::anyhow!("{e}"));
                        }
                    }
                }
                answers
            })
        }
    }

    /// The config's agents alone, for a thread.
    fn agents_config(&self) -> Config {
        Config {
            agents: self.config.agents.clone(),
            ..Config::default()
        }
    }

    /// What runs agents' commands as their sessions would (`Asker`).
    fn asker(&self) -> Asker {
        Asker {
            shell: launch::shell(&self.config, std::env::var("SHELL").ok().as_deref()),
            global_env: self.paths.global_env(),
            dir: self.paths.state_dir.clone(),
        }
    }

    /// `argv` run by roster's shell for `s`, after it sources the env files:
    /// the global one, then that of `s`'s project.
    pub(crate) fn wrapped(
        &self,
        s: &Session,
        prompt: Option<&Path>,
        argv: &[String],
    ) -> Result<Vec<String>> {
        let project = self
            .db
            .projects()?
            .into_iter()
            .find(|p| p.id == s.project_id);
        let project_env = project.map_or_else(PathBuf::new, |p| p.origin.join(PROJECT_ENV));
        let shell = launch::shell(&self.config, std::env::var("SHELL").ok().as_deref());
        Ok(launch::wrap(
            &shell,
            &self.paths.global_env(),
            &project_env,
            prompt,
            argv,
        ))
    }

    /// Where session `id`'s first prompt waits for the launch script to give
    /// it to the agent (and remove it).
    fn prompt_file(&self, id: &str) -> PathBuf {
        let name = format!("{PROMPT_FILE}{id}.{}", std::process::id());
        self.paths.state_dir.join(name)
    }

    /// Removes the first prompts' files a launch script never got to
    /// (`sweep_prompts`).
    fn sweep_prompts(&self, id: &str) {
        sweep_prompts(&self.paths.state_dir, id);
    }

    /// Writes `text` into `file`, only the user's to read; nothing is left
    /// of one that fails.
    fn write_prompt(&self, file: &Path, text: &str) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let written = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        if let Err(e) = written {
            let _ = fs::remove_file(file);
            return Err(e).with_context(|| format!("can't write {}", file.display()));
        }
        Ok(())
    }

    /// Session `id`, unless it's archived.
    fn live_session(&self, id: &str) -> Result<Session> {
        let s = self.db.session(id)?;
        if s.deleted_at.is_some() {
            bail!("{} is archived; restore it first", s.label());
        }
        Ok(s)
    }

    /// Stops a session's agent (SIGTERM, then SIGKILL), keeping its entry and last
    /// screen. Returns false if it wasn't running.
    pub fn stop(&mut self, id: &str) -> Result<bool> {
        let name = self.db.session(id)?.tmux_name();
        let Some(pid) = self.live_pid(&name)? else {
            self.refresh()?;
            return Ok(false);
        };
        signal(pid, libc::SIGTERM);
        let deadline = Instant::now() + STOP_TIMEOUT;
        while self.live_pid(&name)?.is_some() {
            if Instant::now() > deadline {
                signal(pid, libc::SIGKILL);
                sleep(Duration::from_millis(200));
                break;
            }
            sleep(Duration::from_millis(100));
        }
        self.refresh()?;
        // The exit code of a stop the user asked for (143 after SIGTERM) means nothing.
        let mut s = self.db.session(id)?;
        if s.state == State::Stopped && s.exit_status.is_some() {
            let before = s.clone();
            s.exit_status = None;
            self.db.save_changes(&before, &s)?;
        }
        Ok(true)
    }

    pub fn restart(&mut self, id: &str) -> Result<()> {
        self.stop(id)?;
        self.start(id)
    }

    /// Archives a session: stops it and puts it away, keeping its conversation ID
    /// so `restore` can bring it back.
    pub fn archive(&mut self, id: &str) -> Result<()> {
        self.stop(id)?;
        let mut s = self.db.session(id)?;
        let before = s.clone();
        self.tmux.kill_session(&s.tmux_name())?;
        s.deleted_at = Some(now_ms());
        s.set_state(State::Stopped, now_ms());
        self.db.save_changes(&before, &s)
    }

    /// Deletes an archived session for good: roster forgets it and its events.
    /// The conversation stays where the agent keeps it (a Claude one can be
    /// adopted again).
    pub fn delete(&mut self, id: &str) -> Result<Session> {
        self.db.fold_events(now_ms())?;
        let s = self.db.session(id)?;
        if s.deleted_at.is_none() {
            bail!("{} isn't archived; archive it first", s.label());
        }
        self.tmux.kill_session(&s.tmux_name())?;
        self.db.delete_session(id)?;
        Ok(s)
    }

    /// Brings back an archived session (stopped), optionally under a new name.
    pub fn restore(&mut self, id: &str, new_name: Option<&str>) -> Result<Session> {
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
        let before = s.clone();
        if s.deleted_at.is_none() {
            bail!("{} isn't archived", s.label());
        }
        if let Some(name) = new_name {
            validate_name("session", name)?;
            s.name = name.to_string();
        }
        if self.db.name_taken(s.project_id, &s.name, None)? {
            bail!(
                "project {:?} already has a session named {:?}; restore with --name",
                s.project,
                s.name
            );
        }
        s.deleted_at = None;
        s.set_state(State::Stopped, now_ms());
        s.unread = false;
        self.db.save_changes(&before, &s)?;
        Ok(s)
    }

    /// Attaches the terminal to a session, starting it first if it's stopped, and
    /// marks it read once the user detaches.
    /// Returns whether you submitted a prompt while attached (for agents that
    /// report prompts: Claude, pi).
    pub fn attach(&mut self, id: &str) -> Result<bool> {
        let since = self.attach_terminal(id)?;
        self.detached(id, since)
    }

    /// `attach` up to the user detaching; returns when it attached, for
    /// `detached`. Apart so the TUI can draw itself again in between.
    pub fn attach_terminal(&mut self, id: &str) -> Result<i64> {
        self.refresh()?;
        let s = self.live_session(id)?;
        if s.state == State::Stopped {
            self.start(id)?;
        }
        // A first prompt still to go in: nothing refreshes while you're
        // attached, so a roster of its own types it in once the agent is ready.
        if self.db.session(id)?.pending_prompt.is_some() {
            self.deliver_in_background(id);
        }
        // The status line says where you are, drawn like the TUI's header. Only
        // a nicety: if tmux won't take it, attach anyway.
        if self.config.status_line() {
            let (left, right) = crate::tui::status_line(&s, &self.config.keys.detach);
            self.tmux.label(&s.tmux_name(), &left, &right).ok();
        }
        let since = now_ms();
        let status = self.tmux.attach(&s.tmux_name())?;
        if !status.success() {
            bail!("couldn't attach to {}", s.label());
        }
        Ok(since)
    }

    /// After detaching from `id`, attached `since`: whatever happened while
    /// attached counts as seen. Whether a prompt was submitted meanwhile.
    pub fn detached(&mut self, id: &str, since: i64) -> Result<bool> {
        self.refresh()?;
        self.set_unread(id, false)?;
        self.db.prompted_since(id, since)
    }

    /// Renames a session, sets its title, or changes its model and effort;
    /// each None stays as it is, so what another roster changed meanwhile
    /// does too. `title`: Some(None) follows the agent's title again. Model
    /// and effort: Some(None) is the default, the agent's in config.toml as
    /// for a new session, else its own; the agent picks those up on its next
    /// start.
    pub fn update_session(
        &mut self,
        id: &str,
        name: Option<&str>,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        effort: Option<Option<String>>,
    ) -> Result<Session> {
        if let Some(name) = name {
            validate_name("session", name)?;
        }
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
        let before = s.clone();
        if let Some(Some(effort)) = &effort {
            agent::check_effort(&s.agent, effort)?;
        }
        if let Some(name) = name {
            if self.db.name_taken(s.project_id, name, Some(&s.id))? {
                bail!(
                    "project {:?} already has a session named {name:?}",
                    s.project
                );
            }
            s.name = name.to_string();
        }
        if let Some(title) = title {
            s.title_override = title
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
        }
        if agent::adapter(&s.agent).choices().is_some() {
            let chosen = self.config.agent(&s.agent);
            if let Some(model) = model {
                s.model = model.or(chosen.model);
            }
            if let Some(effort) = effort {
                s.effort = effort.or(chosen.effort);
            }
        }
        self.db.save_changes(&before, &s)?;
        Ok(s)
    }

    /// Saves a session's or a project's note from the TUI's editor, opened
    /// on note `base` (nothing but blanks removes it): unless `force`d, not
    /// over another saved meanwhile. The note as saved.
    pub fn save_note(
        &self,
        whose: &Whose,
        text: &str,
        pin: Option<usize>,
        base: Option<&Note>,
        force: bool,
    ) -> Result<Option<Note>> {
        let note = new_note(text, pin);
        if force {
            self.db.set_note(whose, note.as_ref())?;
        } else if !self.db.set_note_if(whose, base, note.as_ref())? {
            bail!(
                "The note changed since you opened it (roster note, say): :w! saves yours over it"
            );
        }
        Ok(note)
    }

    /// Saves `text` as the note `n`, edited from what it was, so its pin
    /// follows its line. What happened, to say.
    pub fn change_note(&self, n: &Noted, text: &str) -> Result<String> {
        let (old, pin) = n
            .note
            .as_ref()
            .map_or(("", None), |n| (n.text.as_str(), n.pin));
        if note::tidy(text) == old {
            return Ok(format!("The note of {} is unchanged", n.label));
        }
        let before: Vec<&str> = old.lines().collect();
        let after: Vec<&str> = text.lines().collect();
        let note = new_note(text, note::follow_pin(&before, &after, pin));
        // Not over one saved meanwhile (another editor, an agent through
        // `roster note`), which yours was never made from.
        if !self
            .db
            .set_note_if(&n.whose, n.note.as_ref(), note.as_ref())?
        {
            bail!(
                "the note of {} changed meanwhile, so yours isn't saved over it",
                n.label
            );
        }
        Ok(match note {
            Some(_) => format!("Saved the note of {}", n.label),
            None => format!("{} has no note now", n.label),
        })
    }

    /// Pins line `line` (from 1) of the note `n`, or with None unpins it.
    /// What happened, to say.
    pub fn pin_note(&self, n: &Noted, line: Option<usize>) -> Result<String> {
        let Some(note) = &n.note else {
            bail!("{} has no note", n.label);
        };
        let lines: Vec<&str> = note.text.lines().collect();
        let label = &n.label;
        let pin = match line {
            None => None,
            Some(line) if (1..=lines.len()).contains(&line) => Some(line - 1),
            Some(line) => bail!(
                "the note of {label} has {} lines, so no line {line}",
                lines.len()
            ),
        };
        // Over the note it was asked of, not one saved meanwhile.
        if !self
            .db
            .set_note_if(&n.whose, Some(note), new_note(&note.text, pin).as_ref())?
        {
            bail!("the note of {label} changed meanwhile; nothing pinned");
        }
        Ok(match pin {
            Some(p) => format!("Pinned line {} of the note of {label}: {}", p + 1, lines[p]),
            None => format!("Unpinned the note of {label}"),
        })
    }

    /// Where `$EDITOR` edits a note: named after whose it is, so the editor's
    /// title says so, and its id, which only it has (`a-b/c` and `a/b-c` name
    /// alike); each editing makes its own file next to that
    /// (`note::edit_externally`).
    pub fn note_file(&self, n: &Noted) -> PathBuf {
        let id = match &n.whose {
            Whose::Session(id) => id.clone(),
            Whose::Project(id) => id.to_string(),
        };
        let name: String = n
            .label
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
                _ => '-',
            })
            .collect();
        self.paths
            .state_dir
            .join("notes")
            .join(format!("{name}-{id}.md"))
    }

    /// What the session's agent can tell about it, from its transcript; None
    /// if it can't.
    pub fn agent_stats(&self, s: &Session) -> Option<AgentStats> {
        let path = Path::new(s.transcript_path.as_deref()?);
        let mut transcript = agent::transcript_of(s, path)?;
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

    /// Waits (refreshing) until a session's first prompt has been typed in, for
    /// one-shot commands. False if the agent wasn't ready within `timeout`, or
    /// stopped or was archived first; an error if typing it in failed for good,
    /// or failed and time ran out.
    pub fn wait_for_prompt(&mut self, id: &str, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut last_failure = None;
        loop {
            self.refresh()?;
            // Its own; others' stay for whoever asks.
            let failed = (self.undelivered.iter())
                .position(|u| u.id == id)
                .map(|i| self.undelivered.remove(i));
            let s = self.db.session(id)?;
            match failed {
                // Not typed in, and not to be tried again by this roster.
                Some(u) if !u.again => bail!("{}", u.message),
                Some(u) => last_failure = Some(u.message),
                None => {}
            }
            if s.pending_prompt.is_none() {
                return Ok(true);
            }
            // Its agent gone (exited, archived): nothing to type it into.
            if s.state == State::Stopped || s.deleted_at.is_some() {
                return Ok(false);
            }
            if Instant::now() > deadline {
                return match last_failure {
                    Some(message) => Err(anyhow::anyhow!(message)),
                    None => Ok(false),
                };
            }
            sleep(Duration::from_millis(500));
        }
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

    /// Whether starting `s` again resumes its conversation; if not, the CLI and
    /// TUI ask first.
    pub fn can_resume(&self, s: &Session) -> bool {
        agent::resumes(s)
    }

    /// Whether an agent is waiting for its first prompt: for one with hooks,
    /// after it has reported its session, which names its transcript (a new
    /// conversation has none before that); for others, idle after starting up.
    /// (An agent that takes its first prompt on its command line never waits.)
    fn ready_for_prompt(&self, s: &Session, now: i64) -> bool {
        s.state == State::Idle
            && if agent::adapter(&s.agent).reports() {
                s.transcript_path.is_some()
            } else {
                s.baseline_at.is_some_and(|b| now > b)
            }
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

/// Fills in the models of sessions that predate recording them: from recent hook
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

/// Default agent, model and effort for a project's new sessions.
#[derive(Clone, Debug, Default)]
pub struct Defaults {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// What a new session should be; the agent defaults to the project's.
#[derive(Clone, Debug, Default)]
pub struct NewSession {
    pub project: String,
    pub name: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Typed into the agent once it's ready.
    pub prompt: Option<String>,
}

fn signal(pid: i32, sig: libc::c_int) {
    // SAFETY: kill(2) has no memory effects; a stale pid just fails.
    unsafe {
        libc::kill(pid, sig);
    }
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

fn validate_name(what: &str, name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        bail!("{what} name {name:?} may only contain letters, digits, '-', '_' and '.'");
    }
    Ok(())
}

/// What runs an agent's commands (`agent::Ask`) as its sessions would, to
/// run anywhere, a thread included: the global env file sourced (in roster's
/// state directory, away from any project's settings), each for at most 10
/// seconds.
struct Asker {
    shell: String,
    global_env: PathBuf,
    dir: PathBuf,
}

impl Asker {
    /// Runs `question` for agent `name` and returns what it printed: in
    /// `env`, else with the env file sourced for it alone.
    fn run(
        &self,
        name: &str,
        question: agent::Question<'_>,
        env: Option<&launch::Environment>,
    ) -> Result<String> {
        let printed = self.printed(name, question, env)?;
        Ok(String::from_utf8_lossy(&printed).into_owned())
    }

    fn printed(
        &self,
        name: &str,
        question: agent::Question<'_>,
        env: Option<&launch::Environment>,
    ) -> Result<Vec<u8>> {
        let mut cmd = match env {
            Some(env) => {
                let mut cmd = std::process::Command::new(&question.argv[0]);
                cmd.args(&question.argv[1..])
                    .env_clear()
                    .envs(env.vars.iter().map(|(k, v)| (k, v)));
                cmd
            }
            None => {
                let argv = launch::wrap(
                    &self.shell,
                    &self.global_env,
                    Path::new(""),
                    None,
                    &question.argv,
                );
                let mut cmd = std::process::Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                for var in crate::tmux::LEAKY_ENV {
                    cmd.env_remove(var);
                }
                cmd
            }
        };
        cmd.current_dir(&self.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = launch::converse(cmd, name, Duration::from_secs(10), question.input)?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            match said.lines().rfind(|l| !l.trim().is_empty()) {
                Some(line) => bail!("{}", line.trim()),
                None => bail!("{} exited with {}", question.argv[0], output.status),
            }
        }
        Ok(output.stdout)
    }

    /// The variables the env file makes, for several commands to run in, so
    /// it's sourced once for all of them (`launch::environment`).
    fn environment(&self) -> Result<launch::Environment> {
        launch::environment(
            &self.shell,
            &self.global_env,
            &self.dir,
            Duration::from_secs(10),
        )
    }
}

/// What asking agent `name` what its account has used came to: its answer;
/// none without an account; or, where asking it failed on a mistake of
/// roster's (its thread panicked), that, rather than seeming to have none.
fn asked(
    name: &str,
    joined: std::thread::Result<Option<Result<crate::usage::Usage>>>,
) -> Option<(String, Result<crate::usage::Usage>)> {
    let said = match joined {
        Ok(said) => said?,
        Err(panic) => Err(anyhow::anyhow!(
            "roster failed asking it: {}",
            util::panic_message(&*panic)
        )),
    };
    Some((name.to_string(), said))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompts_file_is_swept_once_whoever_wrote_it_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mut done = std::process::Command::new("true").spawn().unwrap();
        done.wait().unwrap();
        let file = |id: &str, pid: u32| dir.path().join(format!("{PROMPT_FILE}{id}.{pid}"));
        let (mine, dead, live) = (
            file("s1", std::process::id()),
            file("s1", done.id()),
            file("s1", 1),
        );
        let other = file("s2", done.id());
        for f in [&mine, &dead, &live, &other] {
            fs::write(f, "fix it").unwrap();
        }
        sweep_prompts(dir.path(), "s1");
        // This roster's and a gone one's go; a roster starting it now keeps
        // its own, and another session's, not an hour old, stays.
        let left: Vec<bool> = [&mine, &dead, &live, &other].map(|f| f.exists()).into();
        assert_eq!(left, [false, false, true, true]);
    }

    #[test]
    fn a_first_prompt_fits_a_command_line_up_to_linuxs_longest_argument() {
        assert!(check_prompt_fits("codex", &"x".repeat(LONGEST_ARGUMENT)).is_ok());
        let e = check_prompt_fits("codex", &"x".repeat(200_000)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "the first prompt is too long for codex's command line: 200000 bytes, where it takes 131071 at most"
        );
        assert!(check_prompt_fits("codex", &"x".repeat(LONGEST_ARGUMENT + 1)).is_err());
    }

    #[test]
    fn asking_that_fails_on_a_mistake_of_rosters_says_so() {
        // A panic in an agent's asking: an answer that says so.
        let panicked = std::thread::spawn(|| -> Option<Result<crate::usage::Usage>> {
            panic!("attempt to multiply with overflow")
        })
        .join();
        let (agent, said) = asked("claude", panicked).unwrap();
        assert_eq!(agent, "claude");
        assert_eq!(
            said.unwrap_err().to_string(),
            "roster failed asking it: attempt to multiply with overflow"
        );
        // No account: no answer.
        assert!(asked("pi", Ok(None)).is_none());
    }

    #[test]
    fn names_are_simple() {
        assert!(validate_name("session", "api-fix_2.0").is_ok());
        assert!(validate_name("session", "").is_err());
        assert!(validate_name("session", "a/b").is_err());
        assert!(validate_name("session", "has space").is_err());
    }

    #[test]
    fn removing_a_project_says_what_goes_with_it() {
        let p = Project {
            id: 1,
            name: "demo".into(),
            origin: "/tmp/demo".into(),
            agent: None,
            model: None,
            effort: None,
            note: None,
        };
        let session = |noted: bool| {
            let mut s = Session::new("r-1", &p, "one", "claude", 0);
            s.note = noted.then(|| Note {
                text: "todo".into(),
                pin: None,
                at: 0,
            });
            s
        };
        let ask = |notes: &[bool]| {
            let sessions: Vec<Session> = notes.iter().map(|&n| session(n)).collect();
            project_removal_question(&p, &sessions)
        };
        assert_eq!(ask(&[]), "Remove project demo?");
        assert_eq!(
            ask(&[false]),
            "Remove project demo? Its archived session goes for good."
        );
        assert_eq!(
            ask(&[true]),
            "Remove project demo? Its archived session goes for good, with its note."
        );
        assert_eq!(
            ask(&[false, false]),
            "Remove project demo? Its 2 archived sessions go for good."
        );
        assert_eq!(
            ask(&[true, false]),
            "Remove project demo? Its 2 archived sessions go for good, one with a note."
        );
        assert_eq!(
            ask(&[true, true, false]),
            "Remove project demo? Its 3 archived sessions go for good, 2 with notes."
        );
        // Its own note goes too.
        let noted = Project {
            note: session(true).note,
            ..p.clone()
        };
        assert_eq!(
            project_removal_question(&noted, &[session(false)]),
            "Remove project demo and its note? Its archived session goes for good."
        );
    }
}
