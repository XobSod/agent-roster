//! Notes: what you write about a session, kept with it until it's deleted for
//! good, or about a project, kept with it until it's removed. One line can be
//! pinned: the preview shows it and the ones below (`notes.lines` in all), so
//! a list of things to do shows where you are in it.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Whose {
    /// A session's, by its id.
    Session(String),
    /// A project's, by its id.
    Project(i64),
}

/// A note as read, with whose it is and what to call that.
#[derive(Clone, Debug, PartialEq)]
pub struct Noted {
    pub whose: Whose,
    /// "api/fix", or "project api".
    pub label: String,
    pub note: Option<Note>,
}

/// What the preview shows of a note: `lines` from line `first`, of `total`.
#[derive(Debug, PartialEq)]
pub struct Shown<'a> {
    pub lines: Vec<&'a str>,
    pub first: usize,
    pub total: usize,
}

impl Note {
    /// `count` lines from the pinned one on, or without a pin from the first
    /// not blank; blank ones at the end left out.
    pub fn shown(&self, count: usize) -> Shown<'_> {
        let lines: Vec<&str> = self.text.lines().collect();
        let total = lines.len();
        let first = match self.pin {
            Some(pin) => pin.min(total.saturating_sub(1)),
            None => lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0),
        };
        let mut shown: Vec<&str> = lines.iter().skip(first).take(count).copied().collect();
        while shown.len() > 1 && shown.last().is_some_and(|l| l.trim().is_empty()) {
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

/// Where a part both changed starts in a merged note, with yours below it.
pub const CONFLICT_START: &str = "<<<<<<< yours";
/// Between yours and the external change, below it.
pub const CONFLICT_MIDDLE: &str = "======= external change";
pub const CONFLICT_END: &str = ">>>>>>>";

/// Your text merged with an external change, both made from `base`.
#[derive(Debug, PartialEq)]
pub struct Merged {
    /// Each part changed on one side as that side has it; a part both
    /// changed differently as both have it, between the conflict markers.
    pub text: String,
    pub conflicts: usize,
    /// The first conflict's line.
    pub first: Option<usize>,
}

/// Merges `yours` with `theirs`, both edited from `base`, by lines. Changes to
/// different lines are both kept, a line changed on both sides alike once;
/// lines changed differently on both sides are a conflict. Where both changed
/// lines in place, none moved (a box ticked beside a line edited), each line
/// is told apart.
pub fn merge(base: &str, yours: &str, theirs: &str) -> Merged {
    let (base, yours, theirs): (Vec<&str>, Vec<&str>, Vec<&str>) = (
        base.lines().collect(),
        yours.lines().collect(),
        theirs.lines().collect(),
    );
    let (in_yours, in_theirs) = (matches(&base, &yours), matches(&base, &theirs));
    let mut merged = Merging::default();
    let (mut b, mut y, mut t) = (0, 0, 0);
    // Lines kept as they were on both sides split the notes into parts.
    for i in 0..=base.len() {
        let (ye, te) = if i == base.len() {
            (yours.len(), theirs.len())
        } else if let (Some(ye), Some(te)) = (in_yours[i], in_theirs[i]) {
            (ye, te)
        } else {
            continue;
        };
        // A line kept on one side is where it was on both.
        let in_place = (b..i).all(|k| {
            in_yours[k].is_none_or(|m| m - y == k - b)
                && in_theirs[k].is_none_or(|m| m - t == k - b)
        });
        merged.part(&base[b..i], &yours[y..ye], &theirs[t..te], in_place);
        if let Some(line) = base.get(i) {
            merged.lines.push(line);
        }
        (b, y, t) = (i + 1, ye + 1, te + 1);
    }
    Merged {
        text: merged.lines.join("\n"),
        conflicts: merged.conflicts,
        first: merged.first,
    }
}

#[derive(Default)]
struct Merging<'a> {
    lines: Vec<&'a str>,
    conflicts: usize,
    first: Option<usize>,
}

impl<'a> Merging<'a> {
    /// A part between lines kept on both sides, as each side has it; each
    /// side's lines `in_place` if none moved.
    fn part(&mut self, base: &[&str], yours: &[&'a str], theirs: &[&'a str], in_place: bool) {
        if yours == base || yours == theirs {
            self.lines.extend(theirs);
        } else if theirs == base {
            self.lines.extend(yours);
        } else if in_place && yours.len() == base.len() && theirs.len() == base.len() {
            let mut clash = 0;
            for i in 0..=base.len() {
                let different = i < base.len() && yours[i] != base[i] && theirs[i] != base[i];
                if different && yours[i] != theirs[i] {
                    continue;
                }
                if clash < i {
                    self.conflict(&yours[clash..i], &theirs[clash..i]);
                }
                if let Some(&b) = base.get(i) {
                    self.lines
                        .push(if yours[i] == b { theirs[i] } else { yours[i] });
                }
                clash = i + 1;
            }
        } else {
            self.conflict(yours, theirs);
        }
    }

    fn conflict(&mut self, yours: &[&'a str], theirs: &[&'a str]) {
        self.conflicts += 1;
        self.first.get_or_insert(self.lines.len());
        self.lines.push(CONFLICT_START);
        self.lines.extend(yours);
        self.lines.push(CONFLICT_MIDDLE);
        self.lines.extend(theirs);
        self.lines.push(CONFLICT_END);
    }
}

/// Which line of `new` each line of `old` is kept as, by their longest common
/// run of lines. A note is a few hundred lines at most; beyond what's alike
/// at the start and the end, a middle too big to compare is taken as changed
/// whole.
fn matches(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let mut kept = vec![None; old.len()];
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let end = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for (i, k) in kept.iter_mut().enumerate().take(start) {
        *k = Some(i);
    }
    for (i, k) in kept.iter_mut().rev().enumerate().take(end) {
        *k = Some(new.len() - 1 - i);
    }
    let (a, b) = (&old[start..old.len() - end], &new[start..new.len() - end]);
    if a.is_empty() || b.is_empty() || a.len() * b.len() > 1 << 22 {
        return kept;
    }
    // How many lines from a[i..] and b[j..] can be kept, at most.
    let width = b.len() + 1;
    let mut most = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            most[i * width + j] = if a[i] == b[j] {
                most[(i + 1) * width + j + 1] + 1
            } else {
                most[(i + 1) * width + j].max(most[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            kept[start + i] = Some(start + j);
            (i, j) = (i + 1, j + 1);
        } else if most[(i + 1) * width + j] >= most[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    kept
}

/// Has `$VISUAL` or `$EDITOR` edit `text` in a file of its own next to
/// `base` (`base` with this process's id, so no other editor of it or of
/// another note shares it), then `keep` what it saved; None if the editor
/// failed (as vim's `:cq` makes it). The file goes once the text is kept; if
/// it can't be, it stays, and the error says where.
pub fn edit_externally<T>(
    base: &Path,
    text: &str,
    keep: impl FnOnce(String) -> Result<T>,
) -> Result<Option<T>> {
    let (file, mut handle) = create_own(base)?;
    let end = if text.is_empty() { "" } else { "\n" };
    write!(handle, "{text}{end}").with_context(|| format!("can't write {}", file.display()))?;
    drop(handle);
    if !crate::util::run_editor(&file).inspect_err(|_| {
        let _ = fs::remove_file(&file);
    })? {
        let _ = fs::remove_file(&file);
        return Ok(None);
    }
    let kept = fs::read_to_string(&file)
        .map_err(anyhow::Error::from)
        .and_then(|back| {
            // Without the newline editors end a file with (and it was given).
            let back = back.strip_suffix('\n').unwrap_or(&back);
            keep(back.strip_suffix('\r').unwrap_or(back).to_string())
        })
        .with_context(|| format!("what you wrote is kept in {}", file.display()))?;
    let _ = fs::remove_file(&file);
    Ok(Some(kept))
}

/// A new file for this process next to `base` (`notes/demo-one-1a2b3c4d.md`
/// gives `notes/demo-one-1a2b3c4d.4242.md`), made here and only here: one
/// left by an editor before (kept after a failed save) gets another number.
fn create_own(base: &Path) -> Result<(std::path::PathBuf, fs::File)> {
    let dir = base.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("note");
    let pid = std::process::id();
    for n in 0..100 {
        let name = match n {
            0 => format!("{stem}.{pid}.md"),
            n => format!("{stem}.{pid}-{n}.md"),
        };
        let file = dir.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
        {
            Ok(handle) => return Ok((file, handle)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("can't write {}", file.display())),
        }
    }
    anyhow::bail!("no file of its own for a note next to {}", base.display())
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
        let shown = pinned.shown(2);
        assert_eq!(shown.lines, ["- [ ] two", "- [ ] three"]);
        assert_eq!((shown.first, shown.total), (2, 4));
        // The last line, alone; without a pin, the start past blank lines.
        assert_eq!(note(todo, Some(3)).shown(2).lines, ["- [ ] three"]);
        assert_eq!(
            note("\n\nfirst\nsecond\nthird", None).shown(2).lines,
            ["first", "second"]
        );
        // A blank line below isn't shown; a pin past the end is the last line.
        assert_eq!(note("one\n\ntwo", Some(0)).shown(2).lines, ["one"]);
        assert_eq!(note("one\ntwo", Some(9)).shown(2).lines, ["two"]);
        // As many as asked for (notes.lines), none blank at the end.
        assert_eq!(pinned.shown(1).lines, ["- [ ] two"]);
        assert_eq!(
            note(todo, Some(0)).shown(3).lines,
            ["TODO", "- [x] one", "- [ ] two"]
        );
        assert_eq!(note("a\nb\n\n\nc", Some(0)).shown(4).lines, ["a", "b"]);
    }

    #[test]
    fn notes_are_kept_without_what_is_blank_at_their_end() {
        assert_eq!(tidy("one\r\ntwo  \n\n\n"), "one\ntwo");
        assert_eq!(tidy(" \n\n"), "");
        assert_eq!(tidy("\n  indented"), "\n  indented");
    }

    #[test]
    fn each_editing_gets_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("notes/demo-one-1a2b3c4d.md");
        let (first, _) = create_own(&base).unwrap();
        let (second, _) = create_own(&base).unwrap();
        assert_ne!(first, second);
        let pid = std::process::id().to_string();
        assert!(
            first
                .to_str()
                .unwrap()
                .ends_with(&format!("demo-one-1a2b3c4d.{pid}.md"))
        );
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

    #[test]
    fn an_external_change_is_merged_with_yours() {
        let base = "TODO\n- [ ] first\n- [ ] second\n- [ ] third";
        let clean = |text: &str| Merged {
            text: text.into(),
            conflicts: 0,
            first: None,
        };
        // A box ticked while you added a line further down: both kept.
        let ticked = "TODO\n- [x] first\n- [ ] second\n- [ ] third";
        let added = "TODO\n- [ ] first\n- [ ] second\n- [ ] third\n- [ ] fourth";
        let both = "TODO\n- [x] first\n- [ ] second\n- [ ] third\n- [ ] fourth";
        assert_eq!(merge(base, added, ticked), clean(both));
        assert_eq!(merge(base, ticked, added), clean(both));
        // Beside each other, edited in place: each line told apart.
        let edited = "TODO\n- [ ] first\n- [ ] second, with the retry\n- [ ] third";
        let next = "TODO\n- [x] first\n- [ ] second, with the retry\n- [ ] third";
        assert_eq!(merge(base, edited, ticked), clean(next));
        // A line removed on one side, another edited on the other.
        let removed = "TODO\n- [ ] first\n- [ ] second";
        let third = "TODO\n- [x] first\n- [ ] second";
        assert_eq!(merge(base, removed, ticked), clean(third));
        // The same change on both sides, once; one side unchanged, the other's.
        assert_eq!(merge(base, ticked, ticked), clean(ticked));
        assert_eq!(merge(base, base, ticked), clean(ticked));
        assert_eq!(merge(base, ticked, base), clean(ticked));
        // From no note at all, the same text written on both sides.
        assert_eq!(merge("", "a", "a"), clean("a"));
    }

    #[test]
    fn lines_both_changed_differently_are_marked() {
        let base = "TODO\n- [ ] first\n- [ ] second\n- [ ] third";
        let yours = "TODO\n- [ ] first\n- [ ] second, with the retry\n- [ ] third";
        let theirs = "TODO\n- [x] first\n- [x] second\n- [ ] third";
        let merged = merge(base, yours, theirs);
        assert_eq!(
            merged.text,
            "TODO\n- [x] first\n<<<<<<< yours\n- [ ] second, with the retry\n\
             ======= external change\n- [x] second\n>>>>>>>\n- [ ] third"
        );
        assert_eq!((merged.conflicts, merged.first), (1, Some(2)));
        // A line removed on one side and edited on the other; lines added at
        // the same place on both; each a conflict.
        let merged = merge("a\nb\nc", "a\nc\nyours", "a\nB\nc\ntheirs");
        assert_eq!(
            merged.text,
            "a\n<<<<<<< yours\n======= external change\nB\n>>>>>>>\nc\n\
             <<<<<<< yours\nyours\n======= external change\ntheirs\n>>>>>>>"
        );
        assert_eq!((merged.conflicts, merged.first), (2, Some(1)));
        // Lines moved on one side aren't compared line by line: a line one
        // side removed isn't put back, nor one taken twice.
        for (base, yours, theirs) in [
            ("x\ny", "\nx", "\ny"),
            (
                "TODO\n- [ ] a\n- [ ] b",
                "TODO\n\n- [ ] a",
                "TODO\n\n- [ ] b",
            ),
            ("a\nb\nc", "d\na\nc", "d\nb\na"),
        ] {
            assert_eq!(
                merge(base, yours, theirs).conflicts,
                1,
                "{yours:?} {theirs:?}"
            );
            assert_eq!(
                merge(base, theirs, yours).conflicts,
                1,
                "{theirs:?} {yours:?}"
            );
        }
        // Written from nothing on both sides, differently.
        assert_eq!(merge("", "a", "b").conflicts, 1);
    }

    #[test]
    fn a_middle_too_big_to_compare_is_changed_whole() {
        let note = |n: usize| {
            let mut lines: Vec<String> = (0..n).map(|i| format!("b{i}")).collect();
            lines.insert(0, "start".into());
            lines.push("end".into());
            lines
        };
        // A line added at the top and the last removed on your side, one
        // edited on theirs: merged in a note of a few hundred lines, a
        // conflict in one of thousands.
        for (n, conflicts) in [(300, 0), (2100, 1)] {
            let base = note(n);
            let mut yours = base.clone();
            yours.insert(1, "new".into());
            yours.remove(n + 1);
            let mut theirs = base.clone();
            theirs[n / 2] = "edited".into();
            let merged = merge(&base.join("\n"), &yours.join("\n"), &theirs.join("\n"));
            assert_eq!(merged.conflicts, conflicts, "{n} lines");
        }
    }
}
