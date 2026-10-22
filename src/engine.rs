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
use crate::paths::{self, PROJECT_ENV, Paths};
use crate::stats::AgentStats;
use crate::tmux::{Pane, Tmux};
use crate::util::{self, now_ms};
use crate::{agent, launch};

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
}

impl Roster {
    pub fn open() -> Result<Roster> {
        let paths = Paths::from_env()?;
        let (config, config_text) = Config::load_text(&paths.config_file())?;
        fs::create_dir_all(&paths.state_dir)
            .with_context(|| format!("can't create {}", paths.state_dir.display()))?;
        let db = Db::open(&paths.db())?;
        backfill_models(&db)?;
        let tmux = Tmux::new(paths::tmux_socket(), paths.tmux_conf());
        // A changed detach key or user tmux.conf also applies to a running server.
        if tmux.write_config(
            &config.keys.detach,
            &paths.user_tmux_conf(),
            config.status_line(),
        )? {
            tmux.reload_config()?;
        }
        let exe = std::env::current_exe().context("can't find roster's own path")?;
        Ok(Roster {
            paths,
            config,
            db,
            tmux,
            exe,
            resized: HashMap::new(),
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
        // Typed once every session is saved: typing takes a moment, and rows
        // loaded before it could overwrite what another roster saved meanwhile.
        let mut prompts = Vec::new();
        let mut titles = Vec::new();
        for mut s in self.db.sessions(false)? {
            let before = s.clone();
            let redrawing = self.resized.contains_key(&s.id);
            match panes.get(&s.tmux_name()) {
                None => s.set_state(State::Stopped, now),
                Some(pane) if pane.dead => {
                    // Record the exit code when it stops, not on every pass, so
                    // `stop` can clear it.
                    if s.state != State::Stopped {
                        s.set_state(State::Stopped, now);
                        s.exit_status = pane.dead_status;
                    }
                }
                Some(pane) => {
                    // Running although recorded as stopped: idle until hooks say otherwise.
                    if s.state == State::Stopped {
                        s.set_state(State::Idle, now);
                    }
                    let adapter = agent::adapter(&s.agent);
                    if let Some(title) = adapter.title(&pane.title) {
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
                        // Claimed in the database first, so only one process types it.
                        if let Some(prompt) = self.db.take_pending_prompt(&s.id)? {
                            prompts.push((s.tmux_name(), prompt));
                        }
                        s.pending_prompt = None;
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
                self.db.save_session(&s)?;
            }
        }
        self.panes = Some(panes);
        for id in titles {
            self.auto_title_in_background(&id);
        }
        for (name, prompt) in prompts {
            self.tmux.type_line(&name, &prompt)?;
        }
        Ok(())
    }

    /// Makes `id`'s automatic title in a roster process of its own: it takes
    /// a model call, which nothing should wait for. A failure only means no
    /// title (`s t` asks again, and says why).
    fn auto_title_in_background(&self, id: &str) {
        let child = std::process::Command::new(&self.exe)
            .args(["retitle", "--auto", id])
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
        let project = self.checked_project(id, name, path, defaults)?;
        if self.chat_project_id()? == Some(id) {
            let current = self.db.projects()?.into_iter().find(|p| p.id == id);
            if current.is_some_and(|p| p.origin != project.origin) {
                bail!("the chats project's directory is set by `chats_dir` in config.toml");
            }
        }
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
        self.refresh()?;
        let mut s = self.live_session(id)?;
        if s.state != State::Stopped {
            bail!("{} is already running", s.label());
        }
        let cx = agent::Context {
            roster: &self.exe,
            state_dir: &self.paths.state_dir,
        };
        let launch = agent::launch(&s, &self.config, &cx)?;
        let argv = self.wrapped(&s, &launch.argv)?;
        // What an agent's hooks need to report (`roster report`).
        let env = [
            ("ROSTER_SESSION", s.id.clone()),
            ("ROSTER_DB", self.paths.db().to_string_lossy().into_owned()),
            ("ROSTER_EXE", self.exe.to_string_lossy().into_owned()),
        ];
        let name = s.tmux_name();
        if self.panes()?.contains_key(&name) {
            self.tmux.respawn(&name, &s.workdir, &env, &argv)?;
        } else {
            let (w, h) = util::terminal_size().unwrap_or(DEFAULT_SIZE);
            // A row less for the status line, so attaching doesn't resize it.
            let size = (w, h - u16::from(self.config.status_line() && h > 1));
            self.tmux
                .new_session(&name, &s.workdir, size, &env, &argv)?;
        }
        let now = now_ms();
        if launch.conversation_id.is_some() {
            s.conversation_id = launch.conversation_id;
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
        self.db.save_session(&s)
    }

    /// What lists each agent's models (`Adapter::models`), to run anywhere,
    /// a thread included: an agent's own command runs as its sessions would,
    /// the global env file sourced (in roster's state directory, away from
    /// any project's settings), for at most 10 seconds.
    pub fn model_lister(&self) -> impl Fn(&str) -> Vec<agent::Model> + Send + 'static {
        let config = Config {
            agents: self.config.agents.clone(),
            ..Config::default()
        };
        let shell = launch::shell(&self.config, std::env::var("SHELL").ok().as_deref());
        let global_env = self.paths.global_env();
        let dir = self.paths.state_dir.clone();
        move |name: &str| {
            let ask = |argv: &[String]| -> Option<String> {
                let argv = launch::wrap(&shell, &global_env, Path::new(""), argv);
                let mut cmd = std::process::Command::new(&argv[0]);
                cmd.args(&argv[1..])
                    .current_dir(&dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null());
                for var in crate::tmux::LEAKY_ENV {
                    cmd.env_remove(var);
                }
                let output = launch::run_with_timeout(cmd, name, Duration::from_secs(10)).ok()?;
                output
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
            };
            agent::adapter(name).models(&config, &ask)
        }
    }

    /// `argv` run by roster's shell for `s`, after it sources the env files:
    /// the global one, then that of `s`'s project.
    pub(crate) fn wrapped(&self, s: &Session, argv: &[String]) -> Result<Vec<String>> {
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
            argv,
        ))
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
            s.exit_status = None;
            self.db.save_session(&s)?;
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
        self.tmux.kill_session(&s.tmux_name())?;
        s.deleted_at = Some(now_ms());
        s.set_state(State::Stopped, now_ms());
        self.db.save_session(&s)
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
        self.db.save_session(&s)?;
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

    /// Renames a session or changes its model and effort (None: as they
    /// are; Some(None): the default, the agent's in config.toml as for a new
    /// session, else its own); the agent picks those up on its next start.
    /// `title`: None follows the agent's title.
    pub fn update_session(
        &mut self,
        id: &str,
        name: &str,
        title: Option<String>,
        model: Option<Option<String>>,
        effort: Option<Option<String>>,
    ) -> Result<Session> {
        validate_name("session", name)?;
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
        if let Some(Some(effort)) = &effort {
            agent::check_effort(&s.agent, effort)?;
        }
        if self.db.name_taken(s.project_id, name, Some(&s.id))? {
            bail!(
                "project {:?} already has a session named {name:?}",
                s.project
            );
        }
        s.name = name.to_string();
        s.title_override = title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        if agent::adapter(&s.agent).choices().is_some() {
            let chosen = self.config.agent(&s.agent);
            if let Some(model) = model {
                s.model = model.or(chosen.model);
            }
            if let Some(effort) = effort {
                s.effort = effort.or(chosen.effort);
            }
        }
        self.db.save_session(&s)?;
        Ok(s)
    }

    /// What the session's agent can tell about it, from its transcript; None
    /// if it can't.
    pub fn agent_stats(&self, s: &Session) -> Option<AgentStats> {
        let path = Path::new(s.transcript_path.as_deref()?);
        let mut transcript = agent::adapter(&s.agent).transcript(path)?;
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
    /// one-shot commands. False if the agent wasn't ready within `timeout`.
    pub fn wait_for_prompt(&mut self, id: &str, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            self.refresh()?;
            if self.db.session(id)?.pending_prompt.is_none() {
                return Ok(true);
            }
            if Instant::now() > deadline {
                return Ok(false);
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
        if unread {
            s.unread = true;
        } else {
            let hash = self.screen_hash(&s);
            s.mark_seen(hash, now_ms());
        }
        self.db.save_session(&s)
    }

    /// Whether starting `s` again resumes its conversation; if not, the CLI and
    /// TUI ask first.
    pub fn can_resume(&self, s: &Session) -> bool {
        agent::adapter(&s.agent).resumes()
    }

    /// Whether an agent is waiting for its first prompt: for one with hooks,
    /// after it has reported its session, which names its transcript (a new
    /// conversation has none before that); for others, idle after starting up.
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
/// events, else from the end of their Claude transcript. Then learns what each
/// alias turned out to be.
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
            s.running_model = Some(model);
            db.save_session(&s)?;
        }
    }
    db.learn_model_names()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_simple() {
        assert!(validate_name("session", "api-fix_2.0").is_ok());
        assert!(validate_name("session", "").is_err());
        assert!(validate_name("session", "a/b").is_err());
        assert!(validate_name("session", "has space").is_err());
    }
}
