//! Sessions and projects made: a new session or chat, a conversation found
//! and adopted, a project added.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn new_session(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    works_in: crate::engine::WorksIn,
    project: String,
    agent: String,
    name: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    prompt: Option<String>,
) -> Result<Result<String>> {
    busy(terminal, app, format!("Starting {agent} in {project}…"))?;
    let prompted = prompt.is_some();
    let request = NewSession {
        works_in,
        project,
        name,
        agent: Some(agent),
        model,
        effort,
        prompt,
    };
    Ok(roster.new_session(request).map(|s| {
        app.select(EntryKey::Session(s.id.clone()));
        let prompt = match prompted {
            false => "",
            true if agent::adapter(&s.agent).takes_prompt() => ", the prompt on its command line",
            true => "; the prompt is typed in once it's ready",
        };
        format!("Started {}{prompt}", s.label())
    }))
}

pub(super) fn new_chat(roster: &mut Roster, app: &mut App) -> Result<String> {
    roster.chat_project().and_then(|chats| {
        app.chat_project = Some(chats.id);
        app.open_chat_form(&roster.projects()?, &chats);
        Ok(String::new())
    })
}

pub(super) fn find_conversations(roster: &Roster, app: &mut App) -> Result<String> {
    roster.adoptable(true).map(|found| {
        app.pick_conversation(found);
        String::new()
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn adopt(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    conversation: String,
    project: String,
    name: Option<String>,
    run_on: crate::adopt::RunOn,
    model: Option<Option<String>>,
    effort: Option<Option<String>>,
    fork: bool,
    force: bool,
) -> Result<Result<String>> {
    busy(terminal, app, format!("Adopting into {project}…"))?;
    let request = Adopt {
        conversation,
        project: Some(project),
        name,
        run_on,
        model,
        effort,
        fork,
        force,
    };
    Ok(roster.adopt(request).map(|s| {
        app.select(EntryKey::Session(s.id.clone()));
        let copy = if fork { " as a copy" } else { "" };
        format!("Adopted {}{copy}", s.label())
    }))
}

pub(super) fn add_project(
    roster: &Roster,
    app: &mut App,
    name: String,
    path: String,
    agent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
) -> Result<String> {
    roster
        .add_project(
            &name,
            Path::new(&path),
            Defaults {
                agent,
                model,
                effort,
            },
        )
        .map(|p| {
            app.select_project(&p.name);
            format!("Added project {}", p.name)
        })
}
