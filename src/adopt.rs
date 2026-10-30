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
        let found = self.to_adopt(&req.conversation)?;
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
        // A project made for it runs its agent, unless that's the default.
        let made_for_it = || Defaults {
            agent: (c.agent != self.config.default_agent()).then(|| c.agent.clone()),
            ..Defaults::default()
        };
        let project = match (&req.project, &found.project) {
            (Some(name), _) => match self.db.project(name)? {
                Some(p) => p,
                None => self.add_project(name, &c.cwd, made_for_it())?,
            },
            (None, Some(p)) => p.clone(),
            (None, None) => self.add_project(&found.new_project, &c.cwd, made_for_it())?,
        };
        // The defaults: the project's model and effort, which are for its
        // agent (the agent's in config.toml, else its own, after those).
        let agent = project
            .agent
            .as_deref()
            .unwrap_or(self.config.default_agent());
        let defaults = if agent == c.agent {
            (project.model.clone(), project.effort.clone())
        } else {
            (None, None)
        };
        // Or what it ran on, where its agent recorded it.
        let ran_on = match req.run_on {
            RunOn::Conversation => (
                c.model.clone().or(defaults.0.clone()),
                c.effort
                    .clone()
                    .filter(|e| agent::check_effort(&c.agent, e).is_ok())
                    .or(defaults.1.clone()),
            ),
            RunOn::Defaults => defaults.clone(),
        };
        let pick = |given: Option<Option<String>>, ran_on: Option<String>, default| match given {
            Some(Some(value)) => Some(value),
            Some(None) => default,
            None => ran_on,
        };
        let request = NewSession {
            project: project.name.clone(),
            name: req.name,
            agent: Some(c.agent.clone()),
            model: pick(req.model, ran_on.0, defaults.0),
            effort: pick(req.effort, ran_on.1, defaults.1),
            prompt: None,
        };
        // A copy's conversation is its agent's to name as it starts: Claude
        // and pi are given an id, codex reports its own.
        let fork_from = req.fork.then(|| c.id.clone());
        let (conversation_id, transcript_path) = if req.fork {
            (None, None)
        } else {
            (
                Some(c.id.clone()),
                Some(c.path.to_string_lossy().into_owned()),
            )
        };
        self.create_session(request, |s| {
            s.workdir = c.cwd.clone();
            s.conversation_id = conversation_id;
            s.transcript_path = transcript_path;
            // Moved, it's a conversation its agent kept: it has had turns.
            s.conversation_begun = !req.fork;
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

    /// Where the conversation `req` adopts is open, as far as its agent tells
    /// (Claude does): its agent and pid, for asking first. None for a copy,
    /// which leaves it alone. An error if it can't be adopted (`to_adopt`).
    pub fn open_elsewhere(&self, req: &Adopt) -> Result<Option<(String, i32)>> {
        let c = self.to_adopt(&req.conversation)?.conversation;
        Ok(c.open_in.filter(|_| !req.fork).map(|pid| (c.agent, pid)))
    }

    /// The conversation `conversation` (its id, or a prefix of 4+ characters)
    /// names; an error if there's none, several, or it's an archived session's.
    fn to_adopt(&self, conversation: &str) -> Result<Adoptable> {
        let wanted = |id: &str| {
            id == conversation || (conversation.len() >= 4 && id.starts_with(conversation))
        };
        let candidates = self.adoptable_where(true, wanted)?;
        let found = match &candidates[..] {
            [one] => one.clone(),
            [] => bail!(
                "no conversation {conversation:?} to adopt (`roster adopt --list --all` shows them)"
            ),
            _ => bail!("{conversation:?} matches several conversations; give more of the id"),
        };
        if let Some(s) = &found.archived {
            bail!(
                "that conversation is the archived session {}; restore it instead (`roster restore {}`)",
                s.label(),
                s.id
            );
        }
        Ok(found)
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

/// The model and effort an adopted conversation goes on with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunOn {
    /// What it last ran on, as far as its agent recorded it; the defaults for
    /// the rest.
    #[default]
    Conversation,
    /// A new session's in its project: the project's, if it runs the same
    /// agent, else the agent's in config.toml, else its own.
    Defaults,
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
    /// What it runs on, unless `model` and `effort` say.
    pub run_on: RunOn,
    /// Given here: a model, or (None) the defaults, as `RunOn::Defaults`.
    pub model: Option<Option<String>>,
    pub effort: Option<Option<String>>,
    /// Adopt a copy (the agent forks it) and leave the original alone.
    pub fork: bool,
    /// Adopt it even though it's open in its agent.
    pub force: bool,
}
