//! Sessions' lives: attaching, starting, stopping, killing, restarting,
//! archiving, restoring and deleting them; and orphaned tmux sessions killed.

use super::*;

pub(super) fn attach(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    app.attaching(&id);
    let result = hand_to_tmux(terminal, roster, app, &id)?.map(|prompted| {
        app.detached(prompted);
        String::new()
    });
    // Attaching sized it to the terminal: fit it to the preview again.
    app.preview.fitted.clear();
    Ok(result)
}

/// Hands the terminal to `tmux attach` until the user detaches. The outer result
/// is the terminal's, the inner one the attach's: whether a prompt was submitted.
///
/// tmux switches to the terminal's other screen and back by itself, so roster
/// stays on its own until then (leaving it first flashes the normal screen),
/// and draws itself again as it was the moment tmux is done, before catching
/// up on what happened meanwhile (which takes a moment: catching up first
/// shows the normal screen, then a blank one).
fn hand_to_tmux(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &App,
    id: &str,
) -> Result<Result<bool>> {
    disable_raw_mode()?;
    let view = serde_json::to_string(&app.view()).ok();
    let attached = roster.attach_terminal(id, view.as_deref());
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(attached.and_then(|since| roster.detached(id, since)))
}

pub(super) fn start(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Starting {label}…"))?;
    Ok(roster.start(&id).map(|()| format!("Started {label}")))
}

pub(super) fn prompt_again(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Starting {label}…"))?;
    Ok(roster
        .give_prompt_again(&id)
        .map(|()| format!("Started {label}, its first prompt given again")))
}

pub(super) fn restart_prompt_again(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Restarting {label}…"))?;
    Ok(roster
        .restart_giving_prompt(&id)
        .map(|()| format!("Restarted {label}, its first prompt given again")))
}

pub(super) fn attach_prompt_again(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Starting {label}…"))?;
    // Started meanwhile (by a roster of its own, say): attached to
    // as it is, as attaching would.
    let started =
        (roster.resolve(&id, false)).is_ok_and(|s| s.state != crate::model::State::Stopped);
    let given = if started {
        Ok(())
    } else {
        roster.give_prompt_again(&id)
    };
    match given {
        Err(e) => Ok(Err(e)),
        Ok(()) => attach(terminal, roster, app, id),
    }
}

pub(super) fn stop(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Stopping {label}…"))?;
    Ok(roster.stop(&id).map(|stopped| {
        if stopped {
            format!("Stopped {label}")
        } else {
            format!("{label} wasn't running")
        }
    }))
}

pub(super) fn kill(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    session: String,
    process: Option<(i32, u64)>,
) -> Result<Result<String>> {
    let label = app.session_label(&session);
    busy(terminal, app, format!("Killing {label}…"))?;
    Ok(roster.kill(&session, process).map(|killed| match process {
        Some((pid, _)) => killed.said(&format!("process {pid} of {label}")),
        None => killed.said(&label),
    }))
}

pub(super) fn restart(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Restarting {label}…"))?;
    Ok(roster.restart(&id).map(|()| format!("Restarted {label}")))
}

pub(super) fn archive(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
) -> Result<Result<String>> {
    let label = app.session_label(&id);
    busy(terminal, app, format!("Archiving {label}…"))?;
    Ok(roster
        .archive(&id)
        .map(|()| format!("Archived {label}; it's under Archived")))
}

pub(super) fn delete(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: String,
    how: crate::engine::Deleting,
) -> Result<Result<String>> {
    // A live one's agent stops first, which can take a moment.
    busy(
        terminal,
        app,
        format!("Deleting {}…", app.session_label(&id)),
    )?;
    Ok(roster.delete(&id, how).map(|(s, said)| {
        [format!("Deleted {} for good", s.label())]
            .into_iter()
            .chain(said)
            .collect::<Vec<_>>()
            .join("; ")
    }))
}

pub(super) fn ask_delete(
    roster: &Roster,
    app: &mut App,
    id: String,
    step: Option<op::LeavingStep>,
) -> Result<Option<Op>> {
    roster.db.session(&id).and_then(|s| {
        let leaving = roster.leaving(std::slice::from_ref(&id))?;
        Ok(app.ask_leaving(
            crate::agent::delete_question(&s),
            &leaving,
            step,
            |how| Op::Delete(id.clone(), how),
            |step| Op::AskDelete {
                id: id.clone(),
                step: Some(step),
            },
        ))
    })
}

pub(super) fn restore(
    roster: &mut Roster,
    app: &mut App,
    id: String,
    name: Option<String>,
) -> Result<String> {
    roster.restore(&id, name.as_deref()).map(|s| {
        app.select(EntryKey::Session(s.id.clone()));
        format!("Restored {} (stopped)", s.label())
    })
}

pub(super) fn kill_orphans(roster: &mut Roster) -> Result<String> {
    roster
        .kill_orphans()
        .map(|n| format!("Killed {n} orphaned tmux session(s)"))
}
