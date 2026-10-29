//! Notes: read, saved, merged with what changed meanwhile, pinned.

use super::*;

/// A note of `text`, its pin on a line it has (`note::tidy` may take lines
/// off its end); None if there's nothing but blanks.
pub(super) fn new_note(text: &str, pin: Option<usize>) -> Option<Note> {
    let text = note::tidy(text);
    (!text.is_empty()).then(|| {
        let last = text.lines().count().saturating_sub(1);
        Note {
            pin: pin.map(|p| p.min(last)),
            text,
            at: now_ms(),
        }
    })
}

/// What saving a note from the editor came to.
#[derive(Debug)]
pub enum NoteSaved {
    /// Saved as this (None: removed); `merged` with an external change.
    Saved { note: Option<Note>, merged: bool },
    /// An external change to the same lines as yours: nothing saved. Both are
    /// in `merged`, marked, its pin at `pin`; `current` is the note as saved
    /// now.
    Conflicts {
        merged: note::Merged,
        pin: Option<usize>,
        current: Option<Note>,
    },
}

/// `text`, pinned at `pin` and written from `base`, merged with `current`,
/// saved meanwhile; and where its pin goes: yours, unless you left it on its
/// line (edited or not) and the external change moved it.
pub(super) fn merge_note(
    base: Option<&Note>,
    text: &str,
    pin: Option<usize>,
    current: Option<&Note>,
) -> (note::Merged, Option<usize>) {
    fn text_of(n: Option<&Note>) -> &str {
        n.map_or("", |n| n.text.as_str())
    }
    let text = note::tidy(text);
    let merged = note::merge(text_of(base), &text, text_of(current));
    let before: Vec<&str> = text_of(base).lines().collect();
    let yours: Vec<&str> = text.lines().collect();
    let theirs: Vec<&str> = text_of(current).lines().collect();
    let out: Vec<&str> = merged.text.lines().collect();
    let (base_pin, current_pin) = (base.and_then(|n| n.pin), current.and_then(|n| n.pin));
    let left = pin == note::follow_pin(&before, &yours, base_pin);
    let moved = current_pin != note::follow_pin(&before, &theirs, base_pin);
    let pin = if left && moved {
        note::follow_pin(&theirs, &out, current_pin)
    } else {
        note::follow_pin(&yours, &out, pin)
    };
    (merged, pin)
}

impl Roster {
    /// A session's or a project's note as saved now.
    pub fn note(&self, whose: &Whose) -> Result<Option<Note>> {
        self.db.note(whose)
    }

    /// Saves a session's or a project's note from the TUI's editor, opened
    /// on note `base` (nothing but blanks removes it): merged with one saved
    /// meanwhile, unless `force`d over it.
    pub fn save_note(
        &self,
        whose: &Whose,
        text: &str,
        pin: Option<usize>,
        base: Option<&Note>,
        force: bool,
    ) -> Result<NoteSaved> {
        let note = new_note(text, pin);
        if force {
            self.db.set_note(whose, note.as_ref())?;
        }
        if force || self.db.set_note_if(whose, base, note.as_ref())? {
            return Ok(NoteSaved::Saved {
                note,
                merged: false,
            });
        }
        let current = self.db.note(whose)?;
        let (merged, pin) = merge_note(base, text, pin, current.as_ref());
        if merged.conflicts > 0 {
            return Ok(NoteSaved::Conflicts {
                merged,
                pin,
                current,
            });
        }
        let note = new_note(&merged.text, pin);
        if !self
            .db
            .set_note_if(whose, current.as_ref(), note.as_ref())?
        {
            bail!("The note changed again as it was merged: :w tries again");
        }
        Ok(NoteSaved::Saved { note, merged: true })
    }

    /// Saves `text` as the note `n`, edited from what it was, so its pin
    /// follows its line. What happened, to say.
    pub fn change_note(&self, n: &Noted, text: &str) -> Result<String> {
        let (old, pin) = n
            .note
            .as_ref()
            .map_or(("", None), |n| (n.text.as_str(), n.pin));
        if note::tidy(text) == old {
            return Ok(format!("The note of {} is unchanged", n.label));
        }
        let before: Vec<&str> = old.lines().collect();
        let after: Vec<&str> = text.lines().collect();
        let pin = note::follow_pin(&before, &after, pin);
        let mut note = new_note(text, pin);
        let mut merged = false;
        // Not over one saved meanwhile (another editor, an agent through
        // `roster note`), which yours was never made from: merged with it.
        if !self
            .db
            .set_note_if(&n.whose, n.note.as_ref(), note.as_ref())?
        {
            let current = self.db.note(&n.whose)?;
            let (both, pin) = merge_note(n.note.as_ref(), text, pin, current.as_ref());
            note = new_note(&both.text, pin);
            merged = true;
            if both.conflicts > 0 {
                bail!(
                    "the note of {} changed meanwhile in the lines you changed, so yours isn't \
                     saved over it",
                    n.label
                );
            }
            if !self
                .db
                .set_note_if(&n.whose, current.as_ref(), note.as_ref())?
            {
                bail!(
                    "the note of {} changed again as yours was merged with it, so yours isn't \
                     saved",
                    n.label
                );
            }
        }
        let with = if merged {
            ", merged with the external change"
        } else {
            ""
        };
        Ok(match note {
            Some(_) => format!("Saved the note of {}{with}", n.label),
            None => format!("{} has no note now{with}", n.label),
        })
    }

    /// Pins line `line` (from 1) of the note `n`, or with None unpins it.
    /// What happened, to say.
    pub fn pin_note(&self, n: &Noted, line: Option<usize>) -> Result<String> {
        let Some(note) = &n.note else {
            bail!("{} has no note", n.label);
        };
        let lines: Vec<&str> = note.text.lines().collect();
        let label = &n.label;
        let pin = match line {
            None => None,
            Some(line) if (1..=lines.len()).contains(&line) => Some(line - 1),
            Some(line) => bail!(
                "the note of {label} has {} lines, so no line {line}",
                lines.len()
            ),
        };
        // Over the note it was asked of, not one saved meanwhile.
        if !self
            .db
            .set_note_if(&n.whose, Some(note), new_note(&note.text, pin).as_ref())?
        {
            bail!("the note of {label} changed meanwhile; nothing pinned");
        }
        Ok(match pin {
            Some(p) => format!("Pinned line {} of the note of {label}: {}", p + 1, lines[p]),
            None => format!("Unpinned the note of {label}"),
        })
    }

    /// Where `$EDITOR` edits a note: named after whose it is, so the editor's
    /// title says so, and its id, which only it has (`a-b/c` and `a/b-c` name
    /// alike); each editing makes its own file next to that
    /// (`note::edit_externally`).
    pub fn note_file(&self, n: &Noted) -> PathBuf {
        let id = match &n.whose {
            Whose::Session(id) => id.clone(),
            Whose::Project(id) => id.to_string(),
        };
        let name: String = n
            .label
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
                _ => '-',
            })
            .collect();
        self.paths
            .state_dir
            .join("notes")
            .join(format!("{name}-{id}.md"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merged_notes_pin_is_yours_unless_only_the_other_moved_it() {
        let note = |text: &str, pin| Note {
            text: text.into(),
            pin,
            at: 0,
        };
        let base = note("a\n- [ ] b\nc\nd", Some(1));
        // The agent pinned another line while you ticked the pinned one: its.
        let current = note("a\n- [ ] b\nc\nd!", Some(2));
        let (merged, pin) = merge_note(Some(&base), "a\n- [x] b\nc\nd", Some(1), Some(&current));
        assert_eq!((merged.text.as_str(), pin), ("a\n- [x] b\nc\nd!", Some(2)));
        // You moved it too: yours.
        let (_, pin) = merge_note(Some(&base), "a\n- [x] b\nc\nd", Some(3), Some(&current));
        assert_eq!(pin, Some(3));
        // Neither moved it: on its line, wherever lines added above it put it.
        let current = note("new\na\n- [ ] b\nc\nd", Some(2));
        let (_, pin) = merge_note(Some(&base), "a\n- [x] b\nc\nd", Some(1), Some(&current));
        assert_eq!(pin, Some(2));
        // In a conflict, worked out the same way.
        let current = note("a\n- [-] b\nc\nd", Some(3));
        let (merged, pin) = merge_note(Some(&base), "a\n- [x] b\nc\nd", Some(1), Some(&current));
        assert_eq!((merged.conflicts, pin), (1, Some(7)));
    }
}
