//! Carrying out what the user asked for (`Op`): what it says, or why it couldn't.
//! An op that draws, or hands the terminal over, returns the terminal's result
//! outside its own.

use super::*;

mod create;
mod edit;
mod lifecycle;
mod notes;
mod queued;
mod settings;
mod updates;

/// Carries out an op. Failures become messages; only terminal errors end the TUI.
pub(super) fn perform(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    op: Op,
) -> Result<()> {
    let outcome: Result<String> = match op {
        Op::ChangeSetting { setting, input } => {
            settings::change_setting(roster, app, setting, input)
        }
        Op::EditConfig => settings::edit_config(terminal, roster, app)?,
        Op::AddAgent(line) => settings::add_agent(roster, app, line),
        Op::Attach(id) => lifecycle::attach(terminal, roster, app, id)?,
        Op::Start(id) => lifecycle::start(terminal, roster, app, id)?,
        Op::PromptAgain(id) => lifecycle::prompt_again(terminal, roster, app, id)?,
        Op::RestartPromptAgain(id) => lifecycle::restart_prompt_again(terminal, roster, app, id)?,
        Op::AttachPromptAgain(id) => lifecycle::attach_prompt_again(terminal, roster, app, id)?,
        Op::Stop(id) => lifecycle::stop(terminal, roster, app, id)?,
        Op::Kill { session, process } => lifecycle::kill(terminal, roster, app, session, process)?,
        Op::Restart(id) => lifecycle::restart(terminal, roster, app, id)?,
        Op::Archive(id) => lifecycle::archive(terminal, roster, app, id)?,
        Op::Delete(id, how) => lifecycle::delete(terminal, roster, app, id, how)?,
        Op::AskDelete { id, step } => match lifecycle::ask_delete(roster, app, id, step) {
            Ok(Some(op)) => return perform(terminal, roster, app, op),
            Ok(None) => Ok(String::new()),
            Err(e) => Err(e),
        },
        Op::Restore { id, name } => lifecycle::restore(roster, app, id, name),
        Op::KillOrphans => lifecycle::kill_orphans(roster),
        Op::NewSession {
            works_in,
            project,
            agent,
            name,
            model,
            effort,
            prompt,
        } => create::new_session(
            terminal, roster, app, works_in, project, agent, name, model, effort, prompt,
        )?,
        Op::NewChat => create::new_chat(roster, app),
        Op::FindConversations => create::find_conversations(roster, app),
        Op::Adopt {
            conversation,
            project,
            name,
            run_on,
            model,
            effort,
            fork,
            force,
        } => create::adopt(
            terminal,
            roster,
            app,
            conversation,
            project,
            name,
            run_on,
            model,
            effort,
            fork,
            force,
        )?,
        Op::AddProject {
            name,
            path,
            agent,
            model,
            effort,
        } => create::add_project(roster, app, name, path, agent, model, effort),
        Op::EditSession {
            id,
            name,
            title,
            model,
            effort,
        } => edit::edit_session(roster, id, name, title, model, effort),
        Op::RetitleSession(id) => edit::retitle_session(roster, app, id),
        Op::SetUnread(id, unread) => edit::set_unread(roster, id, unread),
        Op::EditProject {
            id,
            name,
            path,
            agent,
            model,
            effort,
        } => edit::edit_project(roster, app, id, name, path, agent, model, effort),
        Op::RemoveProject(name, how) => edit::remove_project(roster, name, how),
        Op::AskRemoveProject { name, step } => {
            match edit::ask_remove_project(roster, app, name, step) {
                Ok(Some(op)) => return perform(terminal, roster, app, op),
                Ok(None) => Ok(String::new()),
                Err(e) => Err(e),
            }
        }
        Op::SaveNotes {
            saves,
            close,
            unsaved,
        } => notes::save_notes(roster, app, saves, close, unsaved),
        Op::EditNote(whose) => notes::edit_note(terminal, roster, app, &whose)?,
        Op::ReloadNote(whose) => notes::reload_note(roster, app, whose),
        Op::NoteVersion { whose, step } => notes::note_version(roster, app, whose, step),
        Op::SaveQueued {
            session,
            queued,
            n,
            base,
            text,
            close,
        } => queued::save_queued(roster, app, &session, (queued, n), (&base, &text), close),
        Op::EditQueued { session, queued, n } => {
            queued::edit_queued(terminal, roster, app, &session, (queued, n))?
        }
        Op::DropQueued {
            session,
            queued,
            prompt,
        } => queued::drop_queued(roster, app, session, queued, prompt),
        Op::Update => updates::update(app),
        Op::Quit => Ok(String::new()),
    };
    match outcome {
        Ok(message) if message.is_empty() => {}
        Ok(message) => app.flash(message),
        Err(e) => app.fail(format!("{e:#}")),
    }
    Ok(())
}

/// Shows a message while a slow operation runs.
pub(super) fn busy(terminal: &mut DefaultTerminal, app: &mut App, message: String) -> Result<()> {
    app.flash(message);
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(())
}

/// Hands the terminal to `run` (an editor) and takes it back. The outer result
/// is the terminal's.
pub(super) fn in_terminal<T>(terminal: &mut DefaultTerminal, run: impl FnOnce() -> T) -> Result<T> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    let ran = run();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(ran)
}
