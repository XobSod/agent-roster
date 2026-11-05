//! Sessions and projects changed: a session edited, retitled, or marked read
//! or unread; a project edited, or removed.

use super::*;

pub(super) fn edit_session(
    roster: &mut Roster,
    id: String,
    name: Option<String>,
    title: Option<Option<String>>,
    model: Option<Option<String>>,
    effort: Option<Option<String>>,
) -> Result<String> {
    roster
        .update_session(&id, name.as_deref(), title, model, effort)
        .map(|s| {
            if s.state == crate::model::State::Stopped {
                format!("Saved {}", s.label())
            } else {
                format!(
                    "Saved {}; a new model or effort applies after a restart",
                    s.label()
                )
            }
        })
}

/// Asked on a thread: the model takes a while, and the TUI goes on.
pub(super) fn retitle_session(roster: &mut Roster, app: &mut App, id: String) -> Result<String> {
    if app.titling.iter().any(|t| t.id == id) {
        return Err(anyhow::anyhow!(
            "Already asking for a title for {}",
            app.session_label(&id)
        ));
    }
    roster.title_job(&id).map(|job| {
        let agent = crate::util::capitalize(&job.agent);
        let asked_of = job.asked_of.clone();
        let (send, title) = std::sync::mpsc::channel();
        std::thread::spawn(move || send.send(job.run()));
        app.titling.push(app::Titling {
            id: id.clone(),
            title,
            asked_of,
        });
        format!("Asking {agent} for a title for {}…", app.session_label(&id))
    })
}

pub(super) fn set_unread(roster: &mut Roster, id: String, unread: bool) -> Result<String> {
    roster.set_unread(&id, unread).map(|()| String::new())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn edit_project(
    roster: &Roster,
    app: &mut App,
    id: i64,
    name: Option<String>,
    path: Option<String>,
    agent: Option<Option<String>>,
    model: Option<Option<String>>,
    effort: Option<Option<String>>,
) -> Result<String> {
    roster
        .update_project(
            id,
            name.as_deref(),
            path.as_deref().map(Path::new),
            agent,
            model,
            effort,
        )
        .map(|p| {
            app.select_project(&p.name);
            format!("Saved project {}", p.name)
        })
}

pub(super) fn remove_project(
    roster: &mut Roster,
    name: String,
    how: crate::engine::Deleting,
) -> Result<String> {
    roster.remove_project(&name, how).map(|(_, said)| {
        [format!("Removed project {name}")]
            .into_iter()
            .chain(said)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

pub(super) fn ask_remove_project(
    roster: &Roster,
    app: &mut App,
    name: String,
    step: Option<op::LeavingStep>,
) -> Result<Option<Op>> {
    roster.removal_question(&name).and_then(|question| {
        let project =
            (roster.db.project(&name)?).with_context(|| format!("no project {name:?}"))?;
        let ids: Vec<String> = (roster.db.purged_with(&project)?.into_iter())
            .map(|s| s.id)
            .collect();
        let leaving = roster.leaving(&ids)?;
        Ok(app.ask_leaving(
            question,
            &leaving,
            step,
            |how| Op::RemoveProject(name.clone(), how),
            |step| Op::AskRemoveProject {
                name: name.clone(),
                step: Some(step),
            },
        ))
    })
}
