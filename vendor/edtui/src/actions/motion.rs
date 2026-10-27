use std::cmp::min;

use crate::{helper::find_matching_bracket, state::selection::set_selection_with_lines};
use jagged::Index2;

use super::{word, Execute, MotionKind};
use crate::{
    helper::{max_col, max_col_normal, skip_whitespace},
    EditorMode, EditorState, Lines,
};

#[derive(Clone, Debug, Copy)]
pub struct MoveForward(pub usize);

impl Execute for MoveForward {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    /// Past the line's last character too (`dl` there takes it); on an
    /// empty line, nowhere (`cl` there goes into insert mode).
    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        let len = state.lines.len_col(state.cursor.row).unwrap_or(0);
        state.cursor.col = (state.cursor.col + self.0).min(len);
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        for _ in 0..self.0 {
            if state.cursor.col >= max_col(&state.lines, &state.cursor, state.mode) {
                break;
            }
            state.cursor.col += 1;
        }
        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MoveBackward(pub usize);

impl Execute for MoveBackward {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        for _ in 0..self.0 {
            if state.cursor.col == 0 {
                break;
            }
            let max_col = max_col(&state.lines, &state.cursor, state.mode);
            if state.cursor.col > max_col {
                state.cursor.col = max_col;
            }
            state.cursor.col = state.cursor.col.saturating_sub(1);
        }
        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MoveUp(pub usize);

impl Execute for MoveUp {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Linewise)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        state.cursor.row > 0 && {
            self.execute(state);
            true
        }
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        for _ in 0..self.0 {
            if state.cursor.row == 0 {
                break;
            }
            state.cursor.row = state.cursor.row.saturating_sub(1);
        }
        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MoveDown(pub usize);

impl Execute for MoveDown {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Linewise)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        state.cursor.row + 1 < state.lines.len() && {
            self.execute(state);
            true
        }
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        for _ in 0..self.0 {
            if state.cursor.row >= state.lines.len().saturating_sub(1) {
                break;
            }
            state.cursor.row += 1;
        }
        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

/// Move one word forward, as vim's `w`: over the word the cursor's on and
/// the whitespace after it, to an empty line at most. A word is letters,
/// digits and `_` (in any script), or other marks.
#[derive(Clone, Debug, Copy)]
pub struct MoveWordForward(pub usize);

impl Execute for MoveWordForward {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    /// The last word to its line's end at most (`dw` of a line's last
    /// word leaves the line break).
    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        state.cursor = word::forward(&state.lines, state.cursor, self.0, false, true).0;
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let n = self.0;
        by_words(state, |lines, at| {
            word::forward(lines, at, n, false, false).0
        });
    }
}

/// Move one WORD forward, as vim's `W`: a WORD is anything but whitespace.
/// Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveBigWordForward(pub usize);

impl Execute for MoveBigWordForward {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    /// The last word to its line's end at most (`dw` of a line's last
    /// word leaves the line break).
    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        state.cursor = word::forward(&state.lines, state.cursor, self.0, true, true).0;
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let n = self.0;
        by_words(state, |lines, at| {
            word::forward(lines, at, n, true, false).0
        });
    }
}

/// Move to the end of the word, or the next one's if it's there, as vim's
/// `e`.
#[derive(Clone, Debug, Copy)]
pub struct MoveWordForwardToEndOfWord(pub usize);

impl Execute for MoveWordForwardToEndOfWord {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Inclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        state.cursor = word::end(&state.lines, state.cursor, self.0, false, false);
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let n = self.0;
        by_words(state, |lines, at| word::end(lines, at, n, false, false));
    }
}

/// Move to the end of the WORD, as vim's `E`. Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveBigWordForwardToEndOfWord(pub usize);

impl Execute for MoveBigWordForwardToEndOfWord {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Inclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        state.cursor = word::end(&state.lines, state.cursor, self.0, true, false);
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let n = self.0;
        by_words(state, |lines, at| word::end(lines, at, n, true, false));
    }
}

/// Move one word back, to its start, as vim's `b`: over a line's start to
/// the last word before it, to an empty line at most.
#[derive(Clone, Debug, Copy)]
pub struct MoveWordBackward(pub usize);

impl Execute for MoveWordBackward {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        let (at, ok) = word::backward(&state.lines, state.cursor, self.0, false);
        state.cursor = at;
        ok
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let n = self.0;
        by_words(state, |lines, at| word::backward(lines, at, n, false).0);
    }
}

/// Move one WORD back, as vim's `B`. Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveBigWordBackward(pub usize);

impl Execute for MoveBigWordBackward {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        let (at, ok) = word::backward(&state.lines, state.cursor, self.0, true);
        state.cursor = at;
        ok
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let n = self.0;
        by_words(state, |lines, at| word::backward(lines, at, n, true).0);
    }
}

/// Moves the cursor by words, `to` where they go from it, kept on the line
/// as the mode has it (in normal mode, on a character).
fn by_words(state: &mut EditorState, to: impl FnOnce(&Lines, Index2) -> Index2) {
    if state.lines.is_empty() {
        return;
    }
    state.clamp_column();
    state.cursor = to(&state.lines, state.cursor);
    state.clamp_column();

    if state.mode == EditorMode::Visual {
        set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
    }
}

// Move the cursor to the start of the line.
#[derive(Clone, Debug, Copy)]
pub struct MoveToStartOfLine();

impl Execute for MoveToStartOfLine {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.cursor.col = 0;

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}
// move to the first non-whitespace character in the line.
#[derive(Clone, Debug, Copy)]
pub struct MoveToFirst();

impl Execute for MoveToFirst {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.cursor.col = 0;
        skip_whitespace(&state.lines, &mut state.cursor);

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

/// Moves the cursor `n - 1` lines down, to that line's first non-blank, as
/// vim's `_`. Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveDownToFirst(pub usize);

impl Execute for MoveDownToFirst {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Linewise)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        (self.0 <= 1 || state.cursor.row + 1 < state.lines.len()) && {
            self.execute(state);
            true
        }
    }

    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let last = state.lines.len().saturating_sub(1);
        state.cursor.row = (state.cursor.row + self.0.saturating_sub(1)).min(last);
        state.cursor.col = 0;
        skip_whitespace(&state.lines, &mut state.cursor);

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

// Move the cursor to the end of the line; with a count `n`, `n - 1` lines
// down, as vim's `$` (`2$`), failing on the last line. Count added by
// roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveToEndOfLine(pub usize);

impl Execute for MoveToEndOfLine {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Inclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        (self.0 <= 1 || state.cursor.row + 1 < state.lines.len()) && {
            self.execute(state);
            true
        }
    }

    fn execute(&mut self, state: &mut EditorState) {
        if self.0 > 1 {
            let last = state.lines.len().saturating_sub(1);
            if state.cursor.row >= last {
                state.failed = true;
                return;
            }
            state.cursor.row = (state.cursor.row + self.0 - 1).min(last);
        }
        state.cursor.col = max_col(&state.lines, &state.cursor, state.mode);

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

// Move the cursor to the start of the buffer.
#[derive(Clone, Debug, Copy)]
pub struct MoveToFirstRow(pub Option<usize>);

impl Execute for MoveToFirstRow {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Linewise)
    }

    /// With a count, to that line (`5gg`).
    fn count(&mut self, n: usize) -> bool {
        self.0 = Some(n);
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.cursor.row = to_line(state, self.0).unwrap_or(0);
        state.cursor.col = 0;
        skip_whitespace(&state.lines, &mut state.cursor);

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

// Move the cursor to the end of the buffer.
#[derive(Clone, Debug, Copy)]
pub struct MoveToLastRow(pub Option<usize>);

/// The row of line `n` (counted from 1), the last one past the end.
fn to_line(state: &EditorState, n: Option<usize>) -> Option<usize> {
    Some(
        n?.saturating_sub(1)
            .min(state.lines.len().saturating_sub(1)),
    )
}

impl Execute for MoveToLastRow {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Linewise)
    }

    /// With a count, to that line (`5G`).
    fn count(&mut self, n: usize) -> bool {
        self.0 = Some(n);
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.cursor.row = to_line(state, self.0).unwrap_or(state.lines.len().saturating_sub(1));
        state.cursor.col = 0;
        skip_whitespace(&state.lines, &mut state.cursor);

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

// Move the cursor to the closing bracket.
#[derive(Clone, Debug, Copy)]
pub struct MoveToMatchinBracket();

impl Execute for MoveToMatchinBracket {
    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Inclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        let from = state.cursor;
        self.execute(state);
        state.cursor != from
    }

    fn execute(&mut self, state: &mut EditorState) {
        let max_col = max_col_normal(&state.lines, &state.cursor);
        let index = Index2::new(state.cursor.row, state.cursor.col.min(max_col));
        if let Some(index) = find_matching_bracket(&state.lines, index) {
            state.cursor = index;
            if state.mode == EditorMode::Visual {
                set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
            }
        };
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MoveHalfPageDown();

impl Execute for MoveHalfPageDown {
    fn execute(&mut self, state: &mut EditorState) {
        let jump_rows = state.view.num_rows / 2;
        state.cursor.row = min(state.cursor.row + jump_rows, state.lines.last_row_index());

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MoveHalfPageUp();

impl Execute for MoveHalfPageUp {
    fn execute(&mut self, state: &mut EditorState) {
        let jump_rows = state.view.num_rows / 2;
        state.cursor.row = state.cursor.row.saturating_sub(jump_rows);

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MovePageDown();

impl Execute for MovePageDown {
    fn execute(&mut self, state: &mut EditorState) {
        let jump_rows = state.view.num_rows;
        let max_viewport_y = state.lines.len().saturating_sub(jump_rows);
        let old_viewport_y = state.view.viewport.y;

        state.view.viewport.y = min(state.view.viewport.y + jump_rows, max_viewport_y);

        if old_viewport_y == max_viewport_y {
            state.cursor.row = state.lines.last_row_index();
        } else {
            state.cursor.row = state.view.viewport.y;
        }

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

#[derive(Clone, Debug, Copy)]
pub struct MovePageUp();

impl Execute for MovePageUp {
    fn execute(&mut self, state: &mut EditorState) {
        let jump_rows = state.view.num_rows;
        let old_viewport_y = state.view.viewport.y;

        state.view.viewport.y = state.view.viewport.y.saturating_sub(jump_rows);

        if old_viewport_y == 0 {
            state.cursor.row = 0;
        } else {
            let last_visible_row = state.view.viewport.y + jump_rows.saturating_sub(1);
            state.cursor.row = min(last_visible_row, state.lines.last_row_index());
        }

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

/// Move to the next paragraph boundary (Vim `}`); with a count, the `n`th.
/// A paragraph boundary is the first blank line after a block of non-blank
/// lines, or the end of the buffer. Where the buffer ends with some of a
/// count left, nowhere, as vim's (`2}` in the last paragraph). Count added
/// by roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveParagraphForward(pub usize);

impl Execute for MoveParagraphForward {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    /// With no blank line ahead, to the end of the buffer, its last
    /// character taken too; with some of a count left there, nowhere.
    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        if paragraphs(&state.lines, state.cursor.row, self.0, true).is_none() {
            return false;
        }
        self.execute(state);
        if state.cursor.row + 1 >= state.lines.len() {
            state.cursor.col = state.lines.len_col(state.cursor.row).unwrap_or(0);
        }
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let Some(row) = paragraphs(&state.lines, state.cursor.row, self.0, true) else {
            state.failed = true;
            return;
        };
        state.cursor.row = row;

        if state.lines.is_last_row(state.cursor) {
            state.cursor.col = max_col_normal(&state.lines, &state.cursor);
        } else {
            state.cursor.col = 0;
        };

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

/// Move to the previous paragraph boundary (Vim `{`); with a count, the
/// `n`th, and nowhere where the buffer begins with some of it left. Count
/// added by roster.
#[derive(Clone, Debug, Copy)]
pub struct MoveParagraphBackward(pub usize);

impl Execute for MoveParagraphBackward {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        paragraphs(&state.lines, state.cursor.row, self.0, false).is_some() && {
            self.execute(state);
            true
        }
    }

    fn execute(&mut self, state: &mut EditorState) {
        let Some(row) = paragraphs(&state.lines, state.cursor.row, self.0, false) else {
            state.failed = true;
            return;
        };
        state.cursor.row = row;
        state.cursor.col = 0;

        if state.mode == EditorMode::Visual {
            set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
        }
    }
}

/// The row `n` paragraph boundaries from `row`, down or up, as vim's
/// `findpar` finds them: past a paragraph's lines to the blank line after
/// (before) them, else the buffer's end (start), but none if it ends with
/// some of `n` left.
fn paragraphs(lines: &crate::Lines, mut row: usize, n: usize, down: bool) -> Option<usize> {
    let last = lines.last_row_index();
    let blank = |r: usize| lines.is_empty_row(r).unwrap_or(true);
    for left in (0..n.max(1)).rev() {
        let (mut text, mut first) = (false, true);
        loop {
            text |= !blank(row);
            if !first && text && blank(row) {
                break;
            }
            if (down && row >= last) || (!down && row == 0) {
                if left > 0 {
                    return None;
                }
                break;
            }
            row = if down { row + 1 } else { row - 1 };
            first = false;
        }
    }
    Some(row)
}

/// Moves the cursor to the next occurrence of a character to the right on the
/// current line (Vim `f`); with a count, the `n`th (`2fx`). Does nothing if
/// the character is not found, as many times. Count added by roster.
///
/// The target is `None` until the key handler supplies the next keystroke via
/// [`Execute::char_arg`].
#[derive(Clone, Debug, Copy)]
pub struct FindForward(pub Option<char>, pub usize);

impl Execute for FindForward {
    fn count(&mut self, n: usize) -> bool {
        self.1 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Inclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        self.0
            .and_then(|c| find_nth_forward(state, c, self.1))
            .is_some()
            && {
                self.execute(state);
                true
            }
    }

    fn execute(&mut self, state: &mut EditorState) {
        let Some(target) = self.0 else {
            return;
        };
        if let Some(col) = find_nth_forward(state, target, self.1) {
            state.cursor.col = col;
            if state.mode == EditorMode::Visual {
                set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
            }
        } else {
            state.failed = true;
        }
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// Moves the cursor to just before the next occurrence of a character to the
/// right on the current line (Vim `t`); with a count, the `n`th (`2tx`).
/// Does nothing if the character is not found, as many times. Count added
/// by roster.
///
/// The target is `None` until the key handler supplies the next keystroke via
/// [`Execute::char_arg`].
#[derive(Clone, Debug, Copy)]
pub struct TillForward(pub Option<char>, pub usize);

impl Execute for TillForward {
    fn count(&mut self, n: usize) -> bool {
        self.1 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Inclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        self.0
            .and_then(|c| find_nth_forward(state, c, self.1))
            .is_some()
            && {
                self.execute(state);
                true
            }
    }

    fn execute(&mut self, state: &mut EditorState) {
        let Some(target) = self.0 else {
            return;
        };
        if let Some(col) = find_nth_forward(state, target, self.1) {
            state.cursor.col = col.saturating_sub(1);
            if state.mode == EditorMode::Visual {
                set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
            }
        } else {
            state.failed = true;
        }
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// Moves the cursor to the previous occurrence of a character on its line,
/// as vim's `F`; with a count, the `n`th (`2Fx`). Does nothing if there
/// aren't as many. The character comes as the next key
/// (`Execute::char_arg`). Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct FindBackward(pub Option<char>, pub usize);

impl Execute for FindBackward {
    fn count(&mut self, n: usize) -> bool {
        self.1 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        self.0
            .and_then(|c| find_nth_backward(state, c, self.1))
            .is_some()
            && {
                self.execute(state);
                true
            }
    }

    fn execute(&mut self, state: &mut EditorState) {
        if let Some(col) = self.0.and_then(|c| find_nth_backward(state, c, self.1)) {
            state.cursor.col = col;
            if state.mode == EditorMode::Visual {
                set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
            }
        } else {
            state.failed = true;
        }
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// Moves the cursor to just after the previous occurrence of a character on
/// its line, as vim's `T`; with a count, the `n`th (`2Tx`). Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct TillBackward(pub Option<char>, pub usize);

impl Execute for TillBackward {
    fn count(&mut self, n: usize) -> bool {
        self.1 = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        Some(MotionKind::Exclusive)
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        self.0
            .and_then(|c| find_nth_backward(state, c, self.1))
            .is_some()
            && {
                self.execute(state);
                true
            }
    }

    fn execute(&mut self, state: &mut EditorState) {
        if let Some(col) = self.0.and_then(|c| find_nth_backward(state, c, self.1)) {
            state.cursor.col = col + 1;
            if state.mode == EditorMode::Visual {
                set_selection_with_lines(&mut state.selection, state.cursor, &state.lines);
            }
        } else {
            state.failed = true;
        }
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// The cursor's line's characters.
fn cursor_row(state: &EditorState) -> &[char] {
    state
        .lines
        .get(jagged::index::RowIndex::new(state.cursor.row))
        .map_or(&[][..], Vec::as_slice)
}

/// The column of the `n`th `target` right of the cursor on its line, if
/// there are as many. Added by roster.
pub(crate) fn find_nth_forward(state: &EditorState, target: char, n: usize) -> Option<usize> {
    let chars = cursor_row(state);
    (state.cursor.col + 1..chars.len())
        .filter(|&col| chars[col] == target)
        .nth(n.max(1) - 1)
}

/// The column of the `n`th `target` left of the cursor on its line, if
/// there are as many. Added by roster.
pub(crate) fn find_nth_backward(state: &EditorState, target: char, n: usize) -> Option<usize> {
    let chars = cursor_row(state);
    (0..state.cursor.col.min(chars.len()))
        .rev()
        .filter(|&col| chars[col] == target)
        .nth(n.max(1) - 1)
}

/// Returns the column of the next occurrence of `target` to the right of the
/// cursor on the current line, if any.
pub(crate) fn find_char_forward(state: &EditorState, target: char) -> Option<usize> {
    let row = state.cursor.row;
    let start = Index2::new(row, state.cursor.col + 1);
    for (ch, index) in state.lines.iter().from(start) {
        if index.row != row {
            break;
        }
        if ch == Some(&target) {
            return Some(index.col);
        }
    }
    None
}

#[derive(Debug, Clone, Eq)]
pub(crate) enum CharacterClass {
    Unknown,
    Alphanumeric,
    Punctuation,
    Whitespace,
}

impl From<&char> for CharacterClass {
    fn from(value: &char) -> Self {
        // Underscore counts as a word character (matching Vim's `iskeyword`),
        if value.is_ascii_alphanumeric() || *value == '_' {
            return Self::Alphanumeric;
        }
        if value.is_ascii_punctuation() {
            return Self::Punctuation;
        }
        if value.is_ascii_whitespace() {
            return Self::Whitespace;
        }
        Self::Unknown
    }
}

impl From<Option<&char>> for CharacterClass {
    fn from(value: Option<&char>) -> Self {
        value.map_or(CharacterClass::Unknown, Self::from)
    }
}

impl PartialEq for CharacterClass {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (CharacterClass::Unknown, _) | (_, CharacterClass::Unknown) => false,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Index2, Lines};

    use super::*;
    fn test_state() -> EditorState {
        EditorState::new(Lines::from("Hello World!\n\n123."))
    }

    #[test]
    fn test_move_forward() {
        let mut state = test_state();

        MoveForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));

        MoveForward(10).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));

        MoveForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));
    }

    #[test]
    fn test_move_backward() {
        let mut state = test_state();
        state.cursor = Index2::new(0, 11);

        MoveBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));

        MoveBackward(10).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));

        MoveBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_move_down() {
        let mut state = test_state();
        state.cursor = Index2::new(0, 6);

        MoveDown(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 6));

        MoveDown(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 6));

        MoveDown(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 6));
    }

    #[test]
    fn test_move_up() {
        let mut state = test_state();
        state.cursor = Index2::new(2, 2);

        MoveUp(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 2));

        MoveUp(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));

        MoveUp(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
    }

    #[test]
    fn test_move_word_forward() {
        let mut state = test_state();

        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 6));

        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));

        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));

        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));

        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 3));
    }

    #[test]
    fn test_move_word_forward_with_punctuation() {
        let mut state = EditorState::new(Lines::from("forward (w)"));

        // Start at 'f', move forward through "forward" and skip space to land on '('
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 8));

        // At '(', move through '(' and skip space (none) to land on 'w'
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 9));

        // At 'w', move through 'w' to land on ')'
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));
    }

    #[test]
    fn test_move_word_forward_punctuation_detailed() {
        // Test the exact case from the bug report
        let mut state = EditorState::new(Lines::from("forward (w)"));

        // Start at 'f' (position 0)
        assert_eq!(state.cursor, Index2::new(0, 0));

        // First move: from 'f' through "forward" to '('
        MoveWordForward(1).execute(&mut state);
        assert_eq!(
            state.cursor,
            Index2::new(0, 8),
            "Should move from 'f' to '(' after skipping whitespace"
        );

        // Second move: from '(' to 'w' (this was the bug - it went to ')' instead)
        MoveWordForward(1).execute(&mut state);
        assert_eq!(
            state.cursor,
            Index2::new(0, 9),
            "Should move from '(' to 'w', not to ')'"
        );

        // Third move: from 'w' to ')'
        MoveWordForward(1).execute(&mut state);
        assert_eq!(
            state.cursor,
            Index2::new(0, 10),
            "Should move from 'w' to ')'"
        );

        // Test with multiple punctuation characters
        let mut state = EditorState::new(Lines::from("hello()world"));

        // From 'h' to '('
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 5));

        // From '(' through ')' to 'w' (consecutive punctuation treated as one word)
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 7));

        // From 'w' through "world" to end
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));
    }

    #[test]
    fn test_move_word_forward_single_line() {
        // "Hello World" - should land on 'W' first, then on 'd' (end of line)
        let mut state = EditorState::new(Lines::from("Hello World"));

        // Start at 'H', move to 'W'
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 6));

        // From 'W', move to end of "World" (position 10, the 'd')
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));

        // From 'd', should stay at 'd' (already at end)
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));
    }

    #[test]
    fn test_move_word_forward_single_line_insert_mode() {
        // In insert mode, cursor should land AFTER 'd' (position 11)
        let mut state = EditorState::new(Lines::from("Hello World"));
        state.mode = EditorMode::Insert;

        // Start at 'H', move to 'W'
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 6));

        // From 'W', move to position after "World" (position 11, after 'd')
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));
    }

    #[test]
    fn test_move_word_forward_out_of_bounds() {
        let mut state = test_state();

        state.cursor = Index2::new(0, 99);
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));
    }

    #[test]
    fn test_move_page_down() {
        // After page down, viewport scrolls and cursor is at top of new viewport (screen row 0)
        let mut state = EditorState::new(Lines::from(
            "Line 0\nLine 1\nLine 2\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9",
        ));
        state.view.num_rows = 3; // Viewport shows 3 lines

        // Start at line 0, viewport at 0
        assert_eq!(state.cursor.row, 0);
        assert_eq!(state.view.viewport.y, 0);

        // Page down: viewport scrolls to line 3, cursor at top of viewport (line 3)
        MovePageDown().execute(&mut state);
        assert_eq!(state.view.viewport.y, 3);
        assert_eq!(state.cursor.row, 3); // cursor at screen row 0

        // Page down: viewport scrolls to line 6, cursor at top of viewport (line 6)
        MovePageDown().execute(&mut state);
        assert_eq!(state.view.viewport.y, 6);
        assert_eq!(state.cursor.row, 6); // cursor at screen row 0

        // Page down: viewport can only go to line 7 (10 lines - 3 visible = max 7)
        // cursor at top of viewport (line 7)
        MovePageDown().execute(&mut state);
        assert_eq!(state.view.viewport.y, 7);
        assert_eq!(state.cursor.row, 7); // cursor at screen row 0

        // Page down again: already at bottom, cursor moves to last line
        MovePageDown().execute(&mut state);
        assert_eq!(state.view.viewport.y, 7);
        assert_eq!(state.cursor.row, 9); // cursor at last line
    }

    #[test]
    fn test_move_page_up() {
        // After page up, viewport scrolls and cursor is at bottom of new viewport
        let mut state = EditorState::new(Lines::from(
            "Line 0\nLine 1\nLine 2\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9",
        ));
        state.view.num_rows = 3; // Viewport shows 3 lines
        state.cursor.row = 9; // Start at last line
        state.view.viewport.y = 7; // Viewport showing lines 7-9

        // Page up: viewport scrolls to line 4, cursor at bottom of viewport (line 6)
        MovePageUp().execute(&mut state);
        assert_eq!(state.view.viewport.y, 4);
        assert_eq!(state.cursor.row, 6); // cursor at screen row 2 (bottom)

        // Page up: viewport scrolls to line 1, cursor at bottom of viewport (line 3)
        MovePageUp().execute(&mut state);
        assert_eq!(state.view.viewport.y, 1);
        assert_eq!(state.cursor.row, 3); // cursor at screen row 2 (bottom)

        // Page up: viewport scrolls to line 0, cursor at bottom of viewport (line 2)
        MovePageUp().execute(&mut state);
        assert_eq!(state.view.viewport.y, 0);
        assert_eq!(state.cursor.row, 2); // cursor at screen row 2 (bottom)

        // Page up again: already at top, cursor moves to first line
        MovePageUp().execute(&mut state);
        assert_eq!(state.view.viewport.y, 0);
        assert_eq!(state.cursor.row, 0); // cursor at first line
    }

    #[test]
    fn test_move_word_forward_to_end_of_word() {
        let mut state = test_state();

        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));

        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));

        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));

        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 2));

        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 3));

        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 3));
    }

    #[test]
    fn test_move_word_backward() {
        let mut state = test_state();
        state.cursor = Index2::new(2, 3);

        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));

        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));

        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));

        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 6));

        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));

        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_word_motions_are_vims() {
        // w onto the next line's first word; b from a line's start to the
        // last word before it.
        let mut state = EditorState::new(Lines::from("one two\n  three"));
        state.cursor = Index2::new(0, 4);
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 2));
        state.cursor = Index2::new(1, 0);
        MoveWordBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));
        // Letters beyond ASCII are one word's.
        let mut state = EditorState::new(Lines::from("zażółć gęślą"));
        MoveWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 7));
        MoveWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));
        MoveWordBackward(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_big_word_motions_take_marks_in() {
        let mut state = EditorState::new(Lines::from("a foo.bar(x) b"));
        MoveBigWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
        MoveBigWordForwardToEndOfWord(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));
        MoveBigWordForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 13));
        MoveBigWordBackward(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        // With a count; to the end in normal mode at most.
        MoveBigWordForward(9).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 13));
    }

    #[test]
    fn test_find_and_till_backward() {
        let mut state = EditorState::new(Lines::from("a,b,c"));
        state.cursor = Index2::new(0, 4);
        FindBackward(Some(','), 1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 3));
        FindBackward(Some(','), 1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        // None before: nothing.
        FindBackward(Some(','), 1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        state.cursor = Index2::new(0, 4);
        TillBackward(Some('a'), 1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        // With a count, the nth; past what there is, nowhere.
        state.cursor = Index2::new(0, 4);
        FindBackward(Some(','), 2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        state.cursor = Index2::new(0, 4);
        TillBackward(Some(','), 2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
        FindBackward(Some(','), 3).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
    }

    #[test]
    fn test_move_down_to_first() {
        let mut state = EditorState::new(Lines::from("  one\n   two\nsix"));
        state.cursor = Index2::new(0, 4);
        MoveDownToFirst(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
        MoveDownToFirst(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 3));
        MoveDownToFirst(9).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));
    }

    #[test]
    fn test_move_to_start() {
        let mut state = test_state();
        state.cursor = Index2::new(0, 2);

        MoveToStartOfLine().execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_move_to_end() {
        let mut state = test_state();
        state.cursor = Index2::new(0, 2);

        MoveToEndOfLine(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 11));
    }

    #[test]
    fn test_move_to_first() {
        let mut state = EditorState::new(Lines::from(" Hello"));
        state.cursor = Index2::new(0, 3);

        MoveToFirst().execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
    }

    fn paragraph_state() -> EditorState {
        // Lines:
        //   0: "first paragraph"
        //   1: "still first"
        //   2: ""
        //   3: ""
        //   4: ""
        //   5: "second paragraph"
        //   6: ""
        //   7: ""
        //   8: "third paragraph"
        EditorState::new(Lines::from(
            "first paragraph\nstill first\n\n\n\nsecond paragraph\n\n\nthird paragraph",
        ))
    }

    #[test]
    fn test_move_paragraph_forward() {
        let mut state = paragraph_state();
        assert_eq!(state.cursor, Index2::new(0, 0));

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(6, 0));

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(8, 14));

        // Already at last line
        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(8, 14));
    }

    #[test]
    fn test_move_paragraph_backward() {
        let mut state = paragraph_state();
        state.cursor = Index2::new(8, 0);

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(7, 0));

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(4, 0));

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));

        // Already at first line
        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_move_paragraph_forward_from_middle() {
        let mut state = paragraph_state();
        state.cursor = Index2::new(1, 5);

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));
    }

    #[test]
    fn test_move_paragraph_backward_from_middle() {
        let mut state = paragraph_state();
        state.cursor = Index2::new(5, 5);

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(4, 0));
    }

    #[test]
    fn test_move_paragraph_forward_visual_extends_selection() {
        use crate::actions::SwitchMode;
        let mut state = paragraph_state();
        SwitchMode(EditorMode::Visual).execute(&mut state);

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));
        assert!(state.selection.is_some());
        let sel = state.selection.unwrap();
        assert_eq!(sel.start, Index2::new(0, 0));
    }

    #[test]
    fn test_move_paragraph_single_line() {
        let mut state = EditorState::new(Lines::from("just one line"));

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 12));

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_move_paragraph_single_line_visual_selects_to_boundaries() {
        use crate::actions::SwitchMode;
        let mut state = EditorState::new(Lines::from("just one line"));
        state.cursor = Index2::new(0, 5);
        SwitchMode(EditorMode::Visual).execute(&mut state);

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        let sel = state.selection.clone().unwrap();
        assert_eq!(sel.start, Index2::new(0, 5));
        assert_eq!(sel.end, Index2::new(0, 0));

        SwitchMode(EditorMode::Normal).execute(&mut state);
        state.cursor = Index2::new(0, 5);
        SwitchMode(EditorMode::Visual).execute(&mut state);

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 12));
        let sel = state.selection.unwrap();
        assert_eq!(sel.start, Index2::new(0, 5));
        assert_eq!(sel.end, Index2::new(0, 12));
    }

    #[test]
    fn test_move_paragraph_all_blank() {
        let mut state = EditorState::new(Lines::from("\n\n\n"));
        assert_eq!(state.cursor, Index2::new(0, 0));

        MoveParagraphForward(1).execute(&mut state);
        assert_eq!(state.cursor.row, 3);

        MoveParagraphBackward(1).execute(&mut state);
        assert_eq!(state.cursor.row, 0);
    }

    #[test]
    fn test_move_paragraphs_counted_and_none_past_the_end() {
        let mut state = paragraph_state();
        MoveParagraphForward(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(6, 0));
        MoveParagraphBackward(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        // To the end with the last of the count: there; with some left, or
        // from the end: nowhere.
        MoveParagraphForward(3).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(8, 14));
        state.cursor = Index2::new(5, 3);
        MoveParagraphForward(3).execute(&mut state);
        assert_eq!((state.cursor, state.failed), (Index2::new(5, 3), true));
        let mut state = EditorState::new(Lines::from("foo bar baz"));
        MoveParagraphForward(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        MoveParagraphBackward(2).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }
}
