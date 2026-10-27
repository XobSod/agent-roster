pub(crate) mod deprecated;
pub(crate) mod input;

use crate::actions::cpaste::{PasteOverSelection, ReplaceSelection};
use crate::actions::delete::{
    DeleteCharForward, DeleteLineBefore, DeleteToEndOfLine, DeleteToFirstCharOfLine,
    DeleteWordBackward, DeleteWordBefore, DeleteWordForward,
};
use crate::actions::motion::{
    FindBackward, MoveBigWordBackward, MoveBigWordForward, MoveBigWordForwardToEndOfWord,
    MoveDownToFirst, MoveHalfPageDown, MovePageDown, MovePageUp, MoveToFirstRow, MoveToLastRow,
    TillBackward,
};
use crate::actions::search::StartSearch;
#[cfg(feature = "system-editor")]
use crate::actions::OpenSystemEditor;
use crate::actions::{counted, recounted};
use crate::actions::{
    Action, AppendCharToSearch, Chainable, ChangeSelection, CopySelection, DeleteChar,
    DeleteSelection, EnterInsert, Execute, FindFirst, FindForward, FindNext, FindPrevious,
    InsertAt, InsertChar, JoinLines, LineBreak, MoveBackward, MoveDown, MoveForward,
    MoveHalfPageUp, MoveParagraphBackward, MoveParagraphForward, MoveToEndOfLine, MoveToFirst,
    MoveToMatchinBracket, MoveToStartOfLine, MoveUp, MoveWordBackward, MoveWordForward,
    MoveWordForwardToEndOfWord, OnObject, Operate, Operator, Paste, Put, Redo, RemoveChar,
    RemoveCharBefore, RemoveCharFromSearch, RepeatLastChange, Replace, SelectCurrentSearch,
    SelectInnerBetween, SelectInnerWord, SelectLine, StopSearch, SwitchMode, TillForward, Undo,
};
use crate::events::KeyInput;
use crate::{EditorMode, EditorState};
use crossterm::event::KeyCode;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct KeyEventHandler {
    lookup: Vec<KeyInput>,
    register: HashMap<KeyEventRegister, Action>,
    capture_on_insert: bool,
    /// An action awaiting a character argument (e.g. `f`/`t`/`df`/`dt`). The
    /// next keystroke is fed to it via [`Execute::char_arg`].
    pending_char: Option<Action>,
    /// A count typed before a command (`3` of `3dd`), and within one (`2` of
    /// `d2w`), which multiply; and the count of an action waiting for its
    /// character. Added by roster.
    count: Option<usize>,
    inner_count: Option<usize>,
    pending_count: Option<usize>,
    /// An operator waiting for its motion or text object (`d` of `dw`, `c`
    /// of `ci(`), and its key, which again is its line (`dd`); then whether
    /// a text object's `i` or `a` came (`a`). Added by roster.
    operator: Option<(Operate, KeyInput)>,
    object: Option<bool>,
}

impl Default for KeyEventHandler {
    fn default() -> Self {
        Self::vim_mode()
    }
}

impl KeyEventHandler {
    /// Creates a new `KeyEventHandler`.
    #[must_use]
    pub fn new(register: HashMap<KeyEventRegister, Action>, capture_on_insert: bool) -> Self {
        Self {
            lookup: Vec::new(),
            register,
            capture_on_insert,
            pending_char: None,
            count: None,
            inner_count: None,
            pending_count: None,
            operator: None,
            object: None,
        }
    }

    /// Creates a new `KeyEventHandler` with vim keybindings.
    #[must_use]
    pub fn vim_mode() -> Self {
        let register: HashMap<KeyEventRegister, Action> = vim_keybindings();
        Self {
            lookup: Vec::new(),
            register,
            capture_on_insert: false,
            pending_char: None,
            count: None,
            inner_count: None,
            pending_count: None,
            operator: None,
            object: None,
        }
    }

    // Creates a new `KeyEventHandler` with emacs keybindings.
    #[must_use]
    pub fn emacs_mode() -> Self {
        let register: HashMap<KeyEventRegister, Action> = emacs_keybindings();
        Self {
            lookup: Vec::new(),
            register,
            capture_on_insert: true,
            pending_char: None,
            count: None,
            inner_count: None,
            pending_count: None,
            operator: None,
            object: None,
        }
    }

    /// Insert a new callback to the registry
    pub fn insert<T>(&mut self, key: KeyEventRegister, action: T)
    where
        T: Into<Action>,
    {
        self.register.insert(key, action.into());
    }

    /// Extents the register with the contents of an iterator
    pub fn extend<T, U>(&mut self, iter: T)
    where
        U: Into<Action>,
        T: IntoIterator<Item = (KeyEventRegister, U)>,
    {
        self.register
            .extend(iter.into_iter().map(|(k, v)| (k, v.into())));
    }

    /// Remove a callback from the registry
    pub fn remove(&mut self, key: &KeyEventRegister) {
        self.register.remove(key);
    }

    /// Whether the next key goes on what was typed before it: a sequence
    /// begun (`g` of `gg`), an operator waiting for its motion (`d` of
    /// `dw`), or an action waiting for its character (`f`, `r`). Added by
    /// roster, for keys of its own to know when they're edtui's.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.lookup.is_empty()
            || self.pending_char.is_some()
            || self.count.is_some()
            || self.inner_count.is_some()
            || self.operator.is_some()
            || self.object.is_some()
    }

    /// The count typed, if any, given up.
    fn take_count(&mut self) -> Option<usize> {
        match (self.count.take(), self.inner_count.take()) {
            (None, None) => None,
            (outer, inner) => Some(outer.unwrap_or(1).saturating_mul(inner.unwrap_or(1))),
        }
    }

    /// Returns an action for a specific register key, if present.
    /// Returns an action only if there is an exact match. If there
    /// are multiple matches or an inexact match, the specified key
    /// is appended to the lookup vector.
    /// If there is an exact match or if none of the keys in the registry
    /// starts with the current sequence, the lookup sequence is reset.
    #[must_use]
    fn get(&mut self, c: KeyInput, mode: EditorMode) -> Option<Action> {
        self.lookup.push(c);
        let key = KeyEventRegister::new(self.lookup.clone(), mode);

        match self
            .register
            .keys()
            .filter(|k| k.mode == key.mode && k.keys.starts_with(&key.keys))
            .count()
        {
            0 => {
                self.lookup.clear();
                None
            }
            1 => self.register.get(&key).map(|action| {
                self.lookup.clear();
                action.clone()
            }),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_lines)]
fn vim_keybindings() -> HashMap<KeyEventRegister, Action> {
    #[allow(unused_mut)]
    let mut map = HashMap::from([
        // Go into normal mode
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Esc)]),
            SwitchMode(EditorMode::Normal).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::Esc)]),
            SwitchMode(EditorMode::Normal).into(),
        ),
        // Go into insert mode
        (
            KeyEventRegister::n(vec![KeyInput::new('i')]),
            EnterInsert::at(InsertAt::Cursor).into(),
        ),
        // Go into visual mode
        (
            KeyEventRegister::n(vec![KeyInput::new('v')]),
            SwitchMode(EditorMode::Visual).into(),
        ),
        // Goes into search mode and starts of a new search.
        (
            KeyEventRegister::n(vec![KeyInput::new('/')]),
            StartSearch.chain(SwitchMode(EditorMode::Search)).into(),
        ),
        // Trigger initial search
        (
            KeyEventRegister::s(vec![KeyInput::new(KeyCode::Enter)]),
            FindFirst.chain(SwitchMode(EditorMode::Normal)).into(),
        ),
        // Find next
        (
            KeyEventRegister::n(vec![KeyInput::new('n')]),
            FindNext.into(),
        ),
        // Find previous
        (
            KeyEventRegister::n(vec![KeyInput::shift('N')]),
            FindPrevious.into(),
        ),
        // Clear search
        (
            KeyEventRegister::s(vec![KeyInput::new(KeyCode::Esc)]),
            StopSearch.chain(SwitchMode(EditorMode::Normal)).into(),
        ),
        // Delete last character from search
        (
            KeyEventRegister::s(vec![KeyInput::new(KeyCode::Backspace)]),
            RemoveCharFromSearch.into(),
        ),
        // Go into insert mode and move one char forward
        (
            KeyEventRegister::n(vec![KeyInput::new('a')]),
            EnterInsert::at(InsertAt::After).into(),
        ),
        // Move cursor forward
        (
            KeyEventRegister::n(vec![KeyInput::new('l')]),
            MoveForward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('l')]),
            MoveForward(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::Right)]),
            MoveForward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::Right)]),
            MoveForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Right)]),
            MoveForward(1).into(),
        ),
        // Move cursor backward
        (
            KeyEventRegister::n(vec![KeyInput::new('h')]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('h')]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::Left)]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::Left)]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Left)]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl(KeyCode::Right)]),
            MoveWordForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl(KeyCode::Left)]),
            MoveWordBackward(1).into(),
        ),
        // Move cursor up
        (
            KeyEventRegister::n(vec![KeyInput::new('k')]),
            MoveUp(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('k')]),
            MoveUp(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::Up)]),
            MoveUp(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::Up)]),
            MoveUp(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Up)]),
            MoveUp(1).into(),
        ),
        // Move cursor down
        (
            KeyEventRegister::n(vec![KeyInput::new('j')]),
            MoveDown(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('j')]),
            MoveDown(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::Down)]),
            MoveDown(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::Down)]),
            MoveDown(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Down)]),
            MoveDown(1).into(),
        ),
        // Move one word forward/backward
        (
            KeyEventRegister::n(vec![KeyInput::new('w')]),
            MoveWordForward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('w')]),
            MoveWordForward(1).into(),
        ),
        // Find/till the next character on the line (target read from next key)
        (
            KeyEventRegister::n(vec![KeyInput::new('f')]),
            FindForward(None, 1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('f')]),
            FindForward(None, 1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('t')]),
            TillForward(None, 1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('t')]),
            TillForward(None, 1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('e')]),
            MoveWordForwardToEndOfWord(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('e')]),
            MoveWordForwardToEndOfWord(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('b')]),
            MoveWordBackward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('b')]),
            MoveWordBackward(1).into(),
        ),
        // WORDs (anything but whitespace) forward/back, and find/till backward
        (
            KeyEventRegister::n(vec![KeyInput::shift('W')]),
            MoveBigWordForward(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('E')]),
            MoveBigWordForwardToEndOfWord(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('B')]),
            MoveBigWordBackward(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('F')]),
            FindBackward(None, 1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('T')]),
            TillBackward(None, 1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('W')]),
            MoveBigWordForward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('E')]),
            MoveBigWordForwardToEndOfWord(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('B')]),
            MoveBigWordBackward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('F')]),
            FindBackward(None, 1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('T')]),
            TillBackward(None, 1).into(),
        ),
        // Move cursor to start/first/last position
        (
            KeyEventRegister::n(vec![KeyInput::new('0')]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('_')]),
            MoveDownToFirst(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('^')]),
            MoveToFirst().into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('$')]),
            MoveToEndOfLine(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('0')]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('_')]),
            MoveDownToFirst(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('^')]),
            MoveToFirst().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('$')]),
            MoveToEndOfLine(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::ctrl('d')]),
            MoveHalfPageDown().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::ctrl('d')]),
            MoveHalfPageDown().into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::ctrl('u')]),
            MoveHalfPageUp().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::ctrl('u')]),
            MoveHalfPageUp().into(),
        ),
        // Page up/down for full page navigation
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::PageDown)]),
            MovePageDown().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::PageDown)]),
            MovePageDown().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::PageDown)]),
            MovePageDown().into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::PageUp)]),
            MovePageUp().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::PageUp)]),
            MovePageUp().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::PageUp)]),
            MovePageUp().into(),
        ),
        // `Home` and `End` go to first/last position in a line
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Home)]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::Home)]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::Home)]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::End)]),
            MoveToEndOfLine(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::End)]),
            MoveToEndOfLine(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new(KeyCode::End)]),
            MoveToEndOfLine(1).into(),
        ),
        // `Ctrl+u` deletes what was typed before the cursor in insert mode, as
        // vim's
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('u')]),
            DeleteLineBefore.into(),
        ),
        // `Ctrl+w` deletes the word before the cursor in insert mode, as vim's
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('w')]),
            DeleteWordBefore.into(),
        ),
        // Move cursor to start/first/last position and enter insert mode
        (
            KeyEventRegister::n(vec![KeyInput::shift('I')]),
            EnterInsert::at(InsertAt::LineStart).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('A')]),
            EnterInsert::at(InsertAt::LineEnd).into(),
        ),
        // Move cursor to start/last row in the buffer
        (
            KeyEventRegister::n(vec![KeyInput::new('g'), KeyInput::new('g')]),
            MoveToFirstRow(None).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('g'), KeyInput::new('g')]),
            MoveToFirstRow(None).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('G')]),
            MoveToLastRow(None).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('G')]),
            MoveToLastRow(None).into(),
        ),
        // Move cursor to the next opening/closing bracket.
        (
            KeyEventRegister::n(vec![KeyInput::new('%')]),
            MoveToMatchinBracket().into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('%')]),
            MoveToMatchinBracket().into(),
        ),
        // Move to next/previous paragraph boundary
        (
            KeyEventRegister::n(vec![KeyInput::new('}')]),
            MoveParagraphForward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('}')]),
            MoveParagraphForward(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('{')]),
            MoveParagraphBackward(1).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('{')]),
            MoveParagraphBackward(1).into(),
        ),
        // Append/insert new line and switch into insert mode
        (
            KeyEventRegister::n(vec![KeyInput::new('o')]),
            EnterInsert::at(InsertAt::Below).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('O')]),
            EnterInsert::at(InsertAt::Above).into(),
        ),
        // Insert a line break
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Enter)]),
            LineBreak(1).into(),
        ),
        // Remove the current character
        (
            KeyEventRegister::n(vec![KeyInput::new('x')]),
            RemoveChar(1).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new(KeyCode::Delete)]),
            RemoveChar(1).into(),
        ),
        // Remove the character before the cursor
        (
            KeyEventRegister::n(vec![KeyInput::shift('X')]),
            RemoveCharBefore(1).into(),
        ),
        // Replace the character under the cursor with the next key's
        (
            KeyEventRegister::n(vec![KeyInput::new('r')]),
            Replace(None, 1).into(),
        ),
        // Delete the previous character
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Backspace)]),
            DeleteChar(1).into(),
        ),
        // Delete the previous character
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('h')]),
            DeleteChar(1).into(),
        ),
        // Delete the next character
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Delete)]),
            DeleteCharForward(1).into(),
        ),
        // Delete the current selection
        (
            KeyEventRegister::v(vec![KeyInput::new('d')]),
            DeleteSelection.chain(SwitchMode(EditorMode::Normal)).into(),
        ),
        // Join the current line with the line below, as vim's (`3J` three)
        (
            KeyEventRegister::n(vec![KeyInput::shift('J')]),
            JoinLines(2).into(),
        ),
        // Select inner word between delimiters
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('w')]),
            SelectInnerWord.into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('"')]),
            SelectInnerBetween::new('"', '"').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('\'')]),
            SelectInnerBetween::new('\'', '\'').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('(')]),
            SelectInnerBetween::new('(', ')').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new(')')]),
            SelectInnerBetween::new('(', ')').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('{')]),
            SelectInnerBetween::new('{', '}').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('}')]),
            SelectInnerBetween::new('{', '}').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new('[')]),
            SelectInnerBetween::new('[', ']').into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('i'), KeyInput::new(']')]),
            SelectInnerBetween::new('[', ']').into(),
        ),
        // Change selection
        (
            KeyEventRegister::v(vec![KeyInput::new('c')]),
            SwitchMode(EditorMode::Insert).chain(ChangeSelection).into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('x')]),
            ChangeSelection.chain(SwitchMode(EditorMode::Normal)).into(),
        ),
        // Select  the line
        (
            KeyEventRegister::n(vec![KeyInput::shift('V')]),
            SelectLine.into(),
        ),
        // Operators, the motion or text object after them what they act on
        // (`dw`, `c$`, `yi(`), their key again their line (`dd`)
        (
            KeyEventRegister::n(vec![KeyInput::new('d')]),
            Operate::new(Operator::Delete).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('c')]),
            Operate::new(Operator::Change).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('y')]),
            Operate::new(Operator::Yank).into(),
        ),
        // To the end of the line (`d$`, `c$`), the line (`cc`), a character (`cl`)
        (
            KeyEventRegister::n(vec![KeyInput::shift('D')]),
            Operate::on(Operator::Delete, MoveToEndOfLine(1)).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('C')]),
            Operate::on(Operator::Change, MoveToEndOfLine(1)).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('S')]),
            Operate::on(Operator::Change, MoveDownToFirst(1)).into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::new('s')]),
            Operate::on(Operator::Change, MoveForward(1)).into(),
        ),
        // Undo
        (KeyEventRegister::n(vec![KeyInput::new('u')]), Undo.into()),
        // Redo
        (KeyEventRegister::n(vec![KeyInput::ctrl('r')]), Redo.into()),
        // Repeat the last change
        (
            KeyEventRegister::n(vec![KeyInput::new('.')]),
            RepeatLastChange.into(),
        ),
        // Copy
        (
            KeyEventRegister::v(vec![KeyInput::new('y')]),
            CopySelection.chain(SwitchMode(EditorMode::Normal)).into(),
        ),
        // Paste
        (
            KeyEventRegister::n(vec![KeyInput::new('p')]),
            Put {
                count: 1,
                before: false,
            }
            .into(),
        ),
        (
            KeyEventRegister::n(vec![KeyInput::shift('P')]),
            Put {
                count: 1,
                before: true,
            }
            .into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::new('p')]),
            PasteOverSelection
                .chain(SwitchMode(EditorMode::Normal))
                .into(),
        ),
        (
            KeyEventRegister::v(vec![KeyInput::shift('P')]),
            ReplaceSelection
                .chain(SwitchMode(EditorMode::Normal))
                .into(),
        ),
    ]);

    // Open system editor (Ctrl+e in normal mode)
    #[cfg(feature = "system-editor")]
    map.insert(
        KeyEventRegister::n(vec![KeyInput::ctrl('e')]),
        OpenSystemEditor.into(),
    );

    // Text objects in visual mode (vaw, vi(, …), in place of those above.
    for (keys, mode, action) in crate::actions::object::keys() {
        let keys: Vec<KeyInput> = keys
            .into_iter()
            .map(|c| match c.is_uppercase() {
                true => KeyInput::shift(c),
                false => KeyInput::new(c),
            })
            .collect();
        map.insert(KeyEventRegister::new(keys, mode), action);
    }

    map
}

#[allow(clippy::too_many_lines)]
fn emacs_keybindings() -> HashMap<KeyEventRegister, Action> {
    HashMap::from([
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('s')]),
            StartSearch.chain(SwitchMode(EditorMode::Search)).into(),
        ),
        (
            KeyEventRegister::s(vec![KeyInput::ctrl('s')]),
            FindNext.into(),
        ),
        (
            KeyEventRegister::s(vec![KeyInput::ctrl('r')]),
            FindPrevious.into(),
        ),
        (
            KeyEventRegister::s(vec![KeyInput::new(KeyCode::Enter)]),
            SelectCurrentSearch
                .chain(SwitchMode(EditorMode::Insert))
                .into(),
        ),
        (
            KeyEventRegister::s(vec![KeyInput::ctrl('g')]),
            StopSearch.chain(SwitchMode(EditorMode::Insert)).into(),
        ),
        (
            KeyEventRegister::s(vec![KeyInput::new(KeyCode::Backspace)]),
            RemoveCharFromSearch.into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('f')]),
            MoveForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Right)]),
            MoveForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('b')]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Left)]),
            MoveBackward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('p')]),
            MoveUp(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Up)]),
            MoveUp(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('n')]),
            MoveDown(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Down)]),
            MoveDown(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('f')]),
            MoveWordForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('b')]),
            MoveWordBackward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl(KeyCode::Right)]),
            MoveWordForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl(KeyCode::Left)]),
            MoveWordBackward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('v')]),
            MoveHalfPageDown().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('v')]),
            MoveHalfPageUp().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::PageDown)]),
            MovePageDown().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::PageUp)]),
            MovePageUp().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('<')]),
            MoveToFirstRow(None).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('>')]),
            MoveToLastRow(None).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('a')]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Home)]),
            MoveToStartOfLine().into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::End)]),
            MoveToEndOfLine(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('e')]),
            MoveToEndOfLine(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('u')]),
            DeleteToFirstCharOfLine.into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('k')]),
            DeleteToEndOfLine.into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('o')]),
            LineBreak(1)
                .chain(MoveUp(1))
                .chain(MoveToEndOfLine(1))
                .into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Enter)]),
            LineBreak(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('j')]),
            LineBreak(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Backspace)]),
            DeleteChar(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('h')]),
            DeleteChar(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::new(KeyCode::Delete)]),
            DeleteCharForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::ctrl('d')]),
            DeleteCharForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt('d')]),
            DeleteWordForward(1).into(),
        ),
        (
            KeyEventRegister::i(vec![KeyInput::alt(KeyCode::Backspace)]),
            DeleteWordBackward(1).into(),
        ),
        (KeyEventRegister::i(vec![KeyInput::ctrl('u')]), Undo.into()),
        (KeyEventRegister::i(vec![KeyInput::ctrl('r')]), Redo.into()),
        (KeyEventRegister::i(vec![KeyInput::ctrl('y')]), Paste.into()),
        #[cfg(feature = "system-editor")]
        (
            KeyEventRegister::i(vec![KeyInput::alt('e')]),
            OpenSystemEditor.into(),
        ),
    ])
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct KeyInputSequence(Vec<KeyInput>);

impl KeyInputSequence {
    pub fn new(keys: Vec<KeyInput>) -> Self {
        KeyInputSequence(keys)
    }
}

impl From<Vec<KeyInput>> for KeyInputSequence {
    fn from(keys: Vec<KeyInput>) -> Self {
        KeyInputSequence(keys)
    }
}

#[allow(deprecated)]
impl From<Vec<deprecated::KeyEvent>> for KeyInputSequence {
    fn from(events: Vec<deprecated::KeyEvent>) -> Self {
        KeyInputSequence(events.into_iter().map(|event| event.into()).collect())
    }
}

impl From<KeyInputSequence> for Vec<KeyInput> {
    fn from(seq: KeyInputSequence) -> Self {
        seq.0
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct KeyEventRegister {
    keys: Vec<KeyInput>,
    mode: EditorMode,
}

type RegisterCB = fn(&mut EditorState);

#[derive(Clone, Debug)]
struct RegisterVal(pub fn(&mut EditorState));

impl KeyEventRegister {
    pub fn new<T>(key: T, mode: EditorMode) -> Self
    where
        T: Into<KeyInputSequence>,
    {
        Self {
            keys: key.into().into(),
            mode,
        }
    }

    pub fn n<T>(key: T) -> Self
    where
        T: Into<KeyInputSequence>,
    {
        Self::new(key, EditorMode::Normal)
    }

    pub fn v<T>(key: T) -> Self
    where
        T: Into<KeyInputSequence>,
    {
        Self::new(key, EditorMode::Visual)
    }

    pub fn i<T>(key: T) -> Self
    where
        T: Into<KeyInputSequence>,
    {
        Self::new(key, EditorMode::Insert)
    }

    pub fn s<T>(key: T) -> Self
    where
        T: Into<KeyInputSequence>,
    {
        Self::new(key, EditorMode::Search)
    }
}

impl KeyEventHandler {
    pub(crate) fn on_event<T>(&mut self, key: T, state: &mut EditorState)
    where
        T: Into<KeyInput> + Copy + std::fmt::Debug,
    {
        let mode = state.mode;
        let key_input = key.into().normalize_altgr();

        // Always insert characters in insert mode
        if mode == EditorMode::Insert {
            if let input::KeyCode::Char(c) = key_input.key {
                if key_input.modifiers == input::Modifiers::NONE
                    || key_input.modifiers == input::Modifiers::SHIFT
                {
                    if self.capture_on_insert {
                        state.capture();
                    }
                    InsertChar(c).execute(state);
                    return;
                }
            }

            if matches!(key_input.key, input::KeyCode::Tab)
                && key_input.modifiers == input::Modifiers::NONE
            {
                if self.capture_on_insert {
                    state.capture();
                }
                InsertChar('\t').execute(state);
                return;
            }
        }

        // Always add characters to search in search mode
        if mode == EditorMode::Search {
            if let input::KeyCode::Char(c) = key_input.key {
                if key_input.modifiers == input::Modifiers::NONE {
                    AppendCharToSearch(c).execute(state);
                    return;
                }
            }
        }

        // An action from a previous key is waiting for its character (a
        // motion after an operator too: `dfx`).
        if let Some(mut action) = self.pending_char.take() {
            let count = self.pending_count.take();
            if let input::KeyCode::Char(c) = key_input.key {
                if let Some(slot) = action.char_arg() {
                    *slot = Some(c);
                }
                self.run(counted(action, count), state);
            }
            self.operator = None;
            return;
        }

        // A text object's key, after an operator and `i` or `a` (`daw`).
        if let Some(around) = self.object.take() {
            let count = self.take_count();
            let operator = self.operator.take();
            if let (Some((operate, _)), input::KeyCode::Char(c)) = (operator, key_input.key) {
                if let Some(object) = crate::actions::object::named(c) {
                    let action = OnObject {
                        operator: operate.operator,
                        object,
                        around,
                        count: 1,
                    };
                    state.execute_recorded(counted(action.into(), count));
                }
            }
            return;
        }

        // A count, before a command or within one (`3dd`, `d2w`): its digits
        // aren't keys of a sequence; a 0 first is a motion (`0`).
        if matches!(mode, EditorMode::Normal | EditorMode::Visual)
            && key_input.modifiers == input::Modifiers::NONE
        {
            if let input::KeyCode::Char(c @ '0'..='9') = key_input.key {
                let typing = if self.lookup.is_empty() && self.operator.is_none() {
                    &mut self.count
                } else {
                    &mut self.inner_count
                };
                if c != '0' || typing.is_some() {
                    let digit = c as usize - '0' as usize;
                    let n = typing.unwrap_or(0).saturating_mul(10) + digit;
                    *typing = Some(n.min(99_999));
                    return;
                }
            }
        }

        // After an operator, its key again is its line, as many as counted
        // (`dd`, `3cc`); `i` and `a` begin a text object (`ci(`).
        if let Some((_, operator_key)) = &self.operator {
            if self.lookup.is_empty() {
                if key_input == *operator_key {
                    let lines = counted(MoveDownToFirst(1).into(), self.take_count());
                    self.run(lines, state);
                    return;
                }
                if key_input.modifiers == input::Modifiers::NONE {
                    if let input::KeyCode::Char(c @ ('i' | 'a')) = key_input.key {
                        self.object = Some(c == 'a');
                        return;
                    }
                }
            }
        }

        // Else lookup an action from the register. Actions that still need a
        // character argument are held back until the next keystroke, and an
        // operator until its motion.
        if let Some(mut action) = self.get(key_input, mode) {
            if let Action::Operate(operate @ Operate { motion: None, .. }) = &action {
                // Another operator after one (`dc`): neither.
                if self.operator.take().is_some() {
                    self.take_count();
                } else {
                    self.operator = Some((operate.clone(), key_input));
                }
                return;
            }
            let count = self.take_count();
            if action.char_arg().is_some_and(|slot| slot.is_none()) {
                self.pending_char = Some(action);
                self.pending_count = count;
            } else if self.operator.is_some() {
                self.run(counted(action, count), state);
            } else if matches!(action, Action::RepeatLastChange(_)) {
                // `3.`: the last change again, with that count from now on.
                if let Some(n) = count {
                    if let Some(last) = state.last_change.take() {
                        state.last_change = Some(recounted(last, n));
                    }
                }
                state.execute_recorded(action);
            } else {
                state.execute_recorded(counted(action, count));
            }
        } else if self.lookup.is_empty() {
            // Not a sequence after all: its count goes with it, and an
            // operator before it.
            self.take_count();
            self.operator = None;
        }
    }

    /// `action` done; with an operator waiting, the operator on it if it's a
    /// motion (`dw`), else nothing (`dx`).
    fn run(&mut self, action: Action, state: &mut EditorState) {
        match self.operator.take() {
            None => state.execute_recorded(action),
            Some((mut operate, _)) => {
                if action.motion().is_some() {
                    operate.motion = Some(Box::new(action));
                    state.execute_recorded(operate.into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(deprecated)]
    use super::deprecated::KeyEvent;
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn test_key_event_register_with_key_event() {
        let register = KeyEventRegister::n(vec![KeyEvent::Ctrl('a'), KeyEvent::Char('b')]);
        assert_eq!(register.mode, EditorMode::Normal);
        assert_eq!(register.keys.len(), 2);

        assert_eq!(register.keys[0], KeyInput::ctrl('a'));
        assert_eq!(register.keys[1], KeyInput::new('b'));
    }

    #[test]
    fn test_key_event_register_with_key_input() {
        let register = KeyEventRegister::i(vec![KeyInput::ctrl('a'), KeyInput::new('b')]);
        assert_eq!(register.mode, EditorMode::Insert);
        assert_eq!(register.keys.len(), 2);

        assert_eq!(register.keys[0], KeyInput::ctrl('a'));
        assert_eq!(register.keys[1], KeyInput::new('b'));
    }

    #[test]
    fn test_key_event_register_with_crossterm() {
        use crossterm::event::{KeyCode as CTKeyCode, KeyEvent as CTKeyEvent, KeyModifiers};

        let ct_key_event = CTKeyEvent::new(CTKeyCode::Char('a'), KeyModifiers::CONTROL);
        let key_input: KeyInput = ct_key_event.into();

        let register = KeyEventRegister::v(vec![key_input, KeyInput::new(CTKeyCode::Enter)]);
        assert_eq!(register.mode, EditorMode::Visual);
        assert_eq!(register.keys.len(), 2);

        assert_eq!(register.keys[0], KeyInput::ctrl('a'));
        assert_eq!(register.keys[1], KeyInput::new(CTKeyCode::Enter));
    }

    #[test]
    fn test_insert_hello_world() {
        use crate::EditorState;

        let mut state = EditorState::default();
        state.mode = EditorMode::Insert;

        let mut handler = KeyEventHandler::default();

        let inputs = vec![
            KeyInput::shift('H'),
            KeyInput::new('e'),
            KeyInput::new('l'),
            KeyInput::new('l'),
            KeyInput::new('o'),
            KeyInput::new(' '),
            KeyInput::shift('W'),
            KeyInput::new('o'),
            KeyInput::new('r'),
            KeyInput::new('l'),
            KeyInput::new('d'),
            KeyInput::shift('!'),
            KeyInput::new(KeyCode::Enter),
            KeyInput::shift('H'),
            KeyInput::new('i'),
            KeyInput::shift('!'),
        ];

        for input in inputs {
            handler.on_event(input, &mut state);
        }

        assert_eq!(state.lines.to_string(), String::from("Hello World!\nHi!"));
    }

    #[test]
    fn test_dot_repeats_last_change() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("aaaa"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `x` deletes one character, `.` repeats it twice more.
        handler.on_event(KeyInput::new('x'), &mut state);
        handler.on_event(KeyInput::new('.'), &mut state);
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "a");
    }

    #[test]
    fn test_dot_repeats_multikey_change() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("one two three"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `dw` deletes a word forward; `.` repeats the whole multi-key command.
        handler.on_event(KeyInput::new('d'), &mut state);
        handler.on_event(KeyInput::new('w'), &mut state);
        assert_eq!(state.lines.to_string(), "two three");

        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "three");
    }

    #[test]
    fn test_dot_is_noop_without_prior_change() {
        use crate::{EditorState, Lines};

        let mut state = EditorState::new(Lines::from("hello"));
        let mut handler = KeyEventHandler::default();

        // Motions are not changes, so `.` has nothing to repeat.
        handler.on_event(KeyInput::new('l'), &mut state);
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "hello");
    }

    #[test]
    fn test_dot_repeats_change_inner_word() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("foo bar baz"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `ciw` changes the inner word, then we type replacement text and Esc.
        handler.on_event(KeyInput::new('c'), &mut state);
        handler.on_event(KeyInput::new('i'), &mut state);
        handler.on_event(KeyInput::new('w'), &mut state);
        handler.on_event(KeyInput::new('x'), &mut state);
        handler.on_event(KeyInput::new('y'), &mut state);
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert_eq!(state.lines.to_string(), "xy bar baz");
        assert_eq!(state.mode, EditorMode::Normal);
        // Cursor rests on the last inserted character (Vim-style), not after it.
        assert_eq!(state.cursor, Index2::new(0, 1));

        // Move onto the next word and repeat the whole change with `.`.
        handler.on_event(KeyInput::new('w'), &mut state);
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "xy xy baz");
        assert_eq!(state.mode, EditorMode::Normal);
        // Mid-line too, the cursor lands on the last inserted character (the
        // second `y`), not on the trailing space after the word.
        assert_eq!(state.cursor, Index2::new(0, 4));
    }

    #[test]
    fn test_pending_tells_a_sequence_begun_and_a_character_awaited() {
        use crate::{EditorState, Lines};

        let mut state = EditorState::new(Lines::from("abc"));
        let mut handler = KeyEventHandler::default();
        assert!(!handler.pending());
        handler.on_event(KeyInput::new('d'), &mut state);
        assert!(handler.pending());
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert!(!handler.pending());
        handler.on_event(KeyInput::new('f'), &mut state);
        assert!(handler.pending());
        handler.on_event(KeyInput::new('c'), &mut state);
        assert!(!handler.pending());
        assert_eq!(state.cursor.col, 2);
    }

    fn typed(handler: &mut KeyEventHandler, state: &mut crate::EditorState, keys: &str) {
        for c in keys.chars() {
            let key = match c {
                '\x1b' => KeyInput::new(KeyCode::Esc),
                '\n' => KeyInput::new(KeyCode::Enter),
                '\u{8}' => KeyInput::new(KeyCode::Backspace),
                '\u{7f}' => KeyInput::new(KeyCode::Delete),
                '\u{17}' => KeyInput::ctrl('w'),
                '\u{15}' => KeyInput::ctrl('u'),
                c if c.is_uppercase() => KeyInput::shift(c),
                c => KeyInput::new(c),
            };
            handler.on_event(key, state);
        }
    }

    #[test]
    fn test_counts_before_and_within_commands() {
        use crate::clipboard::{ClipboardTrait, InternalClipboard};
        use crate::{EditorState, Lines};

        let text = "one two three four five six\nb\nc\nd\ne";
        let mut state = EditorState::new(Lines::from(text));
        state.set_clipboard(InternalClipboard::default());
        let mut handler = KeyEventHandler::default();
        // 3x takes three, yanked, in one undo step; . does three again.
        typed(&mut handler, &mut state, "3x");
        assert_eq!(
            state.lines.to_string(),
            " two three four five six\nb\nc\nd\ne"
        );
        assert_eq!(state.clip.get_text(), "one");
        typed(&mut handler, &mut state, ".");
        assert_eq!(state.lines.to_string(), "o three four five six\nb\nc\nd\ne");
        typed(&mut handler, &mut state, "u");
        assert_eq!(
            state.lines.to_string(),
            " two three four five six\nb\nc\nd\ne"
        );
        // 2. does it twice, from then on.
        typed(&mut handler, &mut state, "2.");
        assert_eq!(
            state.lines.to_string(),
            "wo three four five six\nb\nc\nd\ne"
        );
        // Motions: 3w, and a count within a command, d2w, multiplied.
        typed(&mut handler, &mut state, "03w");
        assert_eq!(state.cursor.col, 14);
        typed(&mut handler, &mut state, "0d2w");
        assert_eq!(state.lines.to_string(), "four five six\nb\nc\nd\ne");
        // Lines: 2dd puts both in the clipboard; 3G goes to line 3; 5j as far
        // as there are lines.
        typed(&mut handler, &mut state, "j2dd");
        assert_eq!(state.lines.to_string(), "four five six\nd\ne");
        assert_eq!(state.clip.get_text(), "\nb\nc");
        typed(&mut handler, &mut state, "3G");
        assert_eq!(state.cursor.row, 2);
        typed(&mut handler, &mut state, "gg5j");
        assert_eq!(state.cursor.row, 2);
        // 2p puts what was yanked twice, in one undo step; 2yy yanks two.
        typed(&mut handler, &mut state, "gg2yyj2p");
        assert_eq!(
            state.lines.to_string(),
            "four five six\nd\nfour five six\nd\nfour five six\nd\ne"
        );
        typed(&mut handler, &mut state, "u");
        assert_eq!(state.lines.to_string(), "four five six\nd\ne");
        // A count given up with its sequence (3d then Esc) counts for nothing.
        typed(&mut handler, &mut state, "gg3d\x1bx");
        assert_eq!(state.lines.to_string(), "our five six\nd\ne");
        // 0 alone is a motion; 10 a count.
        typed(&mut handler, &mut state, "$0");
        assert_eq!(state.cursor.col, 0);
        assert!(!handler.pending());
        typed(&mut handler, &mut state, "1");
        assert!(handler.pending());
    }

    #[test]
    fn test_counts_for_f_t_dollar_r_and_objects_and_too_many_fail() {
        use crate::clipboard::{ClipboardTrait, InternalClipboard};
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("a,b,c,d\nxy"));
        let mut handler = KeyEventHandler::default();
        // 2f, and 2t, to the second; 9f, past what there is: nowhere.
        typed(&mut handler, &mut state, "2f,");
        assert_eq!(state.cursor, Index2::new(0, 3));
        typed(&mut handler, &mut state, "02t,");
        assert_eq!(state.cursor, Index2::new(0, 2));
        typed(&mut handler, &mut state, "09f,");
        assert_eq!(state.cursor, Index2::new(0, 0));
        // 2$: the next line's end; on the last line, nowhere.
        typed(&mut handler, &mut state, "2$");
        assert_eq!(state.cursor, Index2::new(1, 1));
        typed(&mut handler, &mut state, "02$");
        assert_eq!(state.cursor, Index2::new(1, 0));
        // 3rz: three, the cursor on the last; . three more; too few: none.
        typed(&mut handler, &mut state, "gg3rz");
        assert_eq!(state.lines.to_string(), "zzz,c,d\nxy");
        assert_eq!(state.cursor, Index2::new(0, 2));
        typed(&mut handler, &mut state, "l.");
        assert_eq!(state.lines.to_string(), "zzzzzzd\nxy");
        typed(&mut handler, &mut state, "5ry");
        assert_eq!(state.lines.to_string(), "zzzzzzd\nxy");
        // Objects widened: what's yanked is all of it.
        let mut state = EditorState::new(Lines::from("one two three four five"));
        state.set_clipboard(InternalClipboard::default());
        typed(&mut handler, &mut state, "d2aw");
        assert_eq!(state.lines.to_string(), "three four five");
        typed(&mut handler, &mut state, "y2aw");
        assert_eq!(state.clip.get_text(), "three four ");
        typed(&mut handler, &mut state, "2cawX\x1b");
        assert_eq!(state.lines.to_string(), "Xfive");
    }

    #[test]
    fn test_g_and_gg_go_to_a_lines_first_non_blank() {
        use crate::{EditorState, Lines};

        let mut state = EditorState::new(Lines::from("  first\nmid\n    last"));
        let mut handler = KeyEventHandler::default();
        typed(&mut handler, &mut state, "llG");
        assert_eq!(state.cursor, crate::Index2::new(2, 4));
        typed(&mut handler, &mut state, "$gg");
        assert_eq!(state.cursor, crate::Index2::new(0, 2));
        typed(&mut handler, &mut state, "2G");
        assert_eq!(state.cursor, crate::Index2::new(1, 0));
    }

    #[test]
    fn test_words_find_back_and_first_non_blanks_as_vims_keys() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("  a foo.bar b\n x"));
        let mut handler = KeyEventHandler::default();
        typed(&mut handler, &mut state, "$^");
        assert_eq!(state.cursor, Index2::new(0, 2));
        typed(&mut handler, &mut state, "W");
        assert_eq!(state.cursor, Index2::new(0, 4));
        typed(&mut handler, &mut state, "E");
        assert_eq!(state.cursor, Index2::new(0, 10));
        typed(&mut handler, &mut state, "B");
        assert_eq!(state.cursor, Index2::new(0, 4));
        typed(&mut handler, &mut state, "$Fo");
        assert_eq!(state.cursor, Index2::new(0, 6));
        typed(&mut handler, &mut state, "$Ta");
        assert_eq!(state.cursor, Index2::new(0, 10));
        typed(&mut handler, &mut state, "2_");
        assert_eq!(state.cursor, Index2::new(1, 1));
    }

    #[test]
    fn test_j_joins_as_many_lines_as_counted_and_dot_again() {
        use crate::{EditorState, Lines};

        let mut state = EditorState::new(Lines::from("a\n  b\nc\nd\ne\nf"));
        let mut handler = KeyEventHandler::default();
        typed(&mut handler, &mut state, "J");
        assert_eq!(state.lines.to_string(), "a b\nc\nd\ne\nf");
        typed(&mut handler, &mut state, "j3J");
        assert_eq!(state.lines.to_string(), "a b\nc d e\nf");
        typed(&mut handler, &mut state, "k.");
        assert_eq!(state.lines.to_string(), "a b c d e f");
        // On the last line, nothing: . repeats what came before.
        typed(&mut handler, &mut state, "0xJ.");
        assert_eq!(state.lines.to_string(), "b c d e f");
    }

    #[test]
    fn test_counted_inserts_type_their_text_that_many_times() {
        use crate::{EditorState, Index2, Lines};

        let after = |text: &str, at: (usize, usize), keys: &str| {
            let mut state = EditorState::new(Lines::from(text));
            state.cursor = Index2::new(at.0, at.1);
            let mut handler = KeyEventHandler::default();
            typed(&mut handler, &mut state, keys);
            assert_eq!(state.mode, EditorMode::Normal);
            let cursor = (state.cursor.row, state.cursor.col);
            (state.lines.to_string(), cursor)
        };
        // The cursor on the last character typed; one u undoes it all.
        assert_eq!(after("ab", (0, 0), "3ix-\x1b"), ("x-x-x-ab".into(), (0, 5)));
        assert_eq!(after("ab", (0, 0), "3ix-\x1bu"), ("ab".into(), (0, 0)));
        assert_eq!(after("ab", (0, 0), "2ay\x1b"), ("ayyb".into(), (0, 2)));
        assert_eq!(after("ab", (0, 0), "2Az\x1b"), ("abzz".into(), (0, 3)));
        assert_eq!(after(" ab", (0, 2), "2I-\x1b"), (" --ab".into(), (0, 2)));
        // o and O: a line each.
        assert_eq!(
            after("a\nb", (0, 0), "3ox\x1b"),
            ("a\nx\nx\nx\nb".into(), (3, 0))
        );
        assert_eq!(
            after("a\nb", (1, 0), "2Ox\x1b"),
            ("a\nx\nx\nb".into(), (2, 0))
        );
        // . with the count, or its own; Enter typed again too.
        assert_eq!(after("ab", (0, 0), "3ix\x1b."), ("xxxxxxab".into(), (0, 4)));
        assert_eq!(
            after("a", (0, 0), "2ox\x1b."),
            ("a\nx\nx\nx\nx".into(), (4, 0))
        );
        assert_eq!(after("ab", (0, 0), "ix\x1b3."), ("xxxxab".into(), (0, 2)));
        assert_eq!(
            after("ab", (0, 0), "ix\nY\x1b."),
            ("x\nx\nYYab".into(), (2, 0))
        );
        // Backspace and Delete typed again as they were, with a count and
        // with `.`.
        assert_eq!(
            after("", (0, 0), "3ifoo\u{8}x\x1b"),
            ("foxfoxfox".into(), (0, 8))
        );
        assert_eq!(
            after("ab", (0, 0), "ifoo\u{8}x\x1b."),
            ("fofoxxab".into(), (0, 4))
        );
        assert_eq!(after("ab", (0, 0), "2ix\u{7f}\x1b"), ("xx".into(), (0, 1)));
        // After lines deleted at the end: no line left past the last.
        assert_eq!(
            after("a\nb", (1, 0), "Vd3o\x1b"),
            ("a\n\n\n".into(), (3, 0))
        );
        assert_eq!(
            after("a\nfoo", (1, 1), "diw3o\x1b"),
            ("a\n\n\n\n".into(), (4, 0))
        );
        // A change's count is its motion's: what's typed goes in once.
        assert_eq!(after("a b c d", (0, 0), "3cwX\x1b"), ("X d".into(), (0, 0)));
    }

    #[test]
    fn test_an_insert_is_one_undo_step_and_ctrl_w_is_vims() {
        use crate::clipboard::{ClipboardTrait, InternalClipboard};
        use crate::{EditorState, Index2, Lines};

        let after = |text: &str, at: (usize, usize), keys: &str| {
            let mut state = EditorState::new(Lines::from(text));
            state.set_clipboard(InternalClipboard::default());
            state.clip.set_text("kept".into());
            state.cursor = Index2::new(at.0, at.1);
            let mut handler = KeyEventHandler::default();
            typed(&mut handler, &mut state, keys);
            (state.lines.to_string(), state.clip.get_text())
        };
        let kept = |text: &str| (text.to_string(), "kept".to_string());
        // Backspace and Delete in an insert: one u undoes all of it.
        assert_eq!(after("ab cd", (0, 3), "ixy\u{8}z\x1bu"), kept("ab cd"));
        assert_eq!(after("ab cd", (0, 3), "ix\u{7f}\x1bu"), kept("ab cd"));
        assert_eq!(after("ab", (0, 0), "3ifoo\u{8}x\x1bu"), kept("ab"));
        assert_eq!(after("ab cd", (0, 0), "cwab\u{8}c\x1bu").0, "ab cd");
        // Ctrl+W: the word before, not yanked, stopping once where the
        // insert began, then going on.
        assert_eq!(after("ab cd", (0, 3), "axyz\u{17}\x1b"), kept("ab cd"));
        assert_eq!(after("ab cd", (0, 3), "axyz\u{17}\u{17}\x1b"), kept("ab d"));
        assert_eq!(after("ab cd", (0, 4), "a ef  \u{17}\x1b"), kept("ab cd "));
        // At a line's start, the line break.
        assert_eq!(after("ab\ncd", (1, 0), "i\u{17}\x1b"), kept("abcd"));
        // Ctrl+U: what was typed in this insert, not yanked; with nothing
        // typed, all before the cursor; once at the insert's start, then on.
        let two = "ab cd ef\nsecond line";
        assert_eq!(after(two, (0, 0), "jAxy\u{15}\x1b"), kept(two));
        assert_eq!(
            after("ab cd ef", (0, 3), "axy\u{15}q\x1b"),
            kept("ab cqd ef")
        );
        assert_eq!(after("ab cd ef", (0, 3), "i\u{15}\x1b"), kept("cd ef"));
        assert_eq!(
            after("ab cd ef", (0, 3), "axy\u{15}\u{15}\x1b"),
            kept("d ef")
        );
        assert_eq!(after("ab\ncd", (1, 0), "i\u{15}\x1b"), kept("abcd"));
        // Typed again with a count, each time only what it typed.
        assert_eq!(after("ab", (0, 0), "3ixy\u{15}z\x1b"), kept("zzzab"));
        assert_eq!(after("ab", (0, 0), "3ixy\u{17}z\x1b"), kept("zzzab"));
    }

    #[test]
    fn test_r_takes_any_character_and_dot_repeats_it() {
        use crate::{EditorState, Lines};

        let mut state = EditorState::new(Lines::from("ab\ncd"));
        let mut handler = KeyEventHandler::default();
        handler.on_event(KeyInput::new('r'), &mut state);
        handler.on_event(KeyInput::new('ą'), &mut state);
        handler.on_event(KeyInput::new('j'), &mut state);
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "ąb\nąd");
    }

    #[test]
    fn test_dot_repeats_insert_session() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("ab"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `i` opens an insert session; type `X` and leave with Esc.
        handler.on_event(KeyInput::new('i'), &mut state);
        handler.on_event(KeyInput::new('X'), &mut state);
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert_eq!(state.lines.to_string(), "Xab");
        assert_eq!(state.mode, EditorMode::Normal);

        // `.` replays the whole insert session at the cursor.
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "XXab");
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_dot_repeats_change_word() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("one two three"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `cw` changes the word under the cursor; type `X` and leave insert.
        handler.on_event(KeyInput::new('c'), &mut state);
        handler.on_event(KeyInput::new('w'), &mut state);
        handler.on_event(KeyInput::new('X'), &mut state);
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert_eq!(state.lines.to_string(), "X two three");

        // Put the cursor on the next word and repeat the change with `.`.
        state.cursor = Index2::new(0, 2);
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "X X three");
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_dot_repeats_open_line() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("a\nb"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `o` opens a line below and enters insert; type `X` and leave.
        handler.on_event(KeyInput::new('o'), &mut state);
        handler.on_event(KeyInput::new('X'), &mut state);
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert_eq!(state.lines.to_string(), "a\nX\nb");

        // `.` opens another line below the current one and replays the text.
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "a\nX\nX\nb");
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_dot_repeats_delete_line() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("a\nb\nc\nd"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `dd` deletes the current line; `.` repeats it.
        handler.on_event(KeyInput::new('d'), &mut state);
        handler.on_event(KeyInput::new('d'), &mut state);
        assert_eq!(state.lines.to_string(), "b\nc\nd");

        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "c\nd");
    }

    #[test]
    fn test_find_forward_moves_onto_char() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `fo` moves onto the next `o`.
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));

        // Repeating `fo` finds the following `o`, not the current one.
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.cursor, Index2::new(0, 7));
    }

    #[test]
    fn test_till_forward_moves_before_char() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `to` moves just before the next `o`.
        handler.on_event(KeyInput::new('t'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.cursor, Index2::new(0, 3));
    }

    #[test]
    fn test_find_forward_missing_char_is_noop() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // No `z` to the right, so the cursor does not move.
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('z'), &mut state);
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_find_forward_extends_visual_selection() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // In visual mode, `fo` extends the selection onto the next `o`.
        handler.on_event(KeyInput::new('v'), &mut state);
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.cursor, Index2::new(0, 4));
        assert!(state.selection.is_some());
    }

    #[test]
    fn test_delete_find_forward() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `dfo` deletes up to and including the first `o`.
        handler.on_event(KeyInput::new('d'), &mut state);
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.lines.to_string(), " world");
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_delete_till_forward() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `dto` deletes up to (but not including) the first `o`.
        handler.on_event(KeyInput::new('d'), &mut state);
        handler.on_event(KeyInput::new('t'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.lines.to_string(), "o world");
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_delete_find_forward_missing_char_is_noop() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // No `z` to the right, so nothing is deleted.
        handler.on_event(KeyInput::new('d'), &mut state);
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('z'), &mut state);
        assert_eq!(state.lines.to_string(), "hello world");
        assert_eq!(state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn test_dot_repeats_delete_find_forward() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("a.b.c."));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `df.` deletes up to and including the first `.`; `.` repeats it.
        handler.on_event(KeyInput::new('d'), &mut state);
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "b.c.");

        handler.on_event(KeyInput::new('.'), &mut state);
        assert_eq!(state.lines.to_string(), "c.");
    }

    #[test]
    fn test_change_find_forward() {
        use crate::{EditorMode, EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `cfo` deletes up to and including the first `o` and enters insert.
        handler.on_event(KeyInput::new('c'), &mut state);
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.mode, EditorMode::Insert);
        handler.on_event(KeyInput::new('X'), &mut state);
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert_eq!(state.lines.to_string(), "X world");
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_change_till_forward() {
        use crate::{EditorMode, EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `cto` deletes up to (but not including) the first `o` and enters insert.
        handler.on_event(KeyInput::new('c'), &mut state);
        handler.on_event(KeyInput::new('t'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.mode, EditorMode::Insert);
        handler.on_event(KeyInput::new('X'), &mut state);
        handler.on_event(KeyInput::new(KeyCode::Esc), &mut state);
        assert_eq!(state.lines.to_string(), "Xo world");
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_change_find_forward_missing_char_is_noop() {
        use crate::{EditorMode, EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // No `z` to the right, so nothing changes and insert is not entered.
        handler.on_event(KeyInput::new('c'), &mut state);
        handler.on_event(KeyInput::new('f'), &mut state);
        handler.on_event(KeyInput::new('z'), &mut state);
        assert_eq!(state.lines.to_string(), "hello world");
        assert_eq!(state.mode, EditorMode::Normal);
    }

    #[test]
    fn test_visual_till_forward() {
        use crate::{EditorState, Index2, Lines};

        let mut state = EditorState::new(Lines::from("hello world"));
        let mut handler = KeyEventHandler::default();
        state.cursor = Index2::new(0, 0);

        // `vto` selects up to (but not including) the first `o`.
        handler.on_event(KeyInput::new('v'), &mut state);
        handler.on_event(KeyInput::new('t'), &mut state);
        handler.on_event(KeyInput::new('o'), &mut state);
        assert_eq!(state.cursor, Index2::new(0, 3));
        assert!(state.selection.is_some());
    }

    #[test]
    fn test_altgr_normalization_inserts_characters() {
        use crate::EditorState;
        use crossterm::event::{KeyEvent as CTKeyEvent, KeyModifiers as CTMods};

        let mut state = EditorState::default();
        state.mode = EditorMode::Insert;

        let mut handler = KeyEventHandler::emacs_mode();

        // Simulate AltGr+[ (reported as Ctrl+Alt+[ on international keyboards)
        let altgr_bracket = CTKeyEvent::new(
            crossterm::event::KeyCode::Char('['),
            CTMods::CONTROL | CTMods::ALT,
        );
        handler.on_event(altgr_bracket, &mut state);

        // Simulate AltGr+]
        let altgr_bracket_close = CTKeyEvent::new(
            crossterm::event::KeyCode::Char(']'),
            CTMods::CONTROL | CTMods::ALT,
        );
        handler.on_event(altgr_bracket_close, &mut state);

        // Simulate AltGr+{ (with shift)
        let altgr_brace = CTKeyEvent::new(
            crossterm::event::KeyCode::Char('{'),
            CTMods::CONTROL | CTMods::ALT | CTMods::SHIFT,
        );
        handler.on_event(altgr_brace, &mut state);

        // Simulate AltGr+}
        let altgr_brace_close = CTKeyEvent::new(
            crossterm::event::KeyCode::Char('}'),
            CTMods::CONTROL | CTMods::ALT | CTMods::SHIFT,
        );
        handler.on_event(altgr_brace_close, &mut state);

        assert_eq!(state.lines.to_string(), "[]{}");
    }

    #[test]
    fn test_altgr_does_not_affect_letter_keybindings() {
        use crate::EditorState;

        let mut state = EditorState::new(crate::Lines::from("Hello World"));
        state.mode = EditorMode::Insert;

        let mut handler = KeyEventHandler::emacs_mode();

        // Alt+f should move forward word, not insert 'f'
        let alt_f = KeyInput::alt('f');
        handler.on_event(alt_f, &mut state);

        // Cursor should have moved to position 6 ('W'), not inserted 'f'
        assert_eq!(state.cursor.col, 6);
        assert_eq!(state.lines.to_string(), "Hello World");
    }
}
