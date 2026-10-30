//! Sessions: finding them, creating, starting, stopping, archiving, restoring and deleting them.

use super::*;

impl Roster {
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

    /// `resolve` among live and archived sessions alike, for what can't be
    /// undone (deleting for good, killing): an address naming one of each,
    /// but the id of neither, says so rather than choosing.
    pub fn resolve_either(&self, addr: &str) -> Result<Session> {
        match (self.find(addr, false)?, self.find(addr, true)?) {
            (Some(live), Some(archived)) => {
                if live.id == addr {
                    return Ok(live);
                }
                if archived.id == addr {
                    return Ok(archived);
                }
                bail!(
                    "{addr:?} names a live session, {} ({}), and an archived one, {} ({}): give the id of the one meant",
                    live.label(),
                    live.id,
                    archived.label(),
                    archived.id
                )
            }
            (Some(s), None) | (None, Some(s)) => Ok(s),
            (None, None) => bail!("no session {addr:?}"),
        }
    }

    /// The session `addr` names, None if none does, an error if several do.
    pub(super) fn find(&self, addr: &str, archived: bool) -> Result<Option<Session>> {
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
        // Made last, once nothing else is left to refuse it.
        if let Some((dir, worktree)) = self.place(&project, &name, &req.works_in)? {
            s.workdir = dir;
            s.worktree = Some(worktree);
        }
        if let Err(e) = self.db.insert_session(&s) {
            if let Some(w) = s.worktree.as_ref().filter(|w| w.made) {
                undo_worktree(w);
            }
            return Err(e);
        }
        self.start(&s.id)?;
        self.db.session(&s.id)
    }

    /// Starts a stopped session: sources the env files and launches its agent,
    /// resuming the conversation if it has one.
    pub fn start(&mut self, id: &str) -> Result<()> {
        self.start_giving(id, false)
    }

    /// Starts a stopped session giving its agent the first prompt it was
    /// started with before (`Session::given_prompt`), which it may never
    /// have taken: asked for, never on its own, as given twice it would be
    /// acted on twice.
    pub fn give_prompt_again(&mut self, id: &str) -> Result<()> {
        self.start_giving(id, true)
    }

    /// `start`, giving its kept first prompt `again`: taken from where it's
    /// kept only in what a start that worked saves, so one that fails leaves
    /// it there, and no later start gives it unasked.
    pub(super) fn start_giving(&mut self, id: &str, again: bool) -> Result<()> {
        // A first prompt this roster gave up on is tried again once started.
        self.prompt_tries.remove(id);
        self.refresh()?;
        let mut s = self.live_session(id)?;
        let before = s.clone();
        if again {
            if let Some(refused) = s.prompt_again_refused() {
                bail!(refused);
            }
            s.pending_prompt = s.given_prompt.take();
        }
        if let StartCheck::Refused(why) = start_check(&s, Starting::Start) {
            bail!(why);
        }
        // Its worktree gone: made again where it was (asked about first),
        // and set up again, being new.
        if let Some(w) = s.worktree.as_mut().filter(|w| !w.root.exists()) {
            crate::git::prune_worktrees(&w.repo)?;
            if let Some(parent) = w.root.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("can't make {}", parent.display()))?;
            }
            crate::git::add_worktree(&w.repo, &w.root, &w.branch, None)?;
            let project = (self.db.projects()?.into_iter()).find(|p| p.id == s.project_id);
            w.set_up = !project.is_some_and(|p| p.origin.join(WORKTREE_SETUP).is_file());
            // Kept, though the start fails: the worktree is there now.
            let remade = Session {
                worktree: s.worktree.clone(),
                ..before.clone()
            };
            self.db.save_changes(&before, &remade)?;
        }
        // One its agent no longer has (asked about first) begins anew on its
        // id: nothing of it said yet, and no copy of the one it came from.
        if agent::conversation_lost(&s) {
            s.conversation_begun = false;
            s.fork_from = None;
        }
        // Written, it has begun, whatever roster saw of it, so it's missed
        // once its agent deletes it.
        if (s.transcript_path.as_deref()).is_some_and(|p| Path::new(p).exists()) {
            s.conversation_begun = true;
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
        // Its folder and roster's hooks trusted in its agent first, as
        // asked, by roster run in its place: in its environment, once the
        // env files are sourced.
        let mut command = launch.argv.clone();
        let adapter = agent::adapter(&s.agent);
        let folder = self.config.trust_project_folders() && adapter.trusts_folders();
        let hooks = self.config.trusts_hooks(&s.agent) && adapter.trusts_hooks();
        // A worktree's folder trusted as its project's checkout is.
        let checkout = (s.worktree.is_some() && adapter.trusts_folders())
            .then(|| self.db.projects())
            .transpose()?
            .and_then(|ps| ps.into_iter().find(|p| p.id == s.project_id))
            .map(|p| p.origin);
        if folder || hooks || checkout.is_some() {
            let mut head = vec![self.exe.to_string_lossy().into_owned(), "trust".into()];
            head.extend(folder.then(|| "--folder".to_string()));
            head.extend(hooks.then(|| "--hooks".to_string()));
            head.extend((checkout.as_ref()).map(|c| format!("--like={}", c.display())));
            head.extend([s.agent.clone(), "--".into()]);
            command.splice(0..0, head);
        }
        let argv = self.wrapped(&s, prompt.as_deref(), true, &command)?;
        // What an agent's hooks need to report (`roster report`); for a
        // worktree's setup script, where its project's checkout is.
        let mut env = vec![
            ("ROSTER_SESSION", s.id.clone()),
            ("ROSTER_DB", self.paths.db().to_string_lossy().into_owned()),
            ("ROSTER_EXE", self.exe.to_string_lossy().into_owned()),
        ];
        if s.worktree.is_some()
            && let Some(project) = (self.db.projects()?.into_iter()).find(|p| p.id == s.project_id)
        {
            env.push((
                "ROSTER_CHECKOUT",
                project.origin.to_string_lossy().into_owned(),
            ));
        }
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
        // Before its agent runs, so what it reports of this run comes after.
        let now = now_ms();
        let started = if has_pane {
            self.tmux.respawn(&name, &s.workdir, &env, &argv)
        } else {
            let (w, h) = util::terminal_size().unwrap_or(DEFAULT_SIZE);
            // A row less for the status line, so attaching doesn't resize it;
            // fitted always, the preview's size, as a TUI last told it.
            let mut size = (w, h - u16::from(self.config.status_line() && h > 1));
            let always = agent::fit_mode(&s.agent, &self.config) == FitPreview::Always;
            if always {
                size = self.preview_size().unwrap_or(size);
            }
            let started = self.tmux.new_session(&name, &s.workdir, size, &env, &argv);
            // Kept so as an attach comes; if not, the TUI fits it again.
            if always && started.is_ok() {
                self.tmux.resize_fixed(&name, size).ok();
            }
            started
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
        // Handed to it on its command line: nothing left to type in, but
        // kept until it reports its first turn (it may exit first).
        if agent::adapter(&s.agent).takes_prompt() {
            s.given_prompt = s.pending_prompt.take().or(s.given_prompt.take());
        }
        s.set_state(State::Working, now);
        s.started_at = Some(now);
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

    /// `argv` run by roster's shell for `s`, after it sources the env files:
    /// the global one, then that of `s`'s project; with `set_up`, a worktree
    /// not set up yet is set up first, by its project's script.
    pub(crate) fn wrapped(
        &self,
        s: &Session,
        prompt: Option<&Path>,
        set_up: bool,
        argv: &[String],
    ) -> Result<Vec<String>> {
        let project = self
            .db
            .projects()?
            .into_iter()
            .find(|p| p.id == s.project_id);
        let origin = project.map(|p| p.origin);
        let project_env = origin
            .as_ref()
            .map_or_else(PathBuf::new, |o| o.join(PROJECT_ENV));
        let setup = (origin.as_ref())
            .filter(|_| set_up && s.worktree.as_ref().is_some_and(|w| !w.set_up))
            .map(|o| o.join(WORKTREE_SETUP))
            .filter(|script| script.is_file());
        let shell = launch::shell(&self.config, std::env::var("SHELL").ok().as_deref());
        Ok(launch::wrap(
            &shell,
            &self.paths.global_env(),
            &project_env,
            prompt,
            setup.as_deref(),
            argv,
        ))
    }

    /// Session `id`, unless it's archived.
    pub(super) fn live_session(&self, id: &str) -> Result<Session> {
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

    /// `restart`, giving its kept first prompt again (`give_prompt_again`):
    /// for an agent still at a screen it showed before taking it. One that
    /// said it took it since it was asked about is left running, rather
    /// than stopped for a start that would refuse it.
    pub fn restart_giving_prompt(&mut self, id: &str) -> Result<()> {
        self.refresh()?;
        let s = self.live_session(id)?;
        if s.given_prompt.is_none() {
            bail!(
                "{} took its first prompt meanwhile; left as it is",
                s.label()
            );
        }
        self.stop(id)?;
        self.give_prompt_again(id)
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

    /// Deletes a session for good, archived or not: its agent stops, as it
    /// does archiving, and roster forgets it and its events. The conversation
    /// stays where the agent keeps it (one of Claude, Codex or pi can be adopted
    /// again).
    pub fn delete(&mut self, id: &str, how: Deleting) -> Result<(Session, Vec<String>)> {
        self.db.fold_events(now_ms())?;
        let s = self.db.session(id)?;
        if s.deleted_at.is_none() {
            self.stop(id)?;
        }
        self.tmux.kill_session(&s.tmux_name())?;
        // As it is now, its agent gone: not what was asked about.
        let leaving = self.leaving(&[id.to_string()])?;
        self.db.delete_session(id)?;
        let said = self.let_go(leaving, how);
        Ok((s, said))
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
    /// Its project's checkout, or a worktree.
    pub works_in: WorksIn,
}

pub(super) fn validate_name(what: &str, name: &str) -> Result<()> {
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
