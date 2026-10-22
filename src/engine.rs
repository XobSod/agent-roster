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
use crate::tmux::Tmux;
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
}

impl Roster {
    pub fn open() -> Result<Roster> {
        let paths = Paths::from_env()?;
        let config = Config::load(&paths.config_file())?;
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
        })
    }

    /// Brings the database up to date: applies reported events, notices agents
    /// that have exited, and corrects stale "working" states from the screen.
    pub fn refresh(&mut self) -> Result<()> {
        let now = now_ms();
        self.db.fold_events(now)?;
        let panes = self.tmux.panes()?;
        self.resized.retain(|_, until| *until > now);
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
                            self.tmux.type_line(&s.tmux_name(), &prompt)?;
                        }
                        s.pending_prompt = None;
                    }
                }
            }
            if s != before {
                self.db.save_session(&s)?;
            }
        }
        Ok(())
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
    pub fn fit_screens(&mut self, size: (u16, u16), fitted: &mut HashSet<String>) -> Result<()> {
        let panes = self.tmux.panes()?;
        for s in self.db.sessions(false)? {
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
        let agent = defaults.agent.as_deref().unwrap_or(agent::DEFAULT);
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
                [s] => return Ok(s.clone()),
                _ => {
                    let labels: Vec<String> = found
                        .iter()
                        .map(|s| format!("{} ({})", s.label(), s.id))
                        .collect();
                    bail!("{addr:?} matches several sessions: {}", labels.join(", "));
                }
            }
        }
        bail!(
            "no {}session {addr:?}",
            if archived { "archived " } else { "" }
        )
    }

    /// Creates a session in `project` and starts its agent.
    pub fn new_session(&mut self, req: NewSession) -> Result<Session> {
        self.create_session(req, |_| {})
    }

    /// Creates a session and starts it; `customize` adjusts it before it's saved
    /// (adopting sets its conversation and directory).
    fn create_session(
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
            .unwrap_or_else(|| agent::DEFAULT.to_string());
        let adapter = agent::known(&agent, &self.config)?;
        if let Some(effort) = &req.effort {
            agent::check_effort(&agent, effort)?;
        }
        let taken: Vec<String> = self
            .db
            .sessions(false)?
            .into_iter()
            .filter(|s| s.project_id == project.id)
            .map(|s| s.name)
            .collect();
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
        // Only for agents that take them.
        let (model, effort) = if adapter.choices().is_some() {
            (req.model, req.effort)
        } else {
            (None, None)
        };
        let prompt = req
            .prompt
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let mut s = Session {
            id: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
            project_id: project.id,
            project: project.name.clone(),
            name,
            agent: agent.clone(),
            workdir: project.origin.clone(),
            conversation_id: None,
            transcript_path: None,
            state: State::Stopped,
            unread: false,
            exit_status: None,
            last_event_at: None,
            screen_hash: None,
            screen_changed_at: None,
            created_at: now_ms(),
            deleted_at: None,
            baseline_at: None,
            title: None,
            model,
            effort,
            state_since: Some(now_ms()),
            pending_prompt: prompt,
            running_model: None,
            running_effort: None,
            title_override: None,
            turns: 0,
            worked_ms: 0,
            fork_from: None,
        };
        customize(&mut s);
        self.db.insert_session(&s)?;
        self.start(&s.id)?;
        self.db.session(&s.id)
    }

    /// Starts a stopped session: sources the env files and launches its agent,
    /// resuming the conversation if it has one.
    pub fn start(&mut self, id: &str) -> Result<()> {
        self.refresh()?;
        let mut s = self.db.session(id)?;
        if s.deleted_at.is_some() {
            bail!("{} is archived; restore it first", s.label());
        }
        if s.state != State::Stopped {
            bail!("{} is already running", s.label());
        }
        let project = self
            .db
            .projects()?
            .into_iter()
            .find(|p| p.id == s.project_id)
            .with_context(|| format!("{} has no project", s.label()))?;
        let launch = agent::launch(&s, &self.config, &self.exe)?;
        let shell = launch::shell(&self.config, std::env::var("SHELL").ok().as_deref());
        let argv = launch::wrap(
            &shell,
            &self.paths.global_env(),
            &project.origin.join(PROJECT_ENV),
            &launch.argv,
        );
        let env = [
            ("ROSTER_SESSION", s.id.clone()),
            ("ROSTER_DB", self.paths.db().to_string_lossy().into_owned()),
        ];
        let name = s.tmux_name();
        if self.tmux.panes()?.contains_key(&name) {
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
        let taken = self
            .db
            .sessions(false)?
            .iter()
            .any(|o| o.project_id == s.project_id && o.name == s.name);
        if taken {
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
    /// report prompts: Claude).
    pub fn attach(&mut self, id: &str) -> Result<bool> {
        self.refresh()?;
        let s = self.db.session(id)?;
        if s.deleted_at.is_some() {
            bail!("{} is archived; restore it first", s.label());
        }
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
        // Whatever happened while attached counts as seen.
        self.refresh()?;
        self.set_unread(id, false)?;
        self.db.prompted_since(id, since)
    }

    /// Renames a session or changes its model and effort; the agent picks those up
    /// on its next start.
    /// `title`: None follows the agent's title.
    pub fn update_session(
        &mut self,
        id: &str,
        name: &str,
        title: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<Session> {
        validate_name("session", name)?;
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
        if let Some(effort) = &effort {
            agent::check_effort(&s.agent, effort)?;
        }
        let taken = self
            .db
            .sessions(false)?
            .iter()
            .any(|o| o.id != s.id && o.project_id == s.project_id && o.name == name);
        if taken {
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
            s.model = model;
            s.effort = effort;
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
        Some(transcript.stats(s.running_model.as_deref()))
    }

    /// Asks Haiku for a title from the session's recent prompts and keeps it as
    /// the session's title (the agent's own stays the same otherwise).
    pub fn generate_title(&mut self, id: &str) -> Result<String> {
        let mut s = self.db.session(id)?;
        let adapter = agent::adapter(&s.agent);
        // (Asked without a path: whether it has a transcript roster reads at all.)
        if adapter.transcript(Path::new("")).is_none() {
            bail!(
                "{} keeps no transcript roster can read, so it can't get a generated title; set one with edit",
                s.agent
            );
        }
        let path = s.transcript_path.clone().filter(|p| Path::new(p).exists());
        let path =
            path.with_context(|| format!("{} has no conversation to title yet", s.label()))?;
        let mut transcript = adapter
            .transcript(Path::new(&path))
            .context("no transcript")?;
        transcript.update()?;
        let prompts = transcript.recent_prompts();
        if prompts.is_empty() {
            bail!("{} has no prompts to title yet", s.label());
        }
        // Haiku makes the title, whichever agent the session runs.
        let mut argv = agent::claude::oneoff(&self.config);
        argv.push(title_request(&prompts, s.display_title()));
        let project = self
            .db
            .projects()?
            .into_iter()
            .find(|p| p.id == s.project_id);
        let project_env = project.map_or_else(PathBuf::new, |p| p.origin.join(PROJECT_ENV));
        let shell = launch::shell(&self.config, std::env::var("SHELL").ok().as_deref());
        let argv = launch::wrap(&shell, &self.paths.global_env(), &project_env, &argv);
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&s.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for var in crate::tmux::LEAKY_ENV {
            cmd.env_remove(var);
        }
        let output = run_with_timeout(cmd, TITLE_TIMEOUT)?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!(
                "claude couldn't make a title: {}",
                err.lines().next().unwrap_or("no output")
            );
        }
        let title = clean_title(&String::from_utf8_lossy(&output.stdout))
            .context("claude gave no title")?;
        s.title_override = Some(title.clone());
        self.db.save_session(&s)?;
        Ok(title)
    }

    /// Claude conversations roster could take over: not already one of its live
    /// sessions, each with the project whose directory holds it (the deepest
    /// one), if any, and an archived roster session it belongs to, if any. With
    /// `all` false, only those in a project's directory.
    pub fn adoptable(&self, all: bool) -> Result<Vec<Adoptable>> {
        let Some(dir) = crate::adopt::claude_dir() else {
            return Ok(Vec::new());
        };
        let projects = self.db.projects()?;
        let sessions = self.db.sessions(true)?;
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        let mut found = Vec::new();
        for conversation in crate::adopt::conversations(&dir) {
            let known = sessions
                .iter()
                .find(|s| s.conversation_id.as_deref() == Some(conversation.id.as_str()));
            if known.is_some_and(|s| s.deleted_at.is_none()) {
                continue;
            }
            let project = projects
                .iter()
                .filter(|p| conversation.cwd.starts_with(&p.origin))
                .max_by_key(|p| p.origin.components().count())
                .cloned();
            if project.is_none() && !all {
                continue;
            }
            let new_project = new_project_name(&conversation.cwd, &names);
            found.push(Adoptable {
                conversation,
                project,
                new_project,
                archived: known.cloned(),
            });
        }
        Ok(found)
    }

    /// Takes over a Claude conversation started elsewhere: a session (in its
    /// project, or in a new project for its directory) that resumes it, or with
    /// `fork`, a copy of it. A conversation open in Claude is refused unless
    /// `force`: two Claudes would write the same conversation.
    pub fn adopt(&mut self, req: Adopt) -> Result<Session> {
        let candidates = self.adoptable(true)?;
        let matches: Vec<&Adoptable> = candidates
            .iter()
            .filter(|a| {
                a.conversation.id == req.conversation
                    || (req.conversation.len() >= 4
                        && a.conversation.id.starts_with(&req.conversation))
            })
            .collect();
        let found = match matches[..] {
            [one] => one.clone(),
            [] => bail!(
                "no conversation {:?} to adopt (`roster adopt --list --all` shows them)",
                req.conversation
            ),
            _ => bail!(
                "{:?} matches several conversations; give more of the id",
                req.conversation
            ),
        };
        if let Some(s) = &found.archived {
            bail!(
                "that conversation is the archived session {}; restore it instead (`roster restore {}`)",
                s.label(),
                s.id
            );
        }
        let c = &found.conversation;
        if let Some(pid) = c.open_in
            && !req.fork
            && !req.force
        {
            bail!(
                "it's open in Claude (pid {pid}); close it there first, or adopt a copy (--fork), or adopt it anyway (--yes)"
            );
        }
        let project = match (&req.project, &found.project) {
            (Some(name), _) => match self.db.project(name)? {
                Some(p) => p,
                None => self.add_project(name, &c.cwd, Defaults::default())?,
            },
            (None, Some(p)) => p.clone(),
            (None, None) => self.add_project(&found.new_project, &c.cwd, Defaults::default())?,
        };
        let request = NewSession {
            project: project.name.clone(),
            name: req.name,
            agent: Some(agent::CLAUDE.to_string()),
            model: req.model.unwrap_or(project.model.clone()),
            effort: req.effort.unwrap_or(project.effort.clone()),
            prompt: None,
        };
        let fork_from = req.fork.then(|| c.id.clone());
        let (conversation_id, transcript_path) = if req.fork {
            (uuid::Uuid::new_v4().to_string(), None)
        } else {
            (c.id.clone(), Some(c.path.to_string_lossy().into_owned()))
        };
        self.create_session(request, |s| {
            s.workdir = c.cwd.clone();
            s.conversation_id = Some(conversation_id);
            s.transcript_path = transcript_path;
            s.fork_from = fork_from;
            s.title = c.title.clone();
            // A name it was given is like a title of your own.
            s.title_override = c.custom_title.clone();
            // What it last ran on, which resuming keeps, unless another is asked for.
            if s.model.is_none() {
                s.running_model = c.model.clone();
            }
        })
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
    pub fn screen(&self, id: &str, ansi: bool) -> Result<String> {
        let s = self.db.session(id)?;
        let Some(pane) = self.tmux.panes()?.remove(&s.tmux_name()) else {
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

/// How long a title may take.
const TITLE_TIMEOUT: Duration = Duration::from_secs(60);

fn title_request(prompts: &[String], current: &str) -> String {
    let mut request =
        String::from("These are the latest requests in a coding session, oldest first:\n\n");
    for p in prompts {
        let p: String = p.chars().take(300).collect();
        request.push_str(&format!("- {}\n", p.replace('\n', " ")));
    }
    request.push_str(&format!(
        "\nIts title so far: {current}\n\nGive a short title (3 to 7 words) for what the session is working on now. Reply with the title only."
    ));
    request
}

/// The first line of the reply, without quotes or markdown, at most 80 characters.
fn clean_title(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '#' | ' '));
    let title: String = line.chars().take(80).collect();
    (!title.is_empty()).then_some(title)
}

fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut child = cmd.spawn().context("can't run claude")?;
    let deadline = Instant::now() + timeout;
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("claude took longer than {}s", timeout.as_secs());
        }
        sleep(Duration::from_millis(100));
    }
    Ok(child.wait_with_output()?)
}

/// A conversation roster could adopt.
#[derive(Clone, Debug)]
pub struct Adoptable {
    pub conversation: crate::adopt::Conversation,
    /// The project whose directory holds it, if any.
    pub project: Option<Project>,
    /// Otherwise, the name a new project for its directory gets.
    pub new_project: String,
    /// An archived roster session it belongs to: restore that instead.
    pub archived: Option<Session>,
}

/// What to adopt, and how.
#[derive(Clone, Debug, Default)]
pub struct Adopt {
    /// The conversation's id, or a prefix of 4+ characters.
    pub conversation: String,
    /// A project to put it in, created for its directory if it doesn't exist;
    /// default: the project holding its directory, else a new one named after it.
    pub project: Option<String>,
    pub name: Option<String>,
    /// None: the project's default; Some(None): Claude's own.
    pub model: Option<Option<String>>,
    pub effort: Option<Option<String>>,
    /// Adopt a copy (`--fork-session`) and leave the original alone.
    pub fork: bool,
    /// Adopt it even though it's open in Claude.
    pub force: bool,
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
fn new_project_name(directory: &Path, taken: &[&str]) -> String {
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
    fn titles_are_one_clean_line() {
        assert_eq!(
            clean_title("\n  \"Fix Login Tests\"  \nmore").as_deref(),
            Some("Fix Login Tests")
        );
        assert_eq!(
            clean_title("# **Retry uploads**").as_deref(),
            Some("Retry uploads")
        );
        assert_eq!(clean_title("  \n "), None);
        let request = title_request(&["fix\nthe tests".into()], "Old");
        assert!(request.contains("- fix the tests") && request.contains("Its title so far: Old"));
    }

    #[test]
    fn names_are_simple() {
        assert!(validate_name("session", "api-fix_2.0").is_ok());
        assert!(validate_name("session", "").is_err());
        assert!(validate_name("session", "a/b").is_err());
        assert!(validate_name("session", "has space").is_err());
    }
}
