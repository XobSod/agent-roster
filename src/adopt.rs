//! Taking over conversations started elsewhere (plain `claude` or `pi`,
//! another session manager): finding them (each agent's
//! `Adapter::conversations`) and turning one into a session.

use anyhow::{Result, bail};

use crate::agent::{self, Conversation};
use crate::engine::{Defaults, NewSession, Roster, new_project_name};
use crate::model::{Project, Session};

impl Roster {
    /// Conversations roster could take over: not already one of its live
    /// sessions, each with the project whose directory holds it (the deepest
    /// one), if any, and an archived roster session it belongs to, if any. With
    /// `all` false, only those in a project's directory.
    pub fn adoptable(&self, all: bool) -> Result<Vec<Adoptable>> {
        self.adoptable_where(all, |_| true)
    }

    /// `adoptable`, for the conversations whose id `wanted` says yes to.
    fn adoptable_where(&self, all: bool, wanted: impl Fn(&str) -> bool) -> Result<Vec<Adoptable>> {
        let projects = self.db.projects()?;
        let sessions = self.db.sessions(true)?;
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        let session = |agent: &str, id: &str| {
            sessions
                .iter()
                .find(|s| s.agent == agent && s.conversation_id.as_deref() == Some(id))
        };
        // Every agent's that can be started and adopts, newest first; those of
        // live sessions aren't even read: they're often the largest.
        let mut conversations = Vec::new();
        for name in agent::available(&self.config) {
            let adapter = agent::adapter(&name);
            if adapter.adopts() {
                let live = |id: &str| session(&name, id).is_some_and(|s| s.deleted_at.is_none());
                conversations.extend(adapter.conversations(&|id| wanted(id) && !live(id)));
            }
        }
        conversations.sort_by_key(|c| std::cmp::Reverse(c.modified));
        let mut found = Vec::new();
        for conversation in conversations {
            let known = session(&conversation.agent, &conversation.id);
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

    /// Takes over a conversation started elsewhere: a session of its agent (in
    /// its project, or in a new project for its directory) that resumes it,
    /// or with `fork`, a copy of it. A conversation open in its agent (as far
    /// as the agent tells: Claude does) is refused unless `force`: two would
    /// write the same conversation.
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
                "it's open in {} (pid {pid}); close it there first, or adopt a copy (--fork), or adopt it anyway (--yes)",
                crate::util::capitalize(&c.agent)
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
            agent: Some(c.agent.clone()),
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
            // A name it was given is like a title of your own (pi's is its
            // title, which it reports itself).
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
    /// Adopt a copy (the agent forks it) and leave the original alone.
    pub fork: bool,
    /// Adopt it even though it's open in its agent.
    pub force: bool,
}
