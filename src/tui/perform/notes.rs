//! Notes: saved from the editor, handed to `$EDITOR`, taken as saved again,
//! or an older version shown.

use super::*;

/// Saves notes from the editor in turn, each marked saved there, until one
/// can't be; then closes it if all were and `close`. What was saved, to say,
/// and `unsaved` after it as an error.
pub(super) fn save_notes(
    roster: &Roster,
    app: &mut App,
    saves: Vec<op::NoteSave>,
    close: bool,
    unsaved: Option<String>,
) -> Result<String> {
    let mut said = Vec::new();
    // What was saved before one that couldn't be is, and says so.
    let failed = |e: anyhow::Error, said: &[String]| match said.is_empty() {
        true => e,
        false => e.context(said.join("; ")),
    };
    for s in saves {
        let saved = roster.save_note(&s.whose, &s.text, s.pin, s.base.as_ref(), s.force);
        let (note, merged) = match saved {
            Ok(NoteSaved::Saved { note, merged }) => (note, merged),
            Ok(NoteSaved::Conflicts {
                merged,
                pin,
                current,
            }) => {
                let n = merged.conflicts;
                if let Some(open) = app.note.as_mut() {
                    if let Some(side) = open.side_of(&s.whose) {
                        open.show_open(side);
                    }
                    if let Some(editor) = open.of(&s.whose) {
                        editor.conflicted(&merged, pin, current.as_ref());
                    }
                }
                let s = if n == 1 { "" } else { "s" };
                let why = format!("{n} conflict{s} with the external change, marked in the note");
                return Err(failed(anyhow::anyhow!(why), &said));
            }
            Err(e) => return Err(failed(e, &said)),
        };
        if let Some(editor) = app.note.as_mut().and_then(|n| n.of(&s.whose)) {
            match merged {
                true => editor.reload(note.as_ref()),
                false => editor.saved(note.as_ref()),
            }
        }
        let with = if merged {
            ", merged with the external change"
        } else {
            ""
        };
        said.push(match note {
            Some(_) => format!("Saved the note of {}{with}", s.label),
            None => format!("{} has no note now{with}", s.label),
        });
    }
    if close {
        app.note = None;
    }
    if let Some(mut why) = unsaved {
        // After what was saved, mid-sentence.
        if let Some(first) = why.get_mut(..1) {
            first.make_ascii_lowercase();
        }
        anyhow::bail!("{}; {why}", said.join("; "));
    }
    Ok(said.join("; "))
}

/// Hands a note to `$EDITOR`: the open editor's text, which comes back into
/// it, or else the note as saved, saved again when the editor exits. The
/// outer result is the terminal's.
pub(super) fn edit_note(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    whose: &Whose,
) -> Result<Result<String>> {
    let noted = match whose {
        Whose::Session(id) => roster.resolve_any(id).map(|s| s.noted()),
        Whose::Project(id) => roster.projects().and_then(|ps| {
            let p = ps.into_iter().find(|p| p.id == *id);
            Ok(p.context("the project is gone")?.noted())
        }),
    };
    let n = match noted {
        Ok(n) => n,
        Err(e) => return Ok(Err(e)),
    };
    let file = roster.note_file(&n);
    let failed = || anyhow::anyhow!("$EDITOR exited with an error; the note is as it was");
    if let Some(editor) = app.note.as_mut().and_then(|open| open.of(whose)) {
        let text = editor.text();
        let back = in_terminal(terminal, || {
            crate::note::edit_externally(&file, &text, |back| {
                editor.replace(&back);
                Ok(())
            })
        })?;
        return Ok(back.and_then(|done| {
            done.map(|()| "Back from $EDITOR: :w saves the note".to_string())
                .ok_or_else(failed)
        }));
    }
    let text = n.note.as_ref().map_or(String::new(), |n| n.text.clone());
    let saved = in_terminal(terminal, || {
        crate::note::edit_externally(&file, &text, |back| roster.change_note(&n, &back))
    })?;
    Ok(saved.and_then(|said| said.ok_or_else(failed)))
}

pub(super) fn reload_note(roster: &Roster, app: &mut App, whose: Whose) -> Result<String> {
    roster.note(&whose).map(|note| {
        let editor = app.note.as_mut().and_then(|open| open.of(&whose));
        match editor {
            Some(editor) => {
                editor.reload(note.as_ref());
                format!("Reloaded the note of {}", editor.label)
            }
            None => String::new(),
        }
    })
}

pub(super) fn note_version(
    roster: &Roster,
    app: &mut App,
    whose: Whose,
    step: isize,
) -> Result<String> {
    roster.note_versions(&whose).and_then(|mut kept| {
        let current = roster.note(&whose)?;
        let Some(editor) = app.note.as_mut().and_then(|open| open.of(&whose)) else {
            return Ok(String::new());
        };
        let label = editor.label.clone();
        let of = kept.len();
        let to = match crate::tui::note::step_to(&kept, editor.version.as_ref(), step) {
            None if of == 0 => return Ok(format!("The note of {label} has no older versions")),
            None => {
                return Ok(format!(
                    "No earlier version: version {of} is the oldest kept"
                ));
            }
            Some(0) => {
                editor.reload(current.as_ref());
                return Ok(format!("The note of {label} as saved"));
            }
            Some(to) => to,
        };
        editor.show_version(kept.swap_remove(to - 1), current.as_ref());
        Ok(format!(
            "Version {to} of {of} of the note of {label}: :w brings it back, :e! goes back to \
             the note"
        ))
    })
}
