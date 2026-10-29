//! `roster note`: printing, changing and pinning notes.

use crate::*;

/// `roster note`.
pub fn note(r: &mut Roster, args: NoteArgs) -> Result<()> {
    let NoteArgs {
        session,
        project,
        set,
        append,
        clear,
        edit,
        pin,
        unpin,
    } = args;
    let n = noted(r, session.as_deref(), project.as_deref())?;
    if pin.is_some() || unpin {
        println!("{}", r.pin_note(&n, pin)?);
        return Ok(());
    }
    let old = n.note.as_ref().map_or(String::new(), |n| n.text.clone());
    let new = match (set, append) {
        (Some(text), _) => {
            let text = match text.as_str() {
                "-" => io::read_to_string(io::stdin())?,
                _ => text,
            };
            // Not an empty pipe taken for a note to remove.
            if note::tidy(&text).is_empty() {
                bail!("nothing to set the note to; --clear removes it");
            }
            text
        }
        (None, Some(line)) if old.is_empty() => line,
        (None, Some(line)) => format!("{old}\n{line}"),
        (None, None) if clear => String::new(),
        (None, None) if edit => {
            let file = r.note_file(&n);
            match note::edit_externally(&file, &old, |text| r.change_note(&n, &text))? {
                Some(said) => println!("{said}"),
                None => bail!("$EDITOR exited with an error; the note is as it was"),
            }
            return Ok(());
        }
        (None, None) => {
            match &n.note {
                Some(note) => println!("{}", note.text),
                None => eprintln!("{} has no note", n.label),
            }
            return Ok(());
        }
    };
    println!("{}", r.change_note(&n, &new)?);
    Ok(())
}

/// Whose note `roster note` acts on: `session`'s, or with `project` a
/// project's, the one named or else the session's. Inside an agent, without
/// a session, its own session is meant.
fn noted(r: &Roster, session: Option<&str>, project: Option<&str>) -> Result<note::Noted> {
    let of = |id: i64| -> Result<note::Noted> {
        let p = r.projects()?.into_iter().find(|p| p.id == id);
        Ok(p.context("its project is gone")?.noted())
    };
    // Inside an agent, its session, by the id roster gave it.
    let own = || -> Result<Option<model::Session>> {
        match std::env::var("ROSTER_SESSION") {
            Ok(id) if !id.is_empty() => {
                let s = r.sessions(false)?.into_iter().find(|s| s.id == id);
                let gone =
                    || format!("ROSTER_SESSION is {id}, which names no live session: give one");
                s.map(Some).with_context(gone)
            }
            _ => Ok(None),
        }
    };
    match (session, project) {
        (Some(_), Some(name)) if !name.is_empty() => {
            bail!("a session or --project with a name, not both")
        }
        (Some(session), Some(_)) => of(r.resolve_any(session)?.project_id),
        (Some(session), None) => Ok(r.resolve_any(session)?.noted()),
        (None, Some("")) => match own()? {
            Some(s) => of(s.project_id),
            None => bail!("which project's note? --project <PROJECT>"),
        },
        (None, Some(name)) => match r.projects()?.into_iter().find(|p| p.name == name) {
            Some(p) => Ok(p.noted()),
            None => bail!("no project named {name:?}"),
        },
        (None, None) => match own()? {
            Some(s) => Ok(s.noted()),
            None => bail!("which note? a session's, or --project <PROJECT>"),
        },
    }
}
