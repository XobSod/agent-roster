use jagged::index::RowIndex;

use super::insert::{record, BACKSPACE, DELETE, DELETE_LINE, DELETE_WORD};
use super::word::kind;
use super::Execute;
use crate::{
    actions::motion::{find_char_forward, CharacterClass},
    clipboard::ClipboardTrait,
    helper::{
        cut_chars, is_out_of_bounds, max_col_insert, max_col_normal, skip_whitespace,
        skip_whitespace_rev,
    },
    state::selection::Selection,
    EditorState, Index2, Lines,
};

/// Deletes `n` characters from the cursor on (fewer where the line ends
/// first) and yanks them, as vim's `x`. Does not move the cursor unless it
/// was at the end of the line. On an empty line it does nothing: no undo
/// point, and the clipboard stays as it was.
/// Intended to be called in normal mode.
#[derive(Clone, Debug, Copy)]
pub struct RemoveChar(pub usize);

impl Execute for RemoveChar {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.clamp_column();
        let index = state.cursor;
        if is_out_of_bounds(&state.lines, &index) {
            return;
        }
        state.capture();
        let Some(row) = state.lines.get_mut(RowIndex::new(index.row)) else {
            return;
        };
        let end = (index.col + self.0.max(1)).min(row.len());
        let taken: String = row.drain(index.col..end).collect();
        let left = row.len();
        state.clip.set_text(taken);
        state.cursor.col = index.col.min(left.saturating_sub(1));
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Deletes `n` characters before the cursor (fewer near the line's start)
/// and yanks them, as vim's `X`. At the start of a line it does nothing: no
/// undo point, and the clipboard stays as it was.
/// Intended to be called in normal mode.
#[derive(Clone, Debug, Copy)]
pub struct RemoveCharBefore(pub usize);

impl Execute for RemoveCharBefore {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.clamp_column();
        let index = state.cursor;
        if index.col == 0 || is_out_of_bounds(&state.lines, &index) {
            return;
        }
        state.capture();
        let start = index.col.saturating_sub(self.0.max(1));
        let Some(row) = state.lines.get_mut(RowIndex::new(index.row)) else {
            return;
        };
        let taken: String = row.drain(start..index.col).collect();
        state.clip.set_text(taken);
        state.cursor.col = start;
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Replaces the character under the cursor with a given character.
/// Intended to be called in normal mode.
#[derive(Clone, Debug, Copy)]
pub struct ReplaceChar(pub char);

impl Execute for ReplaceChar {
    fn execute(&mut self, state: &mut EditorState) {
        let index = state.cursor;
        if is_out_of_bounds(&state.lines, &index) {
            return;
        }
        state.capture();
        if let Some(ch) = state.lines.get_mut(index) {
            *ch = self.0;
        };
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Replaces the character under the cursor with the next key's, as vim's
/// `r`, and with a count as many (`3rx`), the cursor on the last; none if
/// fewer are left on the line. Its character is to come
/// (`Execute::char_arg`), which a repeat (`.`) keeps.
#[derive(Clone, Debug, Copy)]
pub struct Replace(pub Option<char>, pub usize);

impl Execute for Replace {
    fn count(&mut self, n: usize) -> bool {
        self.1 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let Some(c) = self.0 else {
            return;
        };
        state.clamp_column();
        let (row, col, n) = (state.cursor.row, state.cursor.col, self.1.max(1));
        if col + n > state.lines.len_col(row).unwrap_or(0) {
            state.failed = true;
            return;
        }
        state.capture();
        if let Some(chars) = state.lines.get_mut(RowIndex::new(row)) {
            chars[col..col + n].fill(c);
        }
        state.cursor.col = col + n - 1;
    }

    fn is_repeatable(&self) -> bool {
        true
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// Deletes a character to the left of the current cursor. Deletes
/// the line break if the the cursor is in column zero.
/// Intended to be called in insert mode.
#[derive(Clone, Debug, Copy)]
pub struct DeleteChar(pub usize);

impl Execute for DeleteChar {
    fn execute(&mut self, state: &mut EditorState) {
        // In an insert session, its undo point holds this too (roster's).
        if state.insert_start.is_none() {
            state.capture();
        }
        for _ in 0..self.0 {
            delete_char(&mut state.lines, &mut state.cursor);
        }
        record(state, BACKSPACE, self.0);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

fn delete_char(lines: &mut Lines, index: &mut Index2) {
    fn move_left(lines: &Lines, index: &mut Index2) {
        if index.col > 0 {
            index.col -= 1;
        } else if index.row > 0 {
            index.row -= 1;
            index.col = lines.len_col(index.row).unwrap_or_default();
        }
    }

    let len_col = lines.len_col(index.row).unwrap_or_default();
    if len_col == 0 && index.row == 0 {
        return;
    }

    if index.col > len_col {
        index.col = len_col;
    }

    if index.col == 0 {
        let mut rest = lines.split_off(*index);
        move_left(lines, index);
        lines.merge(&mut rest);
    } else {
        let max_col = max_col_insert(lines, index);
        index.col = index.col.min(max_col);
        move_left(lines, index);
        let _ = lines.remove(*index);
    }
}

/// Deletes the character at the current cursor position.
/// If at the end of a line, deletes the newline character.
/// Intended to be called in insert mode.
#[derive(Clone, Debug, Copy)]
pub struct DeleteCharForward(pub usize);

impl Execute for DeleteCharForward {
    fn execute(&mut self, state: &mut EditorState) {
        if state.insert_start.is_none() {
            state.capture();
        }
        state.clamp_column();
        for _ in 0..self.0 {
            delete_char_forward(&mut state.lines, &mut state.cursor);
        }
        record(state, DELETE, self.0);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

fn delete_char_forward(lines: &mut Lines, index: &mut Index2) {
    let Some(row) = lines.get(RowIndex::new(index.row)) else {
        return;
    };

    let row_len = row.len();

    // If cursor is at or past the end of the line, delete the newline
    if index.col >= row_len {
        if index.row + 1 >= lines.len() {
            return;
        }

        lines.join_lines(index.row);
        return;
    }

    let _ = lines.remove(*index);
}

/// Deletes from cursor to the end of the current word (Emacs Alt+d).
#[derive(Clone, Debug, Copy)]
pub struct DeleteWordForward(pub usize);

impl Execute for DeleteWordForward {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        if state.lines.is_empty() {
            return;
        }
        state.capture();
        for _ in 0..self.0 {
            delete_word_forward(state);
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

fn delete_motion_forward<F>(
    state: &mut EditorState,
    mut should_continue: F,
    skip_trailing_whitespace: bool,
) where
    F: FnMut(Option<&char>, Option<&char>) -> bool,
{
    let mut start = state.cursor;
    let len_col = state.lines.len_col(start.row).unwrap_or_default();

    // On an empty line there is nothing to delete
    if len_col == 0 {
        if start.row + 1 < state.lines.len() {
            state.lines.join_lines(start.row);
        }
        return;
    }

    // Clamp the cursor when it sits past the end of the line
    start.col = start.col.min(max_col_normal(&state.lines, &start));
    state.cursor = start;

    let start_char = state.lines.get(start);
    let mut end = start;

    for (ch, idx) in state.lines.iter().from(start) {
        // Stop at the end of the current line
        // A forward word delete must not cross into the next line.
        if idx.row != start.row || !should_continue(start_char, ch) {
            break;
        }
        end = idx;
    }
    end.col += 1;

    if skip_trailing_whitespace {
        skip_whitespace(&state.lines, &mut end);
    }
    delete_range(&mut state.lines, start, end, &mut state.clip);

    state.cursor.col = state
        .cursor
        .col
        .min(max_col_normal(&state.lines, &state.cursor));
}

/// Deletes from cursor forward to the next word boundary (Vim `dw`).
fn delete_word_forward(state: &mut EditorState) {
    delete_motion_forward(
        state,
        |a, b| CharacterClass::from(a) == CharacterClass::from(b),
        true,
    );
}

/// Deletes from cursor forward to the next WORD boundary (Vim `dW`).
/// A WORD is any sequence of non-whitespace characters.
#[derive(Clone, Debug, Copy)]
pub struct DeleteBigWordForward(pub usize);

impl Execute for DeleteBigWordForward {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        if state.lines.is_empty() {
            return;
        }
        state.capture();
        for _ in 0..self.0 {
            delete_big_word_forward(state);
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

fn delete_big_word_forward(state: &mut EditorState) {
    delete_motion_forward(
        state,
        |a, b| {
            let a_ws = CharacterClass::from(a) == CharacterClass::Whitespace;
            let b_ws = CharacterClass::from(b) == CharacterClass::Whitespace;
            a_ws == b_ws
        },
        true,
    );
}

/// Deletes from the cursor to the end of the current word, without consuming
/// trailing whitespace. This is the `cw` primitive, which in Vim behaves like
/// `ce` rather than `dw`.
#[derive(Clone, Debug, Copy)]
pub struct DeleteWordEnd(pub usize);

impl Execute for DeleteWordEnd {
    fn execute(&mut self, state: &mut EditorState) {
        if state.lines.is_empty() {
            return;
        }
        state.capture();
        for _ in 0..self.0 {
            delete_word_end(state);
        }
    }
}

fn delete_word_end(state: &mut EditorState) {
    delete_motion_forward(
        state,
        |a, b| CharacterClass::from(a) == CharacterClass::from(b),
        false,
    );
}

/// Deletes from the cursor to the end of the current WORD, without consuming
/// trailing whitespace. This is the `cW` primitive, which in Vim behaves like
/// `cE` rather than `dW`. A WORD is any sequence of non-whitespace characters.
#[derive(Clone, Debug, Copy)]
pub struct DeleteBigWordEnd(pub usize);

impl Execute for DeleteBigWordEnd {
    fn execute(&mut self, state: &mut EditorState) {
        if state.lines.is_empty() {
            return;
        }
        state.capture();
        for _ in 0..self.0 {
            delete_big_word_end(state);
        }
    }
}

fn delete_big_word_end(state: &mut EditorState) {
    delete_motion_forward(
        state,
        |a, b| {
            let a_ws = CharacterClass::from(a) == CharacterClass::Whitespace;
            let b_ws = CharacterClass::from(b) == CharacterClass::Whitespace;
            a_ws == b_ws
        },
        false,
    );
}

/// Deletes the word before the cursor in insert mode, as vim's Ctrl+W: the
/// whitespace before it too, not into the clipboard, stopping once where
/// the insert began; at a line's start, the line break. Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct DeleteWordBefore;

impl Execute for DeleteWordBefore {
    fn execute(&mut self, state: &mut EditorState) {
        if state.insert_start.is_none() {
            state.capture();
        }
        let at = state.cursor;
        if at.col == 0 {
            delete_char(&mut state.lines, &mut state.cursor);
        } else if let Some(chars) = state.lines.get_mut(RowIndex::new(at.row)) {
            let end = at.col.min(chars.len());
            let mut start = end;
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
            if let Some(&c) = start.checked_sub(1).and_then(|i| chars.get(i)) {
                let class = kind(c, false);
                while start > 0 && kind(chars[start - 1], false) == class {
                    start -= 1;
                }
            }
            if let Some(begin) = state.insert_start {
                if begin.row == at.row && start < begin.col && begin.col < end {
                    start = begin.col;
                }
            }
            chars.drain(start..end);
            state.cursor.col = start;
        }
        record(state, DELETE_WORD, 1);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Deletes what's before the cursor on its line in insert mode, as vim's
/// Ctrl+U: what was typed in this insert, else all of it (stopping once
/// where the insert began), not into the clipboard; at a line's start, the
/// line break. Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct DeleteLineBefore;

impl Execute for DeleteLineBefore {
    fn execute(&mut self, state: &mut EditorState) {
        if state.insert_start.is_none() {
            state.capture();
        }
        let at = state.cursor;
        if at.col == 0 {
            delete_char(&mut state.lines, &mut state.cursor);
        } else if let Some(chars) = state.lines.get_mut(RowIndex::new(at.row)) {
            let end = at.col.min(chars.len());
            let start = match state.insert_start {
                Some(begin) if begin.row == at.row && begin.col < end => begin.col,
                _ => 0,
            };
            chars.drain(start..end);
            state.cursor.col = start;
        }
        record(state, DELETE_LINE, 1);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Deletes from cursor backward to start of previous word (Emacs Alt+Backspace).
#[derive(Clone, Debug, Copy)]
pub struct DeleteWordBackward(pub usize);

impl Execute for DeleteWordBackward {
    fn execute(&mut self, state: &mut EditorState) {
        if state.lines.is_empty() {
            return;
        }
        state.capture();
        for _ in 0..self.0 {
            delete_word_backward(state);
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

fn delete_word_backward(state: &mut EditorState) {
    let end = state.cursor;

    if end.row == 0 && end.col == 0 {
        return;
    }

    if end.col == 0 {
        state.cursor.row -= 1;
        state.cursor.col = state.lines.len_col(state.cursor.row).unwrap_or(0);
        state.lines.join_lines(state.cursor.row);
        return;
    }

    let mut start = Index2::new(end.row, end.col.saturating_sub(1));
    skip_whitespace_rev(&state.lines, &mut start);
    let start_class = CharacterClass::from(state.lines.get(start));

    for (ch, idx) in state.lines.iter().from(start).rev() {
        if idx.col == 0 {
            start = idx;
            break;
        }
        if CharacterClass::from(ch) != start_class {
            break;
        }
        start = idx;
    }

    delete_range(&mut state.lines, start, end, &mut state.clip);
    state.cursor = start;
}

fn delete_range(
    lines: &mut Lines,
    start: Index2,
    end: Index2,
    clip: &mut crate::clipboard::Clipboard,
) {
    if start.row != end.row || start.col >= end.col {
        return;
    }

    let Some(row) = lines.get_mut(RowIndex::new(start.row)) else {
        return;
    };

    let end_col = end.col.min(row.len());
    let start_col = start.col.min(end_col);

    let deleted: String = row.drain(start_col..end_col).collect();
    clip.set_text(deleted);
}

/// Deletes the current line.
#[derive(Clone, Debug, Copy)]
pub struct DeleteLine(pub usize);

impl Execute for DeleteLine {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        state.capture();
        // All of them into the clipboard, as lines.
        let mut deleted = String::new();
        for _ in 0..self.0 {
            if state.cursor.row >= state.lines.len() {
                break;
            }
            let row_index = RowIndex::new(state.cursor.row);
            let deleted_line = state.lines.remove(row_index).iter().collect::<String>();
            deleted.push('\n');
            deleted.push_str(&deleted_line);
            state.cursor.col = 0;
            state.cursor.row = state.cursor.row.min(state.lines.len().saturating_sub(1));
        }
        if !deleted.is_empty() {
            state.clip.set_text(deleted);
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Deletes from the current cursor position to the first non-whitespace character of the line
#[derive(Clone, Debug, Copy)]
pub struct DeleteToFirstCharOfLine;

impl Execute for DeleteToFirstCharOfLine {
    fn execute(&mut self, state: &mut EditorState) {
        if state.insert_start.is_none() {
            state.capture();
        }

        let row_index = RowIndex::new(state.cursor.row);
        let Some(row) = state.lines.get_mut(row_index) else {
            return;
        };

        let col = state.cursor.col;

        let first_char = row
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(row.len());

        let anchor = if col <= first_char { 0 } else { first_char };

        if anchor < col && col <= row.len() {
            let deleted = row.drain(anchor..col).collect();
            state.clip.set_text(deleted);
        }

        state.cursor.col = anchor;
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Deletes from the current cursor position to the end of the line
#[derive(Clone, Debug, Copy)]
pub struct DeleteToEndOfLine;

impl Execute for DeleteToEndOfLine {
    fn execute(&mut self, state: &mut EditorState) {
        if is_out_of_bounds(&state.lines, &state.cursor) {
            return;
        }
        state.capture();
        let Some(row) = state.lines.get_mut(RowIndex::new(state.cursor.row)) else {
            return;
        };
        let deleted_chars = row.drain(state.cursor.col..);
        state.cursor.col = state.cursor.col.saturating_sub(1);
        state.clip.set_text(deleted_chars.collect());
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Deletes from the cursor up to and including the next occurrence of a
/// character on the current line (Vim `df<char>`). Does nothing if the
/// character is not found.
///
/// The target is `None` until the key handler supplies the next keystroke via
/// [`Execute::char_arg`].
#[derive(Clone, Debug, Copy)]
pub struct DeleteFindForward(pub Option<char>);

impl Execute for DeleteFindForward {
    fn execute(&mut self, state: &mut EditorState) {
        let Some(target_char) = self.0 else {
            return;
        };
        let Some(target) = find_char_forward(state, target_char) else {
            return;
        };
        state.capture();
        let Some(row) = state.lines.get_mut(RowIndex::new(state.cursor.row)) else {
            return;
        };
        let deleted = row.drain(state.cursor.col..=target).collect();
        state.clip.set_text(deleted);
        state.clamp_column();
    }

    fn is_repeatable(&self) -> bool {
        true
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// Deletes from the cursor up to (but not including) the next occurrence of a
/// character on the current line (Vim `dt<char>`). Does nothing if the
/// character is not found.
///
/// The target is `None` until the key handler supplies the next keystroke via
/// [`Execute::char_arg`].
#[derive(Clone, Debug, Copy)]
pub struct DeleteTillForward(pub Option<char>);

impl Execute for DeleteTillForward {
    fn execute(&mut self, state: &mut EditorState) {
        let Some(target_char) = self.0 else {
            return;
        };
        let Some(target) = find_char_forward(state, target_char) else {
            return;
        };
        state.capture();
        let Some(row) = state.lines.get_mut(RowIndex::new(state.cursor.row)) else {
            return;
        };
        let deleted = row.drain(state.cursor.col..target).collect();
        state.clip.set_text(deleted);
        state.clamp_column();
    }

    fn is_repeatable(&self) -> bool {
        true
    }

    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        Some(&mut self.0)
    }
}

/// Deletes the current selection.
#[derive(Clone, Debug)]
pub struct DeleteSelection;

impl Execute for DeleteSelection {
    fn execute(&mut self, state: &mut EditorState) {
        if let Some(selection) = state.selection.take() {
            state.capture();
            let drained = delete_selection(state, &selection);
            state.clip.set_text(drained.into());
            // As vim leaves it: a line at least. Added by roster.
            if state.lines.is_empty() {
                state.lines.push(vec![]);
            }
        }
        state.selection = None;
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

pub(crate) fn delete_selection(state: &mut EditorState, selection: &Selection) -> Lines {
    let (start, end) = (selection.start(), selection.end());
    let taken = if selection.line_mode {
        selection.extract_from(&mut state.lines)
    } else {
        // The line break after `end` only if `end` is past its line's last
        // character (`v$`), not on it: `extract_from` took it then too.
        // Added by roster.
        let len = state.lines.len_col(end.row).unwrap_or(0);
        let past = if end.col < len {
            Index2::new(end.row, end.col + 1)
        } else if end.row + 1 < state.lines.len() {
            Index2::new(end.row + 1, 0)
        } else {
            Index2::new(end.row, len)
        };
        Lines::from(cut_chars(&mut state.lines, start, past))
    };
    // On a line still there, the last if the last ones went.
    state.cursor = start;
    state.cursor.row = start.row.min(state.lines.len().saturating_sub(1));
    state.clamp_column();
    taken
}

/// Joins line below to the current line.
#[derive(Clone, Debug, Copy)]
pub struct JoinLineWithLineBelow;

impl Execute for JoinLineWithLineBelow {
    fn execute(&mut self, state: &mut EditorState) {
        if state.cursor.row + 1 >= state.lines.len() {
            return;
        }
        state.capture();
        state.lines.join_lines(state.cursor.row);
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Joins `n` lines from the cursor's into one, as vim's `J`: each loses its
/// indent, and a space goes between (none after a space, before a `)`, or
/// with an empty line). `J` is two, as `2J`: vim's count counts the
/// cursor's line. The cursor goes where the last two met. Added by roster.
#[derive(Clone, Debug, Copy)]
pub struct JoinLines(pub usize);

impl Execute for JoinLines {
    fn count(&mut self, n: usize) -> bool {
        self.0 = n;
        true
    }

    fn execute(&mut self, state: &mut EditorState) {
        let row = state.cursor.row;
        let below = state.lines.len().saturating_sub(row + 1);
        let joins = (self.0.max(2) - 1).min(below);
        if joins == 0 {
            // Nothing below: nothing done, nor for `.` to repeat.
            state.failed = true;
            return;
        }
        state.capture();
        for _ in 0..joins {
            let next = state.lines.remove(RowIndex::new(row + 1));
            let next: Vec<char> = next.into_iter().skip_while(|c| c.is_whitespace()).collect();
            let Some(line) = state.lines.get_mut(RowIndex::new(row)) else {
                return;
            };
            state.cursor.col = line.len();
            let space = !matches!(line.last(), None | Some(' ' | '\t'))
                && !matches!(next.first(), None | Some(')'));
            if space {
                line.push(' ');
            }
            line.extend(next);
        }
        state.clamp_column();
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::state::selection::Selection;
    use crate::EditorMode;
    use crate::Index2;
    use crate::Lines;

    use super::*;
    fn test_state() -> EditorState {
        EditorState::new(Lines::from("Hello World!\n\n123."))
    }

    #[test]
    fn test_join_lines_as_vims_j() {
        let mut state = EditorState::new(Lines::from("one\n  two\nthree"));
        JoinLines(2).execute(&mut state);
        assert_eq!(state.lines, Lines::from("one two\nthree"));
        assert_eq!(state.cursor, Index2::new(0, 3));
        // A count counts the cursor's line: 3J joins three.
        let mut state = EditorState::new(Lines::from("a\nb\nc\nd"));
        JoinLines(3).execute(&mut state);
        assert_eq!(state.lines, Lines::from("a b c\nd"));
        assert_eq!(state.cursor, Index2::new(0, 3));
        // No space after one, before a `)`, or with an empty line; as many
        // as there are.
        let mut state = EditorState::new(Lines::from("f(a, \nb\n)\n\nx"));
        JoinLines(9).execute(&mut state);
        assert_eq!(state.lines, Lines::from("f(a, b) x"));
        assert_eq!(state.cursor, Index2::new(0, 7));
        // On the last line, nothing.
        JoinLines(2).execute(&mut state);
        assert_eq!(state.lines, Lines::from("f(a, b) x"));
    }

    #[test]
    fn test_remove_char_yanks_what_it_takes() {
        use crate::clipboard::InternalClipboard;
        let mut state = test_state();
        state.set_clipboard(InternalClipboard::default());
        state.cursor = Index2::new(0, 3);
        RemoveChar(3).execute(&mut state);
        assert_eq!(state.lines, Lines::from("HelWorld!\n\n123."));
        assert_eq!(state.clip.get_text(), "lo ");
        // Past the line's end, what there is; on an empty line, nothing.
        state.cursor = Index2::new(0, 7);
        RemoveChar(5).execute(&mut state);
        assert_eq!(state.lines, Lines::from("HelWorl\n\n123."));
        assert_eq!((state.clip.get_text(), state.cursor.col), ("d!".into(), 6));
        state.cursor = Index2::new(1, 0);
        let undo = state.undo.len();
        RemoveChar(1).execute(&mut state);
        assert_eq!(
            (state.clip.get_text(), state.undo.len()),
            ("d!".into(), undo)
        );
    }

    #[test]
    fn test_remove_char_before_yanks_and_stops_at_the_line_start() {
        use crate::clipboard::InternalClipboard;
        let mut state = test_state();
        state.set_clipboard(InternalClipboard::default());
        state.cursor = Index2::new(0, 4);
        RemoveCharBefore(2).execute(&mut state);
        assert_eq!(state.lines, Lines::from("Heo World!\n\n123."));
        assert_eq!((state.clip.get_text(), state.cursor.col), ("ll".into(), 2));
        RemoveCharBefore(5).execute(&mut state);
        assert_eq!(state.lines, Lines::from("o World!\n\n123."));
        assert_eq!((state.clip.get_text(), state.cursor.col), ("He".into(), 0));
        let undo = state.undo.len();
        RemoveCharBefore(1).execute(&mut state);
        assert_eq!(state.lines, Lines::from("o World!\n\n123."));
        assert_eq!(
            (state.clip.get_text(), state.undo.len()),
            ("He".into(), undo)
        );
    }

    #[test]
    fn test_remove_char() {
        let mut state = test_state();

        state.cursor = Index2::new(0, 4);
        RemoveChar(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));
        assert_eq!(state.lines, Lines::from("Hell World!\n\n123."));

        state.cursor = Index2::new(0, 10);
        RemoveChar(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 9));
        assert_eq!(state.lines, Lines::from("Hell World\n\n123."));
    }

    #[test]
    fn test_replace_char() {
        let mut state = test_state();

        state.cursor = Index2::new(0, 4);
        ReplaceChar('x').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));
        assert_eq!(state.lines, Lines::from("Hellx World!\n\n123."));

        // do nothing on empty line
        state.cursor = Index2::new(1, 0);
        ReplaceChar('x').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));
        assert_eq!(state.lines, Lines::from("Hellx World!\n\n123."));

        // do nothing if out of bounds
        state.cursor = Index2::new(99, 0);
        ReplaceChar('x').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(99, 0));
        assert_eq!(state.lines, Lines::from("Hellx World!\n\n123."));
    }

    #[test]
    fn test_delete_char() {
        let mut state = test_state();

        state.cursor = Index2::new(0, 5);
        DeleteChar(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));
        assert_eq!(state.lines, Lines::from("Hell World!\n\n123."));

        state.cursor = Index2::new(0, 11);
        DeleteChar(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));
        assert_eq!(state.lines, Lines::from("Hell World\n\n123."));
    }

    #[test]
    fn test_delete_char_empty_line() {
        let mut state = test_state();
        state.cursor = Index2::new(1, 99);

        DeleteChar(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 12));
        assert_eq!(state.lines, Lines::from("Hello World!\n123."));

        let mut state = EditorState::new(Lines::from("\nb"));
        state.cursor = Index2::new(0, 1);
        DeleteChar(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        assert_eq!(state.lines, Lines::from("\nb"));
    }

    #[test]
    fn test_delete_line() {
        let mut state = test_state();
        state.cursor = Index2::new(2, 3);

        DeleteLine(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));
        assert_eq!(state.lines, Lines::from("Hello World!\n"));

        DeleteLine(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from("Hello World!"));

        DeleteLine(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from(""));
    }

    #[test]
    fn test_delete_to_first_char_of_line() {
        let mut state = EditorState::new(Lines::from("  Hello World!"));
        state.cursor = Index2::new(0, 4);

        DeleteToFirstCharOfLine.execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
        assert_eq!(state.lines, Lines::from("  llo World!"));

        state.cursor = Index2::new(0, 2);
        DeleteToFirstCharOfLine.execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from("llo World!"));
    }

    #[test]
    fn test_delete_to_end_of_line() {
        let mut state = test_state();
        state.cursor = Index2::new(0, 3);

        DeleteToEndOfLine.execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 2));
        assert_eq!(state.lines, Lines::from("Hel\n\n123."));
    }

    #[test]
    fn test_delete_selection() {
        let mut state = test_state();
        let st = Index2::new(0, 1);
        let en = Index2::new(2, 0);
        state.selection = Some(Selection::new(st, en));

        DeleteSelection.execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        assert_eq!(state.lines, Lines::from("H23."));
    }

    #[test]
    fn test_delete_selection_leaves_the_line_break_after_it() {
        use crate::clipboard::{ClipboardTrait, InternalClipboard};
        // A whole line's characters: the line break stays.
        let mut state = EditorState::new(Lines::from("foo\nbar"));
        state.set_clipboard(InternalClipboard::default());
        state.selection = Some(Selection::new(Index2::new(0, 0), Index2::new(0, 2)));
        DeleteSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("\nbar"));
        assert_eq!(state.clip.get_text(), "foo");
        // Past the line's end, as `v$` goes: the line break too.
        let mut state = EditorState::new(Lines::from("foo\nbar"));
        state.selection = Some(Selection::new(Index2::new(0, 1), Index2::new(0, 3)));
        DeleteSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("fbar"));
        // The last line: the cursor on the one left; every line: one left.
        let mut state = EditorState::new(Lines::from("a\nb"));
        state.cursor = Index2::new(1, 0);
        let line = |row| Some(Selection::new(Index2::new(row, 0), Index2::new(row, 0)).line_mode());
        state.selection = line(1);
        DeleteSelection.execute(&mut state);
        assert_eq!(state.lines, Lines::from("a"));
        assert_eq!(state.cursor, Index2::new(0, 0));
        state.selection = line(0);
        DeleteSelection.execute(&mut state);
        assert_eq!(
            (state.lines.len(), state.lines.to_string()),
            (1, String::new())
        );
    }

    #[test]
    fn test_delete_selection_out_of_bounds() {
        let mut state = EditorState::new(Lines::from("123.\nHello World!\n456."));
        let st = Index2::new(0, 5);
        let en = Index2::new(2, 10);
        state.selection = Some(Selection::new(st, en));

        DeleteSelection.execute(&mut state);
        // assert_eq!(state.cursor, Index2::new(0, 1));
        assert_eq!(state.lines, Lines::from("123."));
    }

    #[test]
    fn test_delete_char_forward() {
        let mut state = EditorState::new(Lines::from("Hello World!\nNext line"));
        state.mode = EditorMode::Insert;

        // Delete character 'H'
        state.cursor = Index2::new(0, 0);
        DeleteCharForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from("ello World!\nNext line"));

        // Delete character 'e'
        state.cursor = Index2::new(0, 0);
        DeleteCharForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from("llo World!\nNext line"));

        // Delete character at end of line (newline)
        state.cursor = Index2::new(0, 10);
        DeleteCharForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));
        assert_eq!(state.lines, Lines::from("llo World!Next line"));
    }

    #[test]
    fn test_delete_char_forward_at_end() {
        let mut state = EditorState::new(Lines::from("Hello\nWorld"));
        state.mode = EditorMode::Insert;

        // Cursor at end of first line should delete newline
        state.cursor = Index2::new(0, 5);
        DeleteCharForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 5));
        assert_eq!(state.lines, Lines::from("HelloWorld"));

        // Cursor at end of last line should do nothing
        state.cursor = Index2::new(0, 10);
        DeleteCharForward(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 10));
        assert_eq!(state.lines, Lines::from("HelloWorld"));
    }

    #[test]
    fn test_delete_word_forward() {
        let mut state = EditorState::new(Lines::from("Hello World Test"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 0);

        DeleteWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "World Test");
        assert_eq!(state.cursor, Index2::new(0, 0));

        DeleteWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "Test");
    }

    #[test]
    fn test_delete_word_forward_mid_word() {
        let mut state = EditorState::new(Lines::from("Hello World"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 2);

        DeleteWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "HeWorld");
    }

    #[test]
    fn test_delete_word_forward_last_character() {
        let mut state = EditorState::new(Lines::from("Hello World"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 10);

        DeleteWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "Hello Worl");
        assert_eq!(state.cursor, Index2::new(0, 9));

        state.cursor = Index2::new(0, 10);
        DeleteWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "Hello Wor");
        assert_eq!(state.cursor, Index2::new(0, 8));
    }

    #[test]
    fn test_delete_word_forward_does_not_cross_line() {
        // Single char on the first line whose class matches the next line's
        // first char must still be deleted (must not bleed into line below).
        let mut state = EditorState::new(Lines::from("a\nHello"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 0);

        DeleteWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "\nHello");

        // Same for a whitespace-delimited WORD delete.
        let mut state = EditorState::new(Lines::from("a\nHello"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 0);

        DeleteBigWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "\nHello");
    }

    #[test]
    fn test_delete_big_word_forward() {
        let mut state = EditorState::new(Lines::from("foo.bar baz"));
        state.cursor = Index2::new(0, 0);

        DeleteBigWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "baz");
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_delete_big_word_forward_mid_word() {
        let mut state = EditorState::new(Lines::from("foo.bar baz"));
        state.cursor = Index2::new(0, 3);

        DeleteBigWordForward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "foobaz");
    }

    #[test]
    fn test_delete_word_end_keeps_trailing_whitespace() {
        let mut state = EditorState::new(Lines::from("Hello World"));
        state.cursor = Index2::new(0, 0);

        DeleteWordEnd(1).execute(&mut state);
        // Unlike `dw`, the trailing whitespace is preserved (Vim `cw` == `ce`).
        assert_eq!(state.lines.to_string(), " World");
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_delete_big_word_vs_small_word() {
        let mut small = EditorState::new(Lines::from("foo.bar baz"));
        small.cursor = Index2::new(0, 0);
        DeleteWordForward(1).execute(&mut small);
        assert_eq!(small.lines.to_string(), ".bar baz");

        let mut big = EditorState::new(Lines::from("foo.bar baz"));
        big.cursor = Index2::new(0, 0);
        DeleteBigWordForward(1).execute(&mut big);
        assert_eq!(big.lines.to_string(), "baz");
    }

    #[test]
    fn test_delete_word_backward() {
        let mut state = EditorState::new(Lines::from("Hello World Test"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 12);

        DeleteWordBackward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "Hello Test");
        assert_eq!(state.cursor, Index2::new(0, 6));
    }

    #[test]
    fn test_delete_word_backward_mid_word() {
        // On "o" of World, should only delete "W"
        let mut state = EditorState::new(Lines::from("Hello World"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 7);

        DeleteWordBackward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "Hello orld");
        assert_eq!(state.cursor, Index2::new(0, 6));
    }

    #[test]
    fn test_delete_word_backward_at_word_start() {
        // On "W" of World, should delete "Hello " (previous word + whitespace)
        let mut state = EditorState::new(Lines::from("Hello World"));
        state.mode = EditorMode::Insert;
        state.cursor = Index2::new(0, 6);

        DeleteWordBackward(1).execute(&mut state);
        assert_eq!(state.lines.to_string(), "World");
        assert_eq!(state.cursor, Index2::new(0, 0));
    }
}
