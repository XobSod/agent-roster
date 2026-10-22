//! Taking over Claude conversations started elsewhere (plain `claude`,
//! another session manager): finding them (`agent::claude::conversations`)
//! and turning one into a session.

use anyhow::{Result, bail};

use crate::agent;
use crate::agent::claude::conversations::{self, Conversation};
use crate::engine::{Defaults, NewSession, Roster, new_project_name};
use crate::model::{Project, Session};

impl Roster {
    /// Claude conversations roster could take over: not already one of its live
    /// sessions, each with the project whose directory holds it (the deepest
    /// one), if any, and an archived roster session it belongs to, if any. With
    /// `all` false, only those in a project's directory.
    pub fn adoptable(&self, all: bool) -> Result<Vec<Adoptable>> {
        self.adoptable_where(all, |_| true)
    }

    /// `adoptable`, for the conversations whose id `wanted` says yes to.
    fn adoptable_where(&self, all: bool, wanted: impl Fn(&str) -> bool) -> Result<Vec<Adoptable>> {
        let Some(dir) = conversations::claude_dir() else {
            return Ok(Vec::new());
        };
        let projects = self.db.projects()?;
        let sessions = self.db.sessions(true)?;
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        let session = |id: &str| {
            sessions
                .iter()
                .find(|s| s.conversation_id.as_deref() == Some(id))
        };
        // Those of live sessions aren't even read: they're often the largest.
        let live = |id: &str| session(id).is_some_and(|s| s.deleted_at.is_none());
        let mut found = Vec::new();
        for conversation in conversations::conversations(&dir, |id| wanted(id) && !live(id)) {
            let known = session(&conversation.id);
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
        let wanted = |id: &str| {
            id == req.conversation
                || (req.conversation.len() >= 4 && id.starts_with(&req.conversation))
        };
        let candidates = self.adoptable_where(true, wanted)?;
        let matches: Vec<&Adoptable> = candidates.iter().collect();
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
}

/// A conversation roster could adopt.
#[derive(Clone, Debug)]
pub struct Adoptable {
    pub conversation: Conversation,
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
