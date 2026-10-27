use jagged::{index::RowIndex, Index2};

use crate::{
    clipboard::ClipboardTrait,
    helper::{append_str, insert_str, skip_whitespace},
    EditorState,
};

#[cfg(test)]
use crate::EditorMode;

use super::{delete::delete_selection, Execute};

/// Pastes the clipboard contents relative to the cursor, `count` times.
///
/// With `before` set, characterwise text is inserted in front of the cursor
/// and linewise text (clipboard starting with `\n`) opens a new line *above*
/// the current one (mirroring Vim's `P`). Otherwise the text is pasted after
/// the cursor / on the line below, matching Vim's `p`, the last line's too.
/// Lines put leave the cursor on the first one's first character that isn't
/// a blank, as vim does (roster's).
fn paste(state: &mut EditorState, before: bool, count: usize) {
    let s = state.clip.get_text();
    if s.is_empty() {
        return;
    }
    let count = count.max(1);

    state.capture();
    state.clamp_column();

    if state.view.single_line {
        let s = s.replace('\n', " ").replace('\r', "").repeat(count);
        if before {
            insert_str(&mut state.lines, &mut state.cursor, &s);
        } else {
            append_str(&mut state.lines, &mut state.cursor, &s);
        }
        return;
    }

    if let Some(stripped) = s.strip_prefix('\n') {
        let row = if before {
            state.cursor.row
        } else {
            (state.cursor.row + 1).min(state.lines.len())
        };
        state.cursor = Index2::new(row, 0);
        state.lines.insert(RowIndex::new(row), vec![]);
        let lines = vec![stripped; count].join("\n");
        append_str(&mut state.lines, &mut state.cursor, &lines);
        state.cursor = Index2::new(row, 0);
        skip_whitespace(&state.lines, &mut state.cursor);
    } else if before {
        insert_str(&mut state.lines, &mut state.cursor, &s.repeat(count));
    } else {
        append_str(&mut state.lines, &mut state.cursor, &s.repeat(count));
    }
}

#[derive(Clone, Debug)]
pub struct Paste;

impl Execute for Paste {
    fn execute(&mut self, state: &mut EditorState) {
        paste(state, false, 1);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

#[derive(Clone, Debug)]
pub struct PasteBefore;

impl Execute for PasteBefore {
    fn execute(&mut self, state: &mut EditorState) {
        paste(state, true, 1);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Vim's `p` (`P` `before`), with a count: what was yanked, put that many
/// times at once. Added by roster.
#[derive(Clone, Copy, Debug)]
pub struct Put {
    pub count: usize,
    pub before: bool,
}

impl Execute for Put {
    fn execute(&mut self, state: &mut EditorState) {
        paste(state, self.before, self.count);
    }

    fn is_repeatable(&self) -> bool {
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.count = n;
        true
    }
}

/// Replaces the selection with what was yanked, as vim's visual `p`, and
/// yanks what was selected.
#[derive(Clone, Debug)]
pub struct PasteOverSelection;

impl Execute for PasteOverSelection {
    fn execute(&mut self, state: &mut EditorState) {
        put_over_selection(state, false);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Replaces the selection with what was yanked, which stays yanked, as
/// vim's visual `P`. Added by roster.
#[derive(Clone, Debug)]
pub struct ReplaceSelection;

impl Execute for ReplaceSelection {
    fn execute(&mut self, state: &mut EditorState) {
        put_over_selection(state, true);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// The selection replaced with what was yanked, as vim does it: lines for
/// lines, text for text; lines over text on lines of their own, between
/// the text's line's halves; text over lines a line of its own. What was
/// selected is yanked then, unless `keep`. Roster's: edtui's put it all
/// where the selection began, lines and all, leaving empty lines.
fn put_over_selection(state: &mut EditorState, keep: bool) {
    let Some(selection) = state.selection.take() else {
        return;
    };
    let text = state.clip.get_text();
    if text.is_empty() {
        return;
    }
    state.capture();
    let replaced = delete_selection(state, &selection);
    let lines = text.strip_prefix('\n');
    if state.view.single_line {
        // In single-line mode, newlines are spaces.
        let text = lines.unwrap_or(&text).replace('\n', " ").replace('\r', "");
        insert_str(&mut state.lines, &mut state.cursor, &text);
    } else if selection.line_mode || lines.is_some() {
        let row = if selection.line_mode {
            selection.start().row.min(state.lines.len())
        } else {
            // Between the halves of the line the text was taken from.
            let at = state.cursor;
            let rest = state
                .lines
                .get_mut(RowIndex::new(at.row))
                .map(|line| line.split_off(at.col.min(line.len())))
                .unwrap_or_default();
            state.lines.insert(RowIndex::new(at.row + 1), rest);
            at.row + 1
        };
        for (i, line) in lines.unwrap_or(&text).split('\n').enumerate() {
            let line: Vec<char> = line.chars().collect();
            state.lines.insert(RowIndex::new(row + i), line);
        }
        state.cursor = Index2::new(row, 0);
        skip_whitespace(&state.lines, &mut state.cursor);
    } else {
        insert_str(&mut state.lines, &mut state.cursor, &text);
    }
    if !keep {
        state.clip.set_text(replaced.into());
    }
}

#[derive(Clone, Debug)]
pub struct CopySelection;

impl Execute for CopySelection {
    fn execute(&mut self, state: &mut EditorState) {
        if let Some(s) = &state.selection {
            state.clip.set_text(s.copy_from(&state.lines).into());
            state.selection = None;
        }
    }
}

#[derive(Clone, Debug)]
pub struct CopyLine;

impl Execute for CopyLine {
    fn execute(&mut self, state: &mut EditorState) {
        if let Some(line) = state.lines.get(RowIndex::new(state.cursor.row)) {
            let text = String::from('\n') + &line.iter().collect::<String>();
            state.clip.set_text(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::actions::Undo;
    use crate::clipboard::InternalClipboard;
    use crate::state::selection::Selection;
    use crate::Index2;
    use crate::Lines;

    use super::*;
    fn test_state() -> EditorState {
        let mut state = EditorState::new(Lines::from("Hello World!\n\n123."));
        state.set_clipboard(InternalClipboard::default());
        state
    }

    #[test]
    fn test_put_over_selection_as_vims_visual_p_and_shift_p() {
        let new = |text: &str, yanked: &str, selection: Selection| {
            let mut state = EditorState::new(Lines::from(text));
            state.set_clipboard(InternalClipboard::default());
            state.clip.set_text(yanked.into());
            state.selection = Some(selection);
            state
        };
        let at = |row, col| Index2::new(row, col);
        let lines = |from, to| Selection::new(at(from, 0), at(to, 0)).line_mode();
        // A line over a line: in its place, and what was there yanked.
        let mut state = new("a\nb\nc", "\na", lines(1, 1));
        PasteOverSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("a\na\nc"));
        assert_eq!(state.clip.get_text(), "\nb");
        // Over every line: the line put, no more, and never none.
        state.selection = Some(lines(0, 2));
        PasteOverSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("b"));
        let mut state = new("solo", "\nx", lines(0, 0));
        PasteOverSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("x"));
        // Text over lines: a line of its own; P keeps what was yanked.
        let mut state = new("a\nb\nc", "x", lines(0, 1));
        ReplaceSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("x\nc"));
        assert_eq!(state.clip.get_text(), "x");
        // Lines over text: between its line's halves; text over text.
        let mut state = new("abc", "\nX", Selection::new(at(0, 1), at(0, 1)));
        PasteOverSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("a\nX\nc"));
        assert_eq!(
            (state.cursor, state.clip.get_text()),
            (at(1, 0), "b".into())
        );
        let mut state = new("abc", "XY", Selection::new(at(0, 1), at(0, 1)));
        PasteOverSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("aXYc"));
        // Nothing yanked: nothing done.
        let mut state = new("abc", "", Selection::new(at(0, 1), at(0, 1)));
        PasteOverSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("abc"));
    }

    #[test]
    fn test_copy_paste() {
        let mut state = test_state();
        let selection = Selection::new(Index2::new(0, 0), Index2::new(0, 2));
        state.selection = Some(selection);

        CopySelection.execute(&mut state);
        Paste.execute(&mut state);

        assert_eq!(state.cursor, Index2::new(0, 3));
        assert_eq!(state.lines, Lines::from("HHelello World!\n\n123."));
    }

    #[test]
    fn test_paste_before_characterwise() {
        let mut state = test_state();
        state.selection = Some(Selection::new(Index2::new(0, 0), Index2::new(0, 2)));

        CopySelection.execute(&mut state);
        PasteBefore.execute(&mut state);

        // `P` inserts in front of the cursor, landing on the last pasted char.
        assert_eq!(state.cursor, Index2::new(0, 2));
        assert_eq!(state.lines, Lines::from("HelHello World!\n\n123."));
    }

    #[test]
    fn test_put_before_of_letters_beyond_ascii_lands_on_the_last() {
        let mut state = test_state();
        state.clip.set_text("żół".into());
        Put {
            count: 1,
            before: true,
        }
        .execute(&mut state);
        assert_eq!(state.lines, Lines::from("żółHello World!\n\n123."));
        assert_eq!(state.cursor, Index2::new(0, 2));
    }

    #[test]
    fn test_paste_before_linewise() {
        let mut state = test_state();
        state.cursor = Index2::new(2, 1);
        state.clip.set_text(String::from("\nnew line"));

        PasteBefore.execute(&mut state);

        // Linewise `P` opens a new line above the current row, the cursor
        // at its start.
        assert_eq!(state.cursor, Index2::new(2, 0));
        assert_eq!(state.lines, Lines::from("Hello World!\n\nnew line\n123."));
    }

    #[test]
    fn test_put_lines_below_the_last_and_counted() {
        let mut state = test_state();
        state.cursor = Index2::new(2, 1);
        state.clip.set_text(String::from("\n  indented"));

        // Below the last line too, the cursor on its first non-blank.
        Put {
            count: 1,
            before: false,
        }
        .execute(&mut state);
        assert_eq!(state.lines, Lines::from("Hello World!\n\n123.\n  indented"));
        assert_eq!(state.cursor, Index2::new(3, 2));

        // Twice at once, a block, one undo step.
        state.cursor = Index2::new(0, 0);
        Put {
            count: 2,
            before: false,
        }
        .execute(&mut state);
        assert_eq!(
            state.lines,
            Lines::from("Hello World!\n  indented\n  indented\n\n123.\n  indented")
        );
        assert_eq!(state.cursor, Index2::new(1, 2));
        crate::actions::Undo.execute(&mut state);
        assert_eq!(state.lines, Lines::from("Hello World!\n\n123.\n  indented"));

        // Characters, twice after the cursor.
        state.clip.set_text(String::from("ab"));
        state.cursor = Index2::new(0, 4);
        Put {
            count: 2,
            before: false,
        }
        .execute(&mut state);
        assert_eq!(
            state.lines.to_string().lines().next(),
            Some("Helloabab World!")
        );
    }

    #[test]
    fn test_paste_with_newline_into_empty_buffer() {
        let mut state = EditorState::default();
        state.set_clipboard(InternalClipboard::default());
        state.clip.set_text("\ntext".to_string());

        Paste.execute(&mut state);

        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from("text"));
    }

    #[test]
    fn test_paste_over_selection() {
        let mut state = test_state();
        state.selection = Some(Selection::new(Index2::new(0, 6), Index2::new(0, 10)));
        state.clip.set_text(String::from("Earth"));
        state.mode = EditorMode::Visual;

        PasteOverSelection.execute(&mut state);

        assert_eq!(state.lines, Lines::from("Hello Earth!\n\n123."));
        assert_eq!(state.cursor, Index2::new(0, 10));
        assert_eq!(state.mode, EditorMode::Visual);

        Undo.execute(&mut state);

        assert_eq!(state.lines, Lines::from("Hello World!\n\n123."));
        assert_eq!(state.mode, EditorMode::Visual);
    }
}
