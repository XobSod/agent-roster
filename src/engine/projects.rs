//! Projects: adding, changing and removing them, and the chats' own.

use super::*;

impl Roster {
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

    pub(super) fn checked_project(
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
