//! A session's queue: a prompt saved from the editor or handed to `$EDITOR`,
//! or prompts dropped.

use super::*;

/// Saves `text`, as written from `base` (`edit`: both), as prompt `n` of
/// session `id`'s queue (`which`: its id and `n`), the editor open on it
/// then as saved, and closed if `close`. If it was typed in or changed
/// meanwhile, that's said, and what the editor has is kept in a file, as
/// `$EDITOR` keeps it, the editor closed.
pub(super) fn save_queued(
    roster: &Roster,
    app: &mut App,
    id: &str,
    which: (i64, usize),
    edit: (&str, &str),
    close: bool,
) -> Result<String> {
    let ((queued, n), (base, text)) = (which, edit);
    let label = app.session_label(id);
    let text = text.trim();
    if text.is_empty() {
        bail!("Nothing is left of prompt {n}: d drops it");
    }
    crate::engine::check_queued_fits(&roster.db.session(id)?, text)?;
    let mut failed = match roster.db.edit_queued(id, queued, base, text)? {
        crate::db::Edited::Saved => None,
        crate::db::Edited::Gone => Some(format!(
            "Prompt {n} of {label}'s queue was typed in meanwhile"
        )),
        crate::db::Edited::Changed => Some(format!(
            "Prompt {n} of {label}'s queue was changed meanwhile"
        )),
    };
    if let Some(view) = app.queue.as_mut().filter(|v| v.session == id)
        && let Some(editing) = view.editing.as_mut().filter(|e| e.id == queued)
    {
        match &mut failed {
            None => {
                let note = crate::note::Note {
                    text: text.to_string(),
                    pin: None,
                    at: 0,
                };
                // Trimmed as it's kept, it's shown so.
                match crate::note::tidy(&editing.editor.text()) == text {
                    true => editing.editor.saved(Some(&note)),
                    false => editing.editor.reload(Some(&note)),
                }
                editing.base = text.to_string();
                if close {
                    view.editing = None;
                }
            }
            Some(why) => {
                let file = roster.queued_file(queued);
                // Not kept, it stays in the editor.
                let kept = crate::note::write_own(&file, &editing.editor.text())
                    .with_context(|| why.clone())?;
                *why += &format!("; what you wrote is kept in {}", kept.display());
                view.editing = None;
            }
        }
    }
    match failed {
        Some(why) => Err(anyhow::anyhow!(why)),
        None => Ok(format!("Saved prompt {n} of {label}'s queue")),
    }
}

/// Hands prompt `n` of session `id`'s queue (`which`: its id and `n`) to
/// `$EDITOR`: the open editor's text, which comes back into it, or else the
/// prompt as queued, saved when the editor exits. The outer result is the
/// terminal's.
pub(super) fn edit_queued(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: &str,
    which: (i64, usize),
) -> Result<Result<String>> {
    let (queued, n) = which;
    let file = roster.queued_file(queued);
    let failed = || anyhow::anyhow!("$EDITOR exited with an error; the prompt is as it was");
    let open = (app.queue.as_mut())
        .filter(|v| v.session == id)
        .and_then(|v| v.editing.as_mut())
        .filter(|e| e.id == queued);
    if let Some(editing) = open {
        let text = editing.editor.text();
        let back = in_terminal(terminal, || {
            crate::note::edit_externally(&file, &text, |back| {
                editing.editor.replace(&back);
                Ok(())
            })
        })?;
        return Ok(back.and_then(|done| {
            done.map(|()| "Back from $EDITOR: :w saves the prompt".to_string())
                .ok_or_else(failed)
        }));
    }
    let label = app.session_label(id);
    let prompt = match roster.db.queue(id) {
        Ok(queue) => queue.into_iter().find(|q| q.id == queued).map(|q| q.prompt),
        Err(e) => return Ok(Err(e)),
    };
    let Some(prompt) = prompt else {
        return Ok(Err(anyhow::anyhow!(
            "Prompt {n} of {label}'s queue was typed in meanwhile"
        )));
    };
    let saved = in_terminal(terminal, || {
        crate::note::edit_externally(&file, &prompt, |back| {
            save_queued(roster, app, id, which, (&prompt, &back), true)
        })
    })?;
    Ok(saved.and_then(|said| said.ok_or_else(failed)))
}

pub(super) fn drop_queued(
    roster: &Roster,
    app: &App,
    session: String,
    queued: Vec<i64>,
    prompt: Option<String>,
) -> Result<String> {
    let label = app.session_label(&session);
    let asked = queued.len();
    (queued.iter())
        .map(|&q| roster.db.drop_queued(&session, Some(q)))
        .sum::<Result<usize>>()
        .and_then(|gone| match (prompt, gone) {
            (Some(p), 0) => bail!("{p} of {label}'s queue was typed in meanwhile"),
            (Some(p), _) => Ok(format!("Dropped {p} from {label}'s queue")),
            (None, 0) => bail!("The prompts queued for {label} were typed in meanwhile"),
            (None, 1) if asked == 1 => Ok(format!("Dropped the prompt queued for {label}")),
            (None, k) if k == asked => Ok(format!("Dropped the {k} prompts queued for {label}")),
            (None, k) => Ok(format!(
                "Dropped {k} of the {asked} prompts queued for {label}: the others were \
                 typed in meanwhile"
            )),
        })
}
