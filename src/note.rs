//! Notes: what you write about a session, kept with it until it's deleted for
//! good. One line can be pinned: the preview shows it and the one below, so a
//! list of things to do shows where you are in it.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub text: String,
    /// The pinned line, of `text.lines()`; without one the preview shows the
    /// note's start.
    pub pin: Option<usize>,
    /// When it was last saved.
    pub at: i64,
}

/// What the preview shows of a note: `lines` from line `first`, of `total`.
#[derive(Debug, PartialEq)]
pub struct Shown<'a> {
    pub lines: Vec<&'a str>,
    pub first: usize,
    pub total: usize,
}

impl Note {
    /// The pinned line and the one below it (unless that's blank), or without a
    /// pin the first two lines, past blank ones.
    pub fn shown(&self) -> Shown<'_> {
        let lines: Vec<&str> = self.text.lines().collect();
        let total = lines.len();
        let first = match self.pin {
            Some(pin) => pin.min(total.saturating_sub(1)),
            None => lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0),
        };
        let mut shown: Vec<&str> = lines.iter().skip(first).take(2).copied().collect();
        if shown.len() == 2 && shown[1].trim().is_empty() {
            shown.pop();
        }
        Shown {
            lines: shown,
            first,
            total,
        }
    }
}

/// A note's text as it's kept: newlines without carriage returns, and nothing
/// blank at its end. Nothing left is no note.
pub fn tidy(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_string()
}

/// Where the pinned line `pin` of `old` is in `new`, after one change between
/// them (an edit, lines added or removed): lines added or removed above it move
/// it with them, and an edit of it keeps it where it is. If it was removed, the
/// pin goes to the line that took its place (or the last one).
pub fn follow_pin(old: &[&str], new: &[&str], pin: Option<usize>) -> Option<usize> {
    let pin = pin?;
    let last = new.len().checked_sub(1)?;
    if pin >= old.len() {
        return Some(pin.min(last));
    }
    // The change: what isn't the same at the start or at the end.
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - start;
    let end = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    let (old_end, new_end) = (old.len() - end, new.len() - end);
    let at = if pin < start {
        pin
    } else if pin >= old_end {
        pin - old_end + new_end
    } else if let Some(moved) = (start..new_end)
        .filter(|&i| new[i] == old[pin])
        .min_by_key(|&i| i.abs_diff(pin))
    {
        moved
    } else if pin < new_end {
        pin
    } else if new_end > start {
        // Lines joined: the one it went into.
        new_end - 1
    } else {
        new_end
    };
    Some(at.min(last))
}

/// Has `$VISUAL` or `$EDITOR` edit `text` in `file`, then `keep` what it
/// saved; None if the editor failed (as vim's `:cq` makes it). The file goes
/// once the text is kept; if it can't be, it stays, and the error says where.
pub fn edit_externally<T>(
    file: &Path,
    text: &str,
    keep: impl FnOnce(String) -> Result<T>,
) -> Result<Option<T>> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)
        .and_then(|mut f| {
            let end = if text.is_empty() { "" } else { "\n" };
            write!(f, "{text}{end}")
        });
    written.with_context(|| format!("can't write {}", file.display()))?;
    if !crate::util::run_editor(file).inspect_err(|_| {
        let _ = fs::remove_file(file);
    })? {
        let _ = fs::remove_file(file);
        return Ok(None);
    }
    let kept = fs::read_to_string(file)
        .map_err(anyhow::Error::from)
        .and_then(|back| {
            // Without the newline editors end a file with (and it was given).
            let back = back.strip_suffix('\n').unwrap_or(&back);
            keep(back.strip_suffix('\r').unwrap_or(back).to_string())
        })
        .with_context(|| format!("what you wrote is kept in {}", file.display()))?;
    let _ = fs::remove_file(file);
    Ok(Some(kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str, pin: Option<usize>) -> Note {
        Note {
            text: text.into(),
            pin,
            at: 0,
        }
    }

    fn follow(old: &str, new: &str, pin: usize) -> Option<usize> {
        let (old, new): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
        follow_pin(&old, &new, Some(pin))
    }

    #[test]
    fn the_preview_shows_the_pinned_line_and_the_next() {
        let todo = "TODO\n- [x] one\n- [ ] two\n- [ ] three\n";
        let pinned = note(todo, Some(2));
        let shown = pinned.shown();
        assert_eq!(shown.lines, ["- [ ] two", "- [ ] three"]);
        assert_eq!((shown.first, shown.total), (2, 4));
        // The last line, alone; without a pin, the start past blank lines.
        assert_eq!(note(todo, Some(3)).shown().lines, ["- [ ] three"]);
        assert_eq!(
            note("\n\nfirst\nsecond\nthird", None).shown().lines,
            ["first", "second"]
        );
        // A blank line below isn't shown; a pin past the end is the last line.
        assert_eq!(note("one\n\ntwo", Some(0)).shown().lines, ["one"]);
        assert_eq!(note("one\ntwo", Some(9)).shown().lines, ["two"]);
    }

    #[test]
    fn notes_are_kept_without_what_is_blank_at_their_end() {
        assert_eq!(tidy("one\r\ntwo  \n\n\n"), "one\ntwo");
        assert_eq!(tidy(" \n\n"), "");
        assert_eq!(tidy("\n  indented"), "\n  indented");
    }

    #[test]
    fn a_pin_follows_its_line() {
        let todo = "a\nb\nPIN\nc\nd";
        // Edits elsewhere, and of the line itself.
        assert_eq!(follow(todo, "a\nb\nPIN\nc\nd e", 2), Some(2));
        assert_eq!(follow(todo, "a\nb\nPIN done\nc\nd", 2), Some(2));
        // Lines added or removed above, or below.
        assert_eq!(follow(todo, "new\na\nb\nPIN\nc\nd", 2), Some(3));
        assert_eq!(follow(todo, "a\nb\nnew\nPIN\nc\nd", 2), Some(3));
        assert_eq!(follow(todo, "b\nPIN\nc\nd", 2), Some(1));
        assert_eq!(follow(todo, "a\nb\nPIN\nnew\nc\nd", 2), Some(2));
        assert_eq!(follow(todo, "a\nb\nPIN", 2), Some(2));
        // Split (Enter in it): its first part.
        assert_eq!(follow(todo, "a\nb\nP\nIN\nc\nd", 2), Some(2));
        // Removed: the line that took its place, at the end the last one.
        assert_eq!(follow(todo, "a\nb\nc\nd", 2), Some(2));
        assert_eq!(follow("a\nPIN", "a", 1), Some(0));
        assert_eq!(follow(todo, "a\nb\nd", 2), Some(2));
        // Joined with the line above: the line it went into.
        assert_eq!(follow(todo, "a\nb PIN\nc\nd", 2), Some(1));
        // Moved within the change.
        assert_eq!(follow(todo, "a\nPIN\nb x\nc\nd", 2), Some(1));
        // Nothing left: no pin.
        assert_eq!(follow(todo, "", 2), None);
        assert_eq!(follow_pin(&["a"], &["b"], None), None);
    }
}
