//! The operations behind the CLI and the TUI, so the two stay at parity.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::agent::Kind;
use crate::config::Config;
use crate::db::Db;
use crate::model::{Project, QUIET_MS, Session, State};
use crate::paths::{self, PROJECT_ENV, Paths};
use crate::tmux::Tmux;
use crate::util::{self, now_ms};
use crate::{agent, launch};

/// Window size for new sessions when roster isn't run from a terminal.
const DEFAULT_SIZE: (u16, u16) = (160, 48);
/// How long `stop` waits after SIGTERM before SIGKILL.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Screen changes this soon after a start are the agent starting up, not a turn.
const BOOT_GRACE_MS: i64 = 5000;

pub struct Roster {
    pub paths: Paths,
    pub config: Config,
    pub db: Db,
    pub(crate) tmux: Tmux,
    exe: PathBuf,
}

impl Roster {
    pub fn open() -> Result<Roster> {
        let paths = Paths::from_env()?;
        let config = Config::load(&paths.config_file())?;
        fs::create_dir_all(&paths.state_dir)
            .with_context(|| format!("can't create {}", paths.state_dir.display()))?;
        let db = Db::open(&paths.db())?;
        let tmux = Tmux::new(paths::tmux_socket(), paths.tmux_conf());
        // A changed detach key or user tmux.conf also applies to a running server.
        if tmux.write_config(&config.keys.detach, &paths.user_tmux_conf())? {
            tmux.reload_config()?;
        }
        let exe = std::env::current_exe().context("can't find roster's own path")?;
        Ok(Roster {
            paths,
            config,
            db,
            tmux,
            exe,
        })
    }

    /// Brings the database up to date: applies reported events, notices agents
    /// that have exited, and corrects stale "working" states from the screen.
    pub fn refresh(&mut self) -> Result<()> {
        let now = now_ms();
        self.db.fold_events(now)?;
        let panes = self.tmux.panes()?;
        for mut s in self.db.sessions(false)? {
            let before = s.clone();
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
                    match self.kind(&s) {
                        Kind::Claude => {
                            if let Some(title) = claude_title(&pane.title) {
                                s.title = Some(title);
                            }
                            if s.needs_screen_check(now)
                                && let Some(hash) = self.screen_hash(&s)
                            {
                                s.observe_screen(&hash, now);
                                s.correct_stale_working(now);
                            }
                        }
                        Kind::Generic => {
                            if let Some(hash) = self.screen_hash(&s) {
                                let idle_after = agent::idle_after_ms(&s.agent, &self.config);
                                s.observe_generic(&hash, now, idle_after);
                            }
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
                let watched = match self.kind(s) {
                    Kind::Claude => s.needs_screen_check(now),
                    Kind::Generic => s.state == State::Working,
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
        if let Some(agent) = &defaults.agent {
            agent::kind(agent, &self.config)?;
        }
        if let Some(effort) = &defaults.effort {
            agent::check_effort(effort)?;
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

    /// Removes a project that has no live sessions; returns the deleted sessions
    /// purged with it.
    pub fn remove_project(&mut self, name: &str) -> Result<Vec<Session>> {
        let project = self
            .db
            .project(name)?
            .with_context(|| format!("no project {name:?}"))?;
        self.db.remove_project(&project)
    }

    // Sessions

    pub fn sessions(&self, include_deleted: bool) -> Result<Vec<Session>> {
        self.db.sessions(include_deleted)
    }

    /// Finds a live (or, with `deleted`, a deleted) session by id, `project/name`,
    /// name, or id prefix of at least 4 characters.
    pub fn resolve(&self, addr: &str, deleted: bool) -> Result<Session> {
        let pool: Vec<Session> = self
            .db
            .sessions(true)?
            .into_iter()
            .filter(|s| s.deleted_at.is_some() == deleted)
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
            if deleted { "deleted " } else { "" }
        )
    }

    /// Creates a session in `project` and starts its agent.
    pub fn new_session(&mut self, req: NewSession) -> Result<Session> {
        let project = self.db.project(&req.project)?.with_context(|| {
            format!(
                "no project {:?} (add it with `roster project add`)",
                req.project
            )
        })?;
        let agent = req
            .agent
            .or_else(|| project.agent.clone())
            .unwrap_or_else(|| agent::CLAUDE.to_string());
        let kind = agent::kind(&agent, &self.config)?;
        if let Some(effort) = &req.effort {
            agent::check_effort(effort)?;
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
        // Model and effort are Claude flags.
        let (model, effort) = if kind == Kind::Claude {
            (req.model, req.effort)
        } else {
            (None, None)
        };
        let prompt = req
            .prompt
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let s = Session {
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
        };
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
            bail!("{} is deleted; restore it first", s.label());
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
            let size = util::terminal_size().unwrap_or(DEFAULT_SIZE);
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
        match self.kind(&s) {
            // Counts as a hook event, so the screen isn't consulted right away.
            Kind::Claude => s.last_event_at = Some(now),
            Kind::Generic => {
                s.last_event_at = None;
                s.baseline_at = Some(now + BOOT_GRACE_MS);
            }
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

    /// Deletes a session: stops it and hides it, keeping its conversation ID so
    /// `restore` can bring it back.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        self.stop(id)?;
        let mut s = self.db.session(id)?;
        self.tmux.kill_session(&s.tmux_name())?;
        s.deleted_at = Some(now_ms());
        s.set_state(State::Stopped, now_ms());
        self.db.save_session(&s)
    }

    /// Brings back a deleted session (stopped), optionally under a new name.
    pub fn restore(&mut self, id: &str, new_name: Option<&str>) -> Result<Session> {
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
        if s.deleted_at.is_none() {
            bail!("{} isn't deleted", s.label());
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
    pub fn attach(&mut self, id: &str) -> Result<()> {
        self.refresh()?;
        let s = self.db.session(id)?;
        if s.deleted_at.is_some() {
            bail!("{} is deleted; restore it first", s.label());
        }
        if s.state == State::Stopped {
            self.start(id)?;
        }
        let status = self.tmux.attach(&s.tmux_name())?;
        if !status.success() {
            bail!("couldn't attach to {}", s.label());
        }
        // Whatever happened while attached counts as seen.
        self.refresh()?;
        self.set_unread(id, false)
    }

    /// Renames a session or changes its model and effort; the agent picks those up
    /// on its next start.
    pub fn update_session(
        &mut self,
        id: &str,
        name: &str,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<Session> {
        validate_name("session", name)?;
        if let Some(effort) = &effort {
            agent::check_effort(effort)?;
        }
        self.db.fold_events(now_ms())?;
        let mut s = self.db.session(id)?;
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
        if self.kind(&s) == Kind::Claude {
            s.model = model;
            s.effort = effort;
        }
        self.db.save_session(&s)?;
        Ok(s)
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
        self.kind(s).can_resume()
    }

    /// Whether an agent is waiting for its first prompt: for Claude, after it has
    /// reported its session (a new conversation has no transcript before that);
    /// for others, idle after starting up.
    fn ready_for_prompt(&self, s: &Session, now: i64) -> bool {
        s.state == State::Idle
            && match self.kind(s) {
                Kind::Claude => s.transcript_path.is_some(),
                Kind::Generic => s.baseline_at.is_some_and(|b| now > b),
            }
    }

    /// Agents removed from the config are still watched, by their screen.
    fn kind(&self, s: &Session) -> Kind {
        agent::kind(&s.agent, &self.config).unwrap_or(Kind::Generic)
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

/// The task Claude names in its terminal title ("✳ Fix the parser" → "Fix the
/// parser"), or None while it shows its default title. Claude's titles start with
/// a glyph; anything else is tmux's default (the host name).
fn claude_title(pane_title: &str) -> Option<String> {
    if pane_title.chars().next().is_none_or(char::is_alphanumeric) {
        return None;
    }
    let title = pane_title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    (!title.is_empty() && title != "Claude Code").then(|| title.to_string())
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
    fn reads_claudes_task_title() {
        assert_eq!(
            claude_title("✳ tmux pane explanation").as_deref(),
            Some("tmux pane explanation")
        );
        assert_eq!(
            claude_title("⠐ Fix the parser").as_deref(),
            Some("Fix the parser")
        );
        assert_eq!(claude_title("✳ Claude Code"), None);
        assert_eq!(claude_title(""), None);
        assert_eq!(claude_title("computer"), None);
    }

    #[test]
    fn names_are_simple() {
        assert!(validate_name("session", "api-fix_2.0").is_ok());
        assert!(validate_name("session", "").is_err());
        assert!(validate_name("session", "a/b").is_err());
        assert!(validate_name("session", "has space").is_err());
    }
}
