use jagged::index::RowIndex;

use super::delete::{DeleteChar, DeleteCharForward, DeleteLineBefore, DeleteWordBefore};
use super::motion::{MoveForward, MoveToEndOfLine, MoveToFirst};
use super::{Execute, SwitchMode};
use crate::{
    helper::{insert_char, line_break},
    EditorMode, EditorState,
};

/// Inserts a single character at the current cursor position.
///
/// In single-line mode, newline characters (`\n`, `\r`) are ignored.
#[derive(Clone, Debug, Copy)]
pub struct InsertChar(pub char);

impl Execute for InsertChar {
    fn execute(&mut self, state: &mut EditorState) {
        // Block newline characters in single-line mode
        if state.view.single_line && matches!(self.0, '\n' | '\r') {
            return;
        }
        insert_char(&mut state.lines, &mut state.cursor, self.0, false);

        // Capture insert session for the dot-repeat command
        if let Some(buffer) = &mut state.insert_recording {
            buffer.push(self.0);
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Inserts a newline at the current cursor position.
///
/// In single-line mode, this action is ignored.
#[derive(Clone, Debug, Copy)]
pub struct LineBreak(pub usize);

impl Execute for LineBreak {
    fn execute(&mut self, state: &mut EditorState) {
        if state.view.single_line {
            return;
        }
        if state.lines.is_empty() {
            state.lines.push(Vec::new());
        }
        for _ in 0..self.0 {
            line_break(&mut state.lines, &mut state.cursor);
        }

        // Typed in an insert session, for the dot-repeat command too.
        if let Some(buffer) = &mut state.insert_recording {
            buffer.extend(std::iter::repeat_n('\n', self.0));
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Appends a newline below the current cursor position.
///
/// In single-line mode, this action is ignored.
#[derive(Clone, Debug, Copy)]
pub struct AppendNewline(pub usize);

impl Execute for AppendNewline {
    fn execute(&mut self, state: &mut EditorState) {
        if state.view.single_line {
            return;
        }
        state.capture();
        state.cursor.col = 0;
        for _ in 0..self.0 {
            if !state.lines.is_empty() {
                state.cursor.row += 1;
            }
            if state.cursor.row < state.lines.len() {
                state.lines.insert(RowIndex::new(state.cursor.row), vec![]);
            } else {
                state.lines.push(vec![]);
            }
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// Appends a newline at the current cursor position.
///
/// In single-line mode, this action is ignored.
#[derive(Clone, Debug, Copy)]
pub struct InsertNewline(pub usize);

impl Execute for InsertNewline {
    fn execute(&mut self, state: &mut EditorState) {
        if state.view.single_line {
            return;
        }
        state.cursor.col = 0;
        for _ in 0..self.0 {
            state.lines.insert(RowIndex::new(state.cursor.row), vec![]);
        }
    }

    fn is_repeatable(&self) -> bool {
        true
    }
}

/// What an insert session records for the keys that delete, typed again as
/// they were (`.`, a count): Backspace, Delete, Ctrl+W and Ctrl+U. Added by
/// roster.
pub(crate) const BACKSPACE: char = '\u{8}';
pub(crate) const DELETE: char = '\u{7f}';
pub(crate) const DELETE_WORD: char = '\u{17}';
pub(crate) const DELETE_LINE: char = '\u{15}';

/// Records `key`, `n` times, in the insert session being recorded, if one is.
pub(crate) fn record(state: &mut EditorState, key: char, n: usize) {
    if let Some(buffer) = &mut state.insert_recording {
        buffer.extend(std::iter::repeat_n(key, n));
    }
}

/// Types one key an insert session recorded, as it was typed.
pub(crate) fn type_recorded(state: &mut EditorState, key: char) {
    match key {
        BACKSPACE => DeleteChar(1).execute(state),
        DELETE => DeleteCharForward(1).execute(state),
        DELETE_WORD => DeleteWordBefore.execute(state),
        DELETE_LINE => DeleteLineBefore.execute(state),
        c => InsertChar(c).execute(state),
    }
}

/// Where insert mode is entered: vim's `i`, `a`, `I`, `A`, `o` and `O`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertAt {
    Cursor,
    After,
    LineStart,
    LineEnd,
    Below,
    Above,
}

/// Enters insert mode `at` the cursor or about it, and, with a count, what's
/// typed there is typed that many times in all on leaving it, as vim's
/// `3ifoo` and `2o`: on lines of their own below after `o` and `O`. Added by
/// roster.
#[derive(Clone, Copy, Debug)]
pub struct EnterInsert {
    pub at: InsertAt,
    pub count: usize,
}

impl EnterInsert {
    #[must_use]
    pub fn at(at: InsertAt) -> Self {
        Self { at, count: 1 }
    }
}

impl Execute for EnterInsert {
    fn execute(&mut self, state: &mut EditorState) {
        SwitchMode(EditorMode::Insert).execute(state);
        match self.at {
            InsertAt::Cursor => {}
            InsertAt::After => MoveForward(1).execute(state),
            InsertAt::LineStart => MoveToFirst().execute(state),
            InsertAt::LineEnd => MoveToEndOfLine(1).execute(state),
            InsertAt::Below => AppendNewline(1).execute(state),
            InsertAt::Above => InsertNewline(1).execute(state),
        }
        let lines = matches!(self.at, InsertAt::Below | InsertAt::Above);
        state.insert_count = (self.count > 1).then_some((self.count - 1, lines));
    }

    fn is_repeatable(&self) -> bool {
        true
    }

    fn count(&mut self, n: usize) -> bool {
        self.count = n;
        true
    }
}

/// Pushes a line to the back of the buffer.
/// Does not affect the cursor position.
#[derive(Clone, Debug, Copy)]
pub struct PushLine<'a>(pub &'a str);

impl Execute for PushLine<'_> {
    fn execute(&mut self, state: &mut EditorState) {
        let chars: Vec<char> = self.0.chars().collect();
        state.lines.push(chars);
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
    fn test_insert_char() {
        let mut state = test_state();

        InsertChar('!').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        assert_eq!(state.lines, Lines::from("!Hello World!\n\n123."));

        state.cursor = Index2::new(0, 13);
        InsertChar('!').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 14));
        assert_eq!(state.lines, Lines::from("!Hello World!!\n\n123."));
    }

    #[test]
    fn test_insert_char_into_empty_buffer() {
        let mut state = EditorState::new(Lines::from("\n"));
        state.cursor.row = 1;

        InsertChar('a').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 1));
        assert_eq!(state.lines, Lines::from("\na"));
    }

    #[test]
    fn test_insert_char_out_of_bounds() {
        let mut state = EditorState::new(Lines::from("\nb"));
        state.cursor = Index2::new(0, 1);

        InsertChar('a').execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 1));
        assert_eq!(state.lines, Lines::from("a\nb"));
    }

    #[test]
    fn test_line_break() {
        let mut state = test_state();

        LineBreak(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));
        assert_eq!(state.lines, Lines::from("\nHello World!\n\n123."));

        state.cursor = Index2::new(1, 5);
        LineBreak(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));
        assert_eq!(state.lines, Lines::from("\nHello\n World!\n\n123."));
    }

    #[test]
    fn test_line_break_col_out_of_bounds() {
        let mut state = test_state();
        state.cursor.col = 99;

        LineBreak(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));
        assert_eq!(state.lines, Lines::from("Hello World!\n\n\n123."));

        state.cursor.col = 99;
        state.cursor.row = 4;
        LineBreak(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(5, 0));
        assert_eq!(state.lines, Lines::from("Hello World!\n\n\n123.\n"));
    }

    #[test]
    fn test_append_newline() {
        let mut state = test_state();

        AppendNewline(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(1, 0));
        assert_eq!(state.lines, Lines::from("Hello World!\n\n\n123."));

        state.cursor = Index2::new(3, 0);
        AppendNewline(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(4, 0));
        assert_eq!(state.lines, Lines::from("Hello World!\n\n\n123.\n"));
    }

    #[test]
    fn test_insert_newline() {
        let mut state = test_state();

        InsertNewline(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert_eq!(state.lines, Lines::from("\nHello World!\n\n123."));

        state.cursor = Index2::new(2, 1);
        InsertNewline(1).execute(&mut state);
        assert_eq!(state.cursor, Index2::new(2, 0));
        assert_eq!(state.lines, Lines::from("\nHello World!\n\n\n123."));
    }

    #[test]
    fn test_push_line() {
        let mut state = test_state();

        PushLine("456.").execute(&mut state);
        assert_eq!(state.lines, Lines::from("Hello World!\n\n123.\n456."));
    }
}
