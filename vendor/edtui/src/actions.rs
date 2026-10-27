//! Editor actions such as move, insert, delete
pub mod change;
pub mod cpaste;
pub mod delete;
pub mod insert;
pub mod motion;
pub mod object;
pub mod operator;
pub mod search;
pub mod select;
#[cfg(feature = "system-editor")]
pub mod system_editor;
mod word;
use crate::state::selection::Selection;
use crate::{EditorMode, EditorState, Index2};
use cpaste::{PasteOverSelection, ReplaceSelection};
use delete::DeleteToEndOfLine;
use enum_dispatch::enum_dispatch;
use insert::type_recorded;
use jagged::index::RowIndex;
use motion::{MoveToFirstRow, MoveToLastRow};
#[cfg(feature = "system-editor")]
pub use system_editor::OpenSystemEditor;

pub use self::change::{
    ChangeBigWord, ChangeFindForward, ChangeInnerBetween, ChangeInnerBigWord, ChangeInnerWord,
    ChangeSelection, ChangeTillForward, ChangeWord,
};
pub use self::cpaste::{CopyLine, CopySelection, Paste, PasteBefore, Put};
pub use self::delete::{
    DeleteBigWordEnd, DeleteBigWordForward, DeleteChar, DeleteCharForward, DeleteFindForward,
    DeleteLine, DeleteLineBefore, DeleteSelection, DeleteTillForward, DeleteToFirstCharOfLine,
    DeleteWordBackward, DeleteWordBefore, DeleteWordEnd, DeleteWordForward, JoinLineWithLineBelow,
    JoinLines, RemoveChar, RemoveCharBefore, Replace, ReplaceChar,
};
pub use self::insert::{
    AppendNewline, EnterInsert, InsertAt, InsertChar, InsertNewline, LineBreak,
};
pub use self::motion::{
    FindBackward, FindForward, MoveBackward, MoveBigWordBackward, MoveBigWordForward,
    MoveBigWordForwardToEndOfWord, MoveDown, MoveDownToFirst, MoveForward, MoveHalfPageDown,
    MoveHalfPageUp, MovePageDown, MovePageUp, MoveParagraphBackward, MoveParagraphForward,
    MoveToEndOfLine, MoveToFirst, MoveToMatchinBracket, MoveToStartOfLine, MoveUp,
    MoveWordBackward, MoveWordForward, MoveWordForwardToEndOfWord, TillBackward, TillForward,
};
pub use self::object::{OnObject, Operator, TextObject};
pub use self::operator::{MotionKind, Operate};
use self::search::StartSearch;
pub use self::search::{
    AppendCharToSearch, FindFirst, FindNext, FindPrevious, RemoveCharFromSearch,
    SelectCurrentSearch, StopSearch,
};
pub use self::select::{
    DeleteInnerBetween, DeleteInnerBigWord, DeleteInnerWord, SelectInnerBetween,
    SelectInnerBigWord, SelectInnerWord, SelectLine,
};

#[enum_dispatch(Execute)]
#[derive(Clone, Debug)]
pub enum Action {
    SwitchMode(SwitchMode),
    MoveForward(MoveForward),
    MoveBackward(MoveBackward),
    MoveUp(MoveUp),
    MoveDown(MoveDown),
    MoveWordForward(MoveWordForward),
    MoveWordForwardToEndOfWord(MoveWordForwardToEndOfWord),
    MoveWordBackward(MoveWordBackward),
    MoveBigWordForward(MoveBigWordForward),
    MoveBigWordForwardToEndOfWord(MoveBigWordForwardToEndOfWord),
    MoveBigWordBackward(MoveBigWordBackward),
    MoveToStartOfLine(MoveToStartOfLine),
    MoveToFirst(MoveToFirst),
    MoveDownToFirst(MoveDownToFirst),
    MoveToEndOfLine(MoveToEndOfLine),
    MoveToFirstRow(MoveToFirstRow),
    MoveToLastRow(MoveToLastRow),
    MoveToMatchingBracket(MoveToMatchinBracket),
    MoveHalfPageDown(MoveHalfPageDown),
    MoveHalfPageUp(MoveHalfPageUp),
    MovePageDown(MovePageDown),
    MovePageUp(MovePageUp),
    MoveParagraphForward(MoveParagraphForward),
    MoveParagraphBackward(MoveParagraphBackward),
    FindForward(FindForward),
    TillForward(TillForward),
    FindBackward(FindBackward),
    TillBackward(TillBackward),
    InsertChar(InsertChar),
    LineBreak(LineBreak),
    AppendNewline(AppendNewline),
    InsertNewline(InsertNewline),
    EnterInsert(EnterInsert),
    ReplaceChar(ReplaceChar),
    Replace(Replace),
    RemoveChar(RemoveChar),
    RemoveCharBefore(RemoveCharBefore),
    OnObject(OnObject),
    Operate(Operate),
    DeleteChar(DeleteChar),
    DeleteCharForward(DeleteCharForward),
    DeleteLine(DeleteLine),
    DeleteToFirstCharOfLine(DeleteToFirstCharOfLine),
    DeleteToEndOfLine(DeleteToEndOfLine),
    DeleteWordForward(DeleteWordForward),
    DeleteBigWordForward(DeleteBigWordForward),
    DeleteFindForward(DeleteFindForward),
    DeleteTillForward(DeleteTillForward),
    DeleteWordEnd(DeleteWordEnd),
    DeleteBigWordEnd(DeleteBigWordEnd),
    ChangeWord(ChangeWord),
    ChangeBigWord(ChangeBigWord),
    ChangeFindForward(ChangeFindForward),
    ChangeTillForward(ChangeTillForward),
    DeleteWordBackward(DeleteWordBackward),
    DeleteWordBefore(DeleteWordBefore),
    DeleteLineBefore(DeleteLineBefore),
    DeleteSelection(DeleteSelection),
    JoinLineWithLineBelow(JoinLineWithLineBelow),
    JoinLines(JoinLines),
    SelectInnerBetween(SelectInnerBetween),
    SelectInnerWord(SelectInnerWord),
    ChangeInnerBetween(ChangeInnerBetween),
    DeleteInnerBetween(DeleteInnerBetween),
    ChangeInnerWord(ChangeInnerWord),
    DeleteInnerWord(DeleteInnerWord),
    SelectInnerBigWord(SelectInnerBigWord),
    ChangeInnerBigWord(ChangeInnerBigWord),
    DeleteInnerBigWord(DeleteInnerBigWord),
    ChangeSelection(ChangeSelection),
    SelectLine(SelectLine),
    Undo(Undo),
    Redo(Redo),
    RepeatLastChange(RepeatLastChange),
    Paste(Paste),
    PasteBefore(PasteBefore),
    Put(Put),
    PasteOverSelection(PasteOverSelection),
    ReplaceSelection(ReplaceSelection),
    CopySelection(CopySelection),
    CopyLine(CopyLine),
    Composed(Composed),
    Repeat(Repeat),
    StartSearch(StartSearch),
    StopSearch(StopSearch),
    FindFirst(FindFirst),
    FindNext(FindNext),
    FindPrevious(FindPrevious),
    SelectCurrentSearch(SelectCurrentSearch),
    AppendCharToSearch(AppendCharToSearch),
    RemoveCharFromSearch(RemoveCharFromSearch),
    #[cfg(feature = "system-editor")]
    OpenSystemEditor(OpenSystemEditor),
}

#[enum_dispatch]
pub trait Execute {
    fn execute(&mut self, state: &mut EditorState);

    /// Whether this action can be replayed by the dot-repeat command.
    fn is_repeatable(&self) -> bool {
        false
    }

    /// Returns a handle to this action's character argument if it takes one
    /// (like `f`/`t`). While the inner value is `None`, the action is still
    /// waiting for the key handler to supply the next keystroke through it.
    fn char_arg(&mut self) -> Option<&mut Option<char>> {
        None
    }

    /// Takes a count typed before it (`3x`, `5j`, `2dd`) as it means for it:
    /// whether it does. One that doesn't is done that many times
    /// (`Repeat`). Added by roster.
    fn count(&mut self, _n: usize) -> bool {
        false
    }

    /// As the motion after an operator (`dw`, `y$`, `cj`): how the operator
    /// takes what it moves over, if it's one. Added by roster.
    fn motion(&self) -> Option<MotionKind> {
        None
    }

    /// Moves the cursor as this motion does after an operator, where that
    /// isn't as `execute` does (`w` stopping at a line's end, `l` going past
    /// it): whether it could (not `j` on the last line, nor `f` with no such
    /// character). Added by roster.
    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        self.execute(state);
        true
    }
}

/// An action done `count` times, as a count typed before it asks (`3p`), in
/// one undo step, and once only if it enters insert mode (a count isn't
/// for those). Added by roster.
#[derive(Clone, Debug)]
pub struct Repeat {
    pub count: usize,
    pub action: Box<Action>,
}

impl Execute for Repeat {
    fn execute(&mut self, state: &mut EditorState) {
        let undo = state.undo.len();
        for _ in 0..self.count {
            self.action.execute(state);
            if state.mode == EditorMode::Insert {
                break;
            }
        }
        state.undo.squash_since(undo);
    }

    fn is_repeatable(&self) -> bool {
        self.action.is_repeatable()
    }

    fn count(&mut self, n: usize) -> bool {
        self.count = n;
        true
    }

    fn motion(&self) -> Option<MotionKind> {
        self.action.motion()
    }

    fn move_after_operator(&mut self, state: &mut EditorState) -> bool {
        (0..self.count).all(|_| self.action.move_after_operator(state))
    }
}

/// `action` with the count typed before it, if one was.
#[must_use]
pub fn counted(mut action: Action, count: Option<usize>) -> Action {
    let Some(n) = count else {
        return action;
    };
    if action.count(n) || n == 1 {
        return action;
    }
    Repeat {
        count: n,
        action: Box::new(action),
    }
    .into()
}

/// `action` with the count `n` in place of any it had (`3.`).
#[must_use]
pub fn recounted(action: Action, n: usize) -> Action {
    let action = match action {
        Action::Repeat(repeat) => *repeat.action,
        action => action,
    };
    counted(action, Some(n))
}

pub trait Chainable {
    fn chain<A: Into<Action>>(self, action: A) -> Composed;
}

impl<T: Into<Action>> Chainable for T {
    fn chain<A: Into<Action>>(self, action: A) -> Composed {
        Composed::new(self.into()).chain(action)
    }
}

#[derive(Clone, Debug)]
pub struct SwitchMode(pub EditorMode);

impl Execute for SwitchMode {
    fn execute(&mut self, state: &mut EditorState) {
        let from_insert = state.mode == EditorMode::Insert;

        state.clamp_column();
        match self.0 {
            EditorMode::Normal => {
                state.selection = None;
                if from_insert {
                    type_again(state);
                    state.insert_start = None;
                }
            }
            EditorMode::Visual => {
                state.selection = Some(Selection::new(state.cursor, state.cursor));
            }
            EditorMode::Insert => {
                if ![EditorMode::Insert, EditorMode::Search].contains(&state.mode) {
                    state.capture();
                }
            }
            EditorMode::Search => {}
        }
        state.mode = self.0;

        if self.0 == EditorMode::Normal {
            // When leaving insert mode, move the cursor one column left so it
            // rests on the last typed character rather than the empty slot
            // after it.
            if from_insert && state.cursor.col > 0 {
                state.cursor.col -= 1;
            }

            // Re-clamp so the cursor never lingers past the end of the line.
            state.clamp_column();
        }
    }

    fn is_repeatable(&self) -> bool {
        // Entering insert mode begins a repeatable insert session (`.`).
        self.0 == EditorMode::Insert
    }
}

/// What was typed in insert mode typed again as many more times as it was
/// entered with a count for (`3ifoo`), on a new line each after `o`.
fn type_again(state: &mut EditorState) {
    let Some((times, lines)) = state.insert_count.take() else {
        return;
    };
    // Not recorded again: `.` types it that many times itself.
    let recording = state.insert_recording.take();
    let text = recording.clone().unwrap_or_default();
    for _ in 0..times {
        if lines && !state.view.single_line {
            let row = (state.cursor.row + 1).min(state.lines.len());
            state.lines.insert(RowIndex::new(row), vec![]);
            state.cursor = Index2::new(row, 0);
        }
        // Each an insert of its own, for Ctrl+W and Ctrl+U to stop at.
        state.insert_start = Some(state.cursor);
        for c in text.chars() {
            type_recorded(state, c);
        }
    }
    state.insert_recording = recording;
}

#[derive(Clone, Debug)]
pub struct Undo;

impl Execute for Undo {
    fn execute(&mut self, state: &mut EditorState) {
        state.undo();
    }
}

/// Repeats the last buffer-changing command (dot-repeat).
#[derive(Clone, Debug)]
pub struct RepeatLastChange;

impl Execute for RepeatLastChange {
    fn execute(&mut self, state: &mut EditorState) {
        let Some(mut action) = state.last_change.clone() else {
            return;
        };
        action.execute(state);

        if let Some(text) = state.last_insert.clone() {
            if state.mode == EditorMode::Insert {
                state.insert_start = Some(state.cursor);
                // Recorded as typed, for a count to type it again (`3.`
                // after `ifoo`).
                state.insert_recording = Some(String::new());
                for c in text.chars() {
                    type_recorded(state, c);
                }
                SwitchMode(EditorMode::Normal).execute(state);
                state.insert_recording = None;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Redo;

impl Execute for Redo {
    fn execute(&mut self, state: &mut EditorState) {
        state.redo();
    }
}

/// Executes multiple actions one after the other.
#[derive(Clone, Debug)]
pub struct Composed(pub Vec<Action>);

impl Composed {
    #[must_use]
    pub fn new<A: Into<Action>>(action: A) -> Self {
        Self(vec![action.into()])
    }

    #[must_use]
    pub fn chain<A: Into<Action>>(mut self, action: A) -> Self {
        self.0.push(action.into());
        self
    }
}

impl Execute for Composed {
    fn execute(&mut self, state: &mut EditorState) {
        for action in &mut self.0 {
            action.execute(state);
        }
    }

    fn is_repeatable(&self) -> bool {
        self.0.iter().any(Execute::is_repeatable)
    }
}

#[cfg(test)]
mod tests {
    use crate::clipboard::InternalClipboard;
    use crate::Lines;

    use super::*;
    fn test_state() -> EditorState {
        let mut state = EditorState::new(Lines::from("Hello World!\n\n123."));
        state.set_clipboard(InternalClipboard::default());
        state
    }

    #[test]
    fn test_switch_mode() {
        let mut state = test_state();
        assert_eq!(state.mode, EditorMode::Normal);

        SwitchMode(EditorMode::Insert).execute(&mut state);
        assert_eq!(state.mode, EditorMode::Insert);

        SwitchMode(EditorMode::Visual).execute(&mut state);
        assert_eq!(state.mode, EditorMode::Visual);
    }

    #[test]
    fn test_chainable_actions() {
        let mut state = test_state();
        assert_eq!(state.mode, EditorMode::Normal);

        // Test the new chainable syntax: SwitchMode().chain().chain()
        let mut action = SwitchMode(EditorMode::Insert)
            .chain(MoveToEndOfLine(1))
            .chain(SwitchMode(EditorMode::Visual));

        action.execute(&mut state);

        // Verify the final state after chaining
        assert_eq!(state.mode, EditorMode::Visual);
        assert!(state.selection.is_some());
    }
}
