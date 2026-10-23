//! The note editor: a session's note, under the preview's header, with vim's
//! keys (edtui's, and a few it lacks), and roster's on top: `:w`, `:wq`, `:q`,
//! `:q!`, `ZZ` and `ZQ` save and close as in vim, `mm` pins a line, Ctrl+G
//! hands the note to `$EDITOR`, and keys typed quickly (`jk`, `notes.escape`)
//! can leave insert mode. Esc never closes it: it's pressed out of habit,
//! often twice.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use edtui::actions::delete::{DeleteToEndOfLine, DeleteWordBackward, RemoveChar, ReplaceChar};
use edtui::actions::{
    Chainable, CopySelection, MoveBackward, MoveForward, MoveToFirst, MoveToStartOfLine, Paste,
    SwitchMode,
};
use edtui::events::{KeyEventRegister, KeyInput};
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::input;
use crate::model::Session;
use crate::note::{self, Note};

/// Columns a tab takes, in the editor and where its lines are counted.
pub const TAB_WIDTH: usize = 4;

/// What `r` replaces with as edtui's own keys (so `.` repeats it): ASCII, and
/// Latin-1 and Latin Extended-A (Polish, Czech, German, French letters, …).
const REPLACEABLE: [std::ops::RangeInclusive<char>; 2] = [' '..='~', '\u{a0}'..='\u{17f}'];

/// The mark's key: a change of roster's own (`x`, `X`) is given to edtui as
/// this, which it keeps as its last for `.` to repeat. No key typed reaches
/// edtui as it (`shifted`).
fn mark() -> KeyInput {
    KeyInput::shift('.')
}

/// Keys that leave insert mode typed quickly (`jk`), and how soon each must
/// follow the one before.
pub type Escape = (Vec<char>, Duration);

pub struct NoteEditor {
    /// Whose: the session's id.
    pub session: String,
    /// edtui's state; drawing it needs it mutable (it keeps its scroll there).
    pub state: RefCell<EditorState>,
    keys: EditorEventHandler,
    pub pin: Option<usize>,
    /// The text (tidied) and pin as last saved.
    saved: (String, Option<usize>),
    /// The note as it was opened, or last saved: what a save expects to
    /// replace (one saved by another meanwhile isn't, without `:w!`).
    pub base: Option<Note>,
    /// The lines before the last key, for the pin to follow its line.
    lines: Vec<String>,
    /// `Z` or `m`, waiting for its second key.
    pending: Option<char>,
    /// The change of roster's own (`x` or `X`) the mark stands for.
    repeat: Option<char>,
    /// The key (`f`, `t` or `r`) after which the next is a character for it,
    /// not a command.
    char_arg: Option<char>,
    /// The `:` command being typed.
    pub command: Option<String>,
    escape: Option<Escape>,
    /// How many of `escape`'s keys were typed, held back until it's clear
    /// whether the rest follow, and when the last came.
    held: Option<(usize, Instant)>,
}

/// What a key asks of the app.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    None,
    /// Save the note, then close the editor or stay in it; `force`d, also
    /// over one saved by another meanwhile.
    Save {
        close: bool,
        force: bool,
    },
    /// Close without saving.
    Close,
    /// Hand the text to `$EDITOR`.
    External,
    /// Say why not.
    Fail(String),
    /// Say this (not an error).
    Notice(String),
}

impl NoteEditor {
    /// Opens on the session's note, the cursor on its pinned line.
    pub fn new(s: &Session, escape: Option<Escape>) -> NoteEditor {
        let (text, pin) = s
            .note
            .as_ref()
            .map_or((String::new(), None), |n| (n.text.clone(), n.pin));
        let mut state = EditorState::new(Lines::from(&text));
        if let Some(pin) = pin {
            state.cursor = Index2::new(pin, 0);
        }
        let lines = rows(&state);
        NoteEditor {
            session: s.id.clone(),
            state: RefCell::new(state),
            keys: vim_keys(),
            pin,
            saved: (text, pin),
            base: s.note.clone(),
            lines,
            pending: None,
            repeat: None,
            char_arg: None,
            command: None,
            escape,
            held: None,
        }
    }

    /// The escape keys held back, shown where they'd go.
    pub fn held(&self) -> String {
        match (&self.held, &self.escape) {
            (Some((n, _)), Some((keys, _))) => keys[..*n].iter().collect(),
            _ => String::new(),
        }
    }

    /// When keys held back go in, unless the rest follow.
    pub fn deadline(&self) -> Option<Instant> {
        let (_, at) = self.held?;
        Some(at + self.escape.as_ref()?.1)
    }

    /// Puts keys held back in once it's too late for the rest; whether it did.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|d| now >= d) {
            self.flush();
            return true;
        }
        false
    }

    /// The escape keys held back, typed after all.
    fn flush(&mut self) {
        let (Some((n, _)), Some((keys, _))) = (self.held.take(), self.escape.clone()) else {
            return;
        };
        for &c in &keys[..n] {
            self.feed(shifted(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
    }

    /// In insert mode, a key of `escape`: held back, or the last, leaving
    /// insert mode. Whether it was one.
    fn escaping(&mut self, e: KeyEvent, now: Instant) -> bool {
        let Some((keys, within)) = self.escape.clone() else {
            return false;
        };
        if self.mode() != EditorMode::Insert {
            return false;
        }
        if self.held.is_some_and(|(_, at)| now >= at + within) {
            self.flush();
        }
        let typed = match e.code {
            KeyCode::Char(c)
                if !e
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                c
            }
            _ => {
                self.flush();
                return false;
            }
        };
        let n = self.held.map_or(0, |(n, _)| n);
        if n > 0 && keys[n] != typed {
            // Not the rest after all: what was held goes in, and this key
            // may start them again.
            self.flush();
            return self.escaping(e, now);
        }
        if keys[n] != typed {
            return false;
        }
        if n + 1 == keys.len() {
            self.held = None;
            self.feed(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        } else {
            self.held = Some((n + 1, now));
        }
        true
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn mode(&self) -> EditorMode {
        self.state.borrow().mode
    }

    /// Changed since it was last saved (the pin too).
    pub fn dirty(&self) -> bool {
        (note::tidy(&self.text()), self.pin) != self.saved
    }

    /// Saved as `note` (None: removed).
    pub fn saved(&mut self, note: Option<&Note>) {
        self.saved = note.map_or((String::new(), None), |n| (n.text.clone(), n.pin));
        self.pin = self.saved.1;
        self.base = note.cloned();
    }

    /// The text as `$EDITOR` left it; the pin follows its line.
    pub fn replace(&mut self, text: &str) {
        let mut state = self.state.borrow_mut();
        let cursor = state.cursor;
        let mode = state.mode;
        *state = EditorState::new(Lines::from(text));
        let rows = rows(&state);
        let last = rows.len().saturating_sub(1);
        state.cursor = Index2::new(cursor.row.min(last), 0);
        state.mode = if mode == EditorMode::Insert {
            EditorMode::Insert
        } else {
            EditorMode::Normal
        };
        drop(state);
        self.follow(rows);
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Outcome {
        self.on_key_at(e, Instant::now())
    }

    pub fn on_key_at(&mut self, e: KeyEvent, now: Instant) -> Outcome {
        let e = shifted(e);
        if self.command.is_some() {
            return self.on_command_key(e);
        }
        // edtui panics on keys it doesn't know (F keys, Shift+Tab, Insert).
        if !matches!(
            e.code,
            KeyCode::Char(_)
                | KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Tab
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        ) {
            return Outcome::None;
        }
        if self.escaping(e, now) {
            return Outcome::None;
        }
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && e.code == KeyCode::Char('g') {
            // Nothing waits on a key across $EDITOR: not roster, nor edtui.
            self.pending = None;
            if self.char_arg.take().is_some() || self.mode() == EditorMode::Normal {
                self.feed(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
            return Outcome::External;
        }
        let plain = !ctrl && !e.modifiers.contains(KeyModifiers::ALT);
        if self.mode() == EditorMode::Normal && self.char_arg.is_none() {
            if let Some(first) = self.pending.take() {
                return match (first, e.code) {
                    ('Z', KeyCode::Char('Z')) if plain => self.write_and_close(false),
                    ('Z', KeyCode::Char('Q')) if plain => Outcome::Close,
                    ('m', KeyCode::Char('m')) if plain => {
                        self.toggle_pin();
                        Outcome::None
                    }
                    _ => Outcome::None,
                };
            }
            if let KeyCode::Char(c @ (':' | 'Z' | 'm')) = e.code
                && plain
            {
                // edtui has nothing on these keys; given to it, they end a
                // sequence it was in (`d:`), so it doesn't go on after.
                self.feed(e);
                if c == ':' {
                    self.command = Some(String::new());
                } else {
                    self.pending = Some(c);
                }
                return Outcome::None;
            }
            // `x` (Delete too) and `X` are roster's (`take_char`), and `.`
            // repeats them by way of the mark.
            if matches!(e.code, KeyCode::Char('x' | 'X') | KeyCode::Delete) && plain {
                let change = if e.code == KeyCode::Char('X') {
                    'X'
                } else {
                    'x'
                };
                // Unless it ends a sequence edtui was in (`dx`), which then
                // takes nothing, as in vim.
                self.keys.on_key_event(mark(), &mut self.state.borrow_mut());
                if self.unmarked() {
                    self.repeat = Some(change);
                    self.take_char(change);
                }
                return Outcome::None;
            }
        }
        // Ctrl+C is Esc, as in vim.
        let e = if ctrl && e.code == KeyCode::Char('c') {
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        } else {
            e
        };
        let mode = self.mode();
        // What's searched for is typed without Shift, in edtui (capitals too).
        let mut e = e;
        if mode == EditorMode::Search && matches!(e.code, KeyCode::Char(_)) {
            e.modifiers.remove(KeyModifiers::SHIFT);
        }
        let was_char_arg = self.char_arg.take();
        match (was_char_arg, e.code) {
            // `r` with a character not in edtui's keymap (`rą`): replaced here
            // (given to edtui first, it ends the `r` it waits on).
            (Some('r'), KeyCode::Char(c))
                if plain && !REPLACEABLE.iter().any(|r| r.contains(&c)) =>
            {
                self.feed(e);
                self.state.borrow_mut().execute(ReplaceChar(c));
                let rows = rows(&self.state.borrow());
                self.follow(rows);
            }
            _ => self.feed(e),
        }
        // `.` that replayed the mark: roster's change again. However edtui
        // took it as `.`: Alt+`.` too, and after `yf`, which it doesn't know.
        if e.code == KeyCode::Char('.')
            && mode == EditorMode::Normal
            && self.unmarked()
            && let Some(change) = self.repeat
        {
            self.take_char(change);
        }
        // Keys that take the next character: f and t (df, ct, …), and r.
        self.char_arg = match e.code {
            KeyCode::Char(c @ ('f' | 't'))
                if matches!(mode, EditorMode::Normal | EditorMode::Visual) =>
            {
                Some(c)
            }
            KeyCode::Char('r') if mode == EditorMode::Normal => Some('r'),
            _ => None,
        }
        .filter(|_| was_char_arg.is_none() && plain);
        Outcome::None
    }

    fn on_command_key(&mut self, e: KeyEvent) -> Outcome {
        let Some(command) = self.command.as_mut() else {
            return Outcome::None;
        };
        if input::cancels(e) || (e.code == KeyCode::Backspace && command.is_empty()) {
            self.command = None;
            return Outcome::None;
        }
        if e.code == KeyCode::Enter {
            let command = self.command.take().unwrap_or_default();
            return self.run(command.trim());
        }
        input::edit_line(command, e);
        Outcome::None
    }

    /// A `:` command.
    fn run(&mut self, command: &str) -> Outcome {
        match command {
            "" => Outcome::None,
            // Nothing of yours to save, so nothing over a note saved meanwhile.
            "w" | "w!" if !self.dirty() => Outcome::Notice("Nothing new to save".into()),
            "wq" | "wq!" if !self.dirty() => Outcome::Close,
            "w" => Outcome::Save {
                close: false,
                force: false,
            },
            "w!" => Outcome::Save {
                close: false,
                force: true,
            },
            "wq" => Outcome::Save {
                close: true,
                force: false,
            },
            "wq!" => Outcome::Save {
                close: true,
                force: true,
            },
            "x" | "xit" | "exit" => self.write_and_close(false),
            "x!" | "xit!" | "exit!" => self.write_and_close(true),
            "q" | "quit" if self.dirty() => Outcome::Fail(
                "The note has unsaved changes: :wq saves them, :q! leaves them".into(),
            ),
            "q" | "quit" | "q!" | "quit!" => Outcome::Close,
            other => Outcome::Fail(format!(
                "Not a command here: :{other} (:w saves, :wq or :x closes too, :q! leaves changes)"
            )),
        }
    }

    /// `:x` and `ZZ`: saves if changed, and closes.
    fn write_and_close(&self, force: bool) -> Outcome {
        if self.dirty() {
            Outcome::Save { close: true, force }
        } else {
            Outcome::Close
        }
    }

    /// `mm`: pins the cursor's line, or unpins it if it's pinned.
    fn toggle_pin(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        let row = self.state.borrow().cursor.row;
        self.pin = (self.pin != Some(row)).then_some(row);
    }

    /// `x`, or `X`: the character under the cursor, or before it, yanked and
    /// taken out. As in vim, nothing on an empty line, nor for `X` at a
    /// line's start (where the cursor shows: `feed`).
    fn take_char(&mut self, change: char) {
        let rows = {
            let mut state = self.state.borrow_mut();
            let len = state.lines.len_col(state.cursor.row).unwrap_or(0);
            let col = state.cursor.col.min(len.saturating_sub(1));
            if len == 0 || (change == 'X' && col == 0) {
                return;
            }
            state.cursor.col = col;
            if change == 'X' {
                state.execute(MoveBackward(1));
            }
            // Selected, it's copied first.
            state.execute(
                SwitchMode(EditorMode::Visual)
                    .chain(CopySelection)
                    .chain(SwitchMode(EditorMode::Normal))
                    .chain(RemoveChar(1)),
            );
            rows(&state)
        };
        self.follow(rows);
    }

    /// Back in normal mode if edtui just ran the mark, given it or by `.`:
    /// whether it did.
    fn unmarked(&mut self) -> bool {
        let mut state = self.state.borrow_mut();
        let marked = state.mode == EditorMode::Search;
        if marked {
            state.mode = EditorMode::Normal;
        }
        marked
    }

    /// A key for edtui; the pin follows its line through what it did.
    fn feed(&mut self, e: KeyEvent) {
        let rows = {
            let mut state = self.state.borrow_mut();
            // edtui keeps the column through a shorter line (`j` on to a longer
            // one comes back to it, as in vim), but its edits then act past
            // the line's end: anything else acts where the cursor shows.
            let vertical = matches!(
                e.code,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            ) || matches!(e.code, KeyCode::Char('j' | 'k'))
                && state.mode == EditorMode::Normal
                && e.modifiers.is_empty()
                || matches!(e.code, KeyCode::Char('d' | 'u'))
                    && e.modifiers == KeyModifiers::CONTROL;
            let len = state.lines.len_col(state.cursor.row).unwrap_or(0);
            let last = match state.mode {
                EditorMode::Insert => Some(len),
                EditorMode::Normal => Some(len.saturating_sub(1)),
                EditorMode::Visual | EditorMode::Search => None,
            };
            if let Some(last) = last.filter(|_| !vertical) {
                state.cursor.col = state.cursor.col.min(last);
            }
            self.keys.on_key_event(e, &mut state);
            rows(&state)
        };
        self.follow(rows);
    }

    fn follow(&mut self, rows: Vec<String>) {
        if rows != self.lines {
            let old: Vec<&str> = self.lines.iter().map(String::as_str).collect();
            let new: Vec<&str> = rows.iter().map(String::as_str).collect();
            self.pin = note::follow_pin(&old, &new, self.pin);
            self.lines = rows;
        }
    }

    /// What the keys do in its mode, for the footer.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        let hints: &[(&str, &'static str)] = if self.command.is_some() {
            &[("Enter", "run"), ("Esc", "cancel")]
        } else {
            match self.mode() {
                EditorMode::Normal => &[
                    ("i", "insert"),
                    (":w", "save"),
                    (":wq", "save and close"),
                    (":q!", "close, leave changes"),
                    ("mm", "pin a line"),
                    ("C-g", "$EDITOR"),
                ],
                EditorMode::Insert => &[("Esc", "normal mode"), ("C-g", "$EDITOR")],
                EditorMode::Visual => &[("y", "yank"), ("d", "delete"), ("Esc", "normal mode")],
                EditorMode::Search => &[("Enter", "find"), ("Esc", "cancel")],
            }
        };
        hints
            .iter()
            .map(|(k, what)| (k.to_string(), *what))
            .collect()
    }
}

/// edtui's vim keys, `p` as vim's (below the last line too), some it lacks
/// (`C`, `cc` and `S`, `^`, `r`, and Ctrl+W in insert mode, as in roster's
/// other text fields), and the mark. (`x` and `X` are roster's: `on_key_at`.)
fn vim_keys() -> EditorEventHandler {
    let mut keys = EditorEventHandler::vim_mode();
    let k = &mut keys.key_handler;
    // `r` and a character, each its own key sequence, so `.` repeats it
    // (`rx` ticks a box, `.` the next): ASCII and the Latin letters of most
    // European languages; others are replaced in `on_key_at`.
    for c in REPLACEABLE.into_iter().flatten() {
        let key = match c.is_uppercase() {
            true => KeyInput::shift(c),
            false => KeyInput::new(c),
        };
        k.insert(
            KeyEventRegister::n(vec![KeyInput::new('r'), key]),
            ReplaceChar(c),
        );
    }
    let insert = || SwitchMode(EditorMode::Insert);
    // Into insert mode first, so one `u` undoes the whole change (its undo
    // point then holds the text before it). `D` leaves the cursor on the
    // last character left: insert after it, as `a`.
    k.insert(
        KeyEventRegister::n(vec![KeyInput::shift('C')]),
        insert().chain(DeleteToEndOfLine).chain(MoveForward(1)),
    );
    for change_line in [
        KeyEventRegister::n(vec![KeyInput::new('c'), KeyInput::new('c')]),
        KeyEventRegister::n(vec![KeyInput::shift('S')]),
    ] {
        k.insert(
            change_line,
            insert().chain(MoveToStartOfLine()).chain(DeleteToEndOfLine),
        );
    }
    // edtui's `x` and Delete, which don't yank, out: those are roster's. (A
    // key roster gives edtui as f's character after a `yf`, which edtui
    // doesn't know, would reach them.)
    for key in [KeyInput::new('x'), KeyInput::new(KeyCode::Delete)] {
        k.remove(&KeyEventRegister::n(vec![key]));
    }
    // The mark: does nothing but leave edtui in search mode, which no change
    // of edtui's does. Insert mode on the way, by way of search mode (no
    // undo point), makes it a change edtui keeps for `.`.
    k.insert(
        KeyEventRegister::n(vec![mark()]),
        SwitchMode(EditorMode::Search)
            .chain(SwitchMode(EditorMode::Insert))
            .chain(SwitchMode(EditorMode::Search)),
    );
    // `p` puts a line below the cursor's, the last one's too: edtui puts it
    // above the last line, except in insert mode. Into it by way of search
    // mode, which takes no undo point (so one `u` undoes it), and out the
    // same way, which leaves the cursor where the paste left it.
    k.insert(
        KeyEventRegister::n(vec![KeyInput::new('p')]),
        SwitchMode(EditorMode::Search)
            .chain(SwitchMode(EditorMode::Insert))
            .chain(Paste)
            .chain(SwitchMode(EditorMode::Search))
            .chain(SwitchMode(EditorMode::Normal)),
    );
    k.insert(KeyEventRegister::n(vec![KeyInput::new('^')]), MoveToFirst());
    k.insert(KeyEventRegister::v(vec![KeyInput::new('^')]), MoveToFirst());
    k.insert(
        KeyEventRegister::i(vec![KeyInput::ctrl('w')]),
        DeleteWordBackward(1),
    );
    keys
}

/// A key with Shift as edtui's keys have it: on capitals (`A`), not on other
/// characters (`$`, `%`), whatever the terminal reported.
fn shifted(mut e: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(c) = e.code {
        if c.is_uppercase() {
            e.modifiers.insert(KeyModifiers::SHIFT);
        } else if !c.is_alphabetic() {
            e.modifiers.remove(KeyModifiers::SHIFT);
        }
    }
    e
}

/// The editor's lines, as text.
fn rows(state: &EditorState) -> Vec<String> {
    state
        .lines
        .iter_row()
        .map(|row| row.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    fn editor(text: Option<&str>, pin: Option<usize>) -> NoteEditor {
        let project = Project {
            id: 1,
            name: "api".into(),
            origin: "/tmp".into(),
            agent: None,
            model: None,
            effort: None,
        };
        let mut s = Session::new("r-1", &project, "fix", "claude", 0);
        s.note = text.map(|t| Note {
            text: t.into(),
            pin,
            at: 0,
        });
        NoteEditor::new(&s, None)
    }

    fn keys(e: &mut NoteEditor, typed: &str) -> Vec<Outcome> {
        typed
            .chars()
            .map(|c| {
                let code = match c {
                    '\n' => KeyCode::Enter,
                    '\x1b' => KeyCode::Esc,
                    c => KeyCode::Char(c),
                };
                // Capitals with Shift, as terminals send them.
                let shift = match c.is_uppercase() {
                    true => KeyModifiers::SHIFT,
                    false => KeyModifiers::NONE,
                };
                e.on_key(KeyEvent::new(code, shift))
            })
            .filter(|o| *o != Outcome::None)
            .collect()
    }

    #[test]
    fn it_saves_and_closes_as_vim_does() {
        let mut e = editor(Some("one"), None);
        assert_eq!(keys(&mut e, ":q\n"), [Outcome::Close]);
        assert_eq!(keys(&mut e, "ZZ"), [Outcome::Close]);
        // Unchanged: nothing saved, so nothing over a note saved meanwhile.
        assert_eq!(keys(&mut e, ":wq\n"), [Outcome::Close]);
        assert!(matches!(keys(&mut e, ":w!\n")[..], [Outcome::Notice(_)]));
        keys(&mut e, "Atwo\x1b");
        assert_eq!(e.text(), "onetwo");
        assert!(e.dirty());
        // Not with unsaved changes; :q! and ZQ leave them.
        assert!(matches!(keys(&mut e, ":q\n")[..], [Outcome::Fail(_)]));
        assert_eq!(keys(&mut e, ":q!\n"), [Outcome::Close]);
        assert_eq!(keys(&mut e, "ZQ"), [Outcome::Close]);
        let save = |close, force| [Outcome::Save { close, force }];
        assert_eq!(keys(&mut e, ":w\n"), save(false, false));
        assert_eq!(keys(&mut e, ":wq\n"), save(true, false));
        assert_eq!(keys(&mut e, "ZZ"), save(true, false));
        assert_eq!(keys(&mut e, ":x\n"), save(true, false));
        // With `!`, also over a note saved meanwhile.
        assert_eq!(keys(&mut e, ":w!\n"), save(false, true));
        assert_eq!(keys(&mut e, ":wq!\n"), save(true, true));
        assert_eq!(keys(&mut e, ":x!\n"), save(true, true));
        assert!(matches!(keys(&mut e, ":e\n")[..], [Outcome::Fail(_)]));
        // Saved, it's clean again.
        e.saved(Some(&Note {
            text: "onetwo".into(),
            pin: None,
            at: 0,
        }));
        assert!(!e.dirty());
    }

    #[test]
    fn esc_never_closes_it() {
        let mut e = editor(None, None);
        assert_eq!(keys(&mut e, "\x1b\x1bihi\x1b\x1b\x1b"), []);
        assert_eq!(e.text(), "hi");
        // Nor does a command given up on.
        assert_eq!(keys(&mut e, ":wq\x1b"), []);
        assert_eq!(e.command, None);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(e.on_key(ctrl_c), Outcome::None);
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(e.on_key(ctrl_g), Outcome::External);
    }

    #[test]
    fn mm_pins_the_cursors_line_and_the_pin_follows_it() {
        let mut e = editor(Some("TODO\n- one\n- two\n- three"), None);
        keys(&mut e, "jjmm");
        assert_eq!(e.pin, Some(2));
        assert!(e.dirty());
        // Lines added and removed above move it; it stays on "- two".
        keys(&mut e, "ggOnew\x1b");
        assert_eq!(e.pin, Some(3));
        keys(&mut e, "dd");
        assert_eq!(e.pin, Some(2));
        // Removed, the next line takes it; mm there unpins.
        keys(&mut e, "jjdd");
        assert_eq!(e.lines[2], "- three");
        assert_eq!(e.pin, Some(2));
        keys(&mut e, "mm");
        assert_eq!(e.pin, None);
    }

    #[test]
    fn keys_vim_takes_a_character_after_are_vims() {
        // fm finds an m; it doesn't start a pin, nor fZ a close.
        let mut e = editor(Some("a m Z :"), None);
        keys(&mut e, "fm");
        assert_eq!(e.state.borrow().cursor.col, 2);
        keys(&mut e, "m");
        assert_eq!(e.pin, None);
        keys(&mut e, "\x1bfZ");
        assert_eq!(e.state.borrow().cursor.col, 4);
        assert_eq!(keys(&mut e, "Z"), []);
        keys(&mut e, "\x1bf:");
        assert_eq!(e.state.borrow().cursor.col, 6);
        assert_eq!(e.command, None);
        // `d` then `:` doesn't leave edtui waiting to delete.
        keys(&mut e, "0d:\x1bw");
        assert_eq!(e.text(), "a m Z :");
    }

    #[test]
    fn vim_keys_edtui_lacks() {
        // C changes to the end of the line; . repeats it.
        let mut e = editor(Some("one two\nsix seven"), None);
        keys(&mut e, "wCfour\x1b");
        assert_eq!(e.text(), "one four\nsix seven");
        keys(&mut e, "j0w.");
        assert_eq!(e.text(), "one four\nsix four");
        // At the start of a line, and on an empty one.
        keys(&mut e, "0Cx\x1bonew\x1b0C\x1b");
        assert_eq!(e.text(), "one four\nx\n");
        keys(&mut e, "Cy\x1b");
        assert_eq!(e.text(), "one four\nx\ny");
        // cc and S change the whole line; ^ goes to its first character.
        keys(&mut e, "ggccfirst\x1bjSsecond\x1b");
        assert_eq!(e.text(), "first\nsecond\ny");
        keys(&mut e, "ggI  \x1b$^x");
        assert_eq!(e.text(), "  irst\nsecond\ny");
        // Ctrl+W takes the word before the cursor, in insert mode.
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        keys(&mut e, "Ga and more");
        e.on_key(ctrl_w);
        keys(&mut e, "\x1b");
        assert_eq!(e.text(), "  irst\nsecond\ny and ");
    }

    #[test]
    fn x_and_shift_x_yank_and_p_puts_after_as_in_vim() {
        // xp swaps two characters; X takes the one before the cursor.
        let mut e = editor(Some("ab\ncd"), None);
        keys(&mut e, "xp");
        assert_eq!(e.text(), "ba\ncd");
        // One u undoes each.
        keys(&mut e, "u");
        assert_eq!(e.text(), "b\ncd");
        keys(&mut e, "u");
        assert_eq!(e.text(), "ab\ncd");
        keys(&mut e, "j$Xp");
        assert_eq!(e.text(), "ab\ndc");
        // P puts before the cursor; . repeats x, yanking too.
        keys(&mut e, "0xP");
        assert_eq!(e.text(), "ab\ndc");
        keys(&mut e, "k0x.p");
        assert_eq!(e.text(), "b\ndc");
        // X at a line's start, and x on an empty line, take nothing, and
        // leave what was yanked.
        keys(&mut e, "0Xo\x1bxp");
        assert_eq!(e.text(), "b\nb\ndc");
    }

    #[test]
    fn dot_repeats_x_and_shift_x_as_vim_does() {
        // X.. takes back to the line's start, and no further.
        let mut e = editor(Some("abc"), None);
        keys(&mut e, "$X..p");
        assert_eq!(e.text(), "ca");
        // On an empty line, it takes nothing and leaves what was yanked.
        let mut e = editor(Some("ab\n\ncd"), None);
        keys(&mut e, "xj.p");
        assert_eq!(e.text(), "b\na\ncd");
        // An x that took nothing is still what . repeats, not the dd before.
        let mut e = editor(Some("a\nb\n\nc"), None);
        keys(&mut e, "ddjx.");
        assert_eq!(e.text(), "b\n\nc");
        keys(&mut e, "j.");
        assert_eq!(e.text(), "b\n\n");
        // A change after it is what . repeats then.
        let mut e = editor(Some("ab"), None);
        keys(&mut e, "xiz\x1b.");
        assert_eq!(e.text(), "zzb");
        // Ending a sequence (dx), x takes nothing; Delete is x.
        let mut e = editor(Some("ab"), None);
        keys(&mut e, "dx");
        assert_eq!(e.text(), "ab");
        e.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        keys(&mut e, "p");
        assert_eq!(e.text(), "ba");
        // One u undoes X; an X that took nothing left nothing to undo.
        keys(&mut e, "$X0Xu");
        assert_eq!(e.text(), "ba");
        // A `.` edtui takes another way repeats it too, and the editor stays
        // in normal mode: Alt+`.`, and `.` after `yf`, which edtui doesn't
        // know. Nor does a key after `yf` reach an x of edtui's.
        let mut e = editor(Some("abcd"), None);
        keys(&mut e, "x");
        e.on_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT));
        assert_eq!((e.text().as_str(), e.mode()), ("cd", EditorMode::Normal));
        keys(&mut e, "yf.");
        assert_eq!((e.text().as_str(), e.mode()), ("d", EditorMode::Normal));
        keys(&mut e, "yfxp");
        assert_eq!(e.text(), "dc");
    }

    #[test]
    fn ddp_swaps_a_line_with_the_next_the_last_but_one_too() {
        let mut e = editor(Some("one\ntwo"), None);
        keys(&mut e, "ddp");
        assert_eq!(e.text(), "two\none");
        keys(&mut e, "u");
        assert_eq!(e.text(), "two");
        keys(&mut e, "u");
        assert_eq!(e.text(), "one\ntwo");
        // On the last line, p puts below it; . puts again.
        let mut e = editor(Some("one\ntwo\nsix"), None);
        keys(&mut e, "Gyyp");
        assert_eq!(e.text(), "one\ntwo\nsix\nsix");
        keys(&mut e, "ggddGp.");
        assert_eq!(e.text(), "two\nsix\nsix\none\none");
        // P puts above the line.
        keys(&mut e, "ggP");
        assert_eq!(e.text(), "one\ntwo\nsix\nsix\none\none");
    }

    #[test]
    fn r_replaces_a_character_and_dot_the_next() {
        let mut e = editor(Some("- [ ] one\n- [ ] two\n- [ ] six"), None);
        keys(&mut e, "lllrx");
        assert_eq!(e.text(), "- [x] one\n- [ ] two\n- [ ] six");
        keys(&mut e, "j.");
        assert_eq!(e.text(), "- [x] one\n- [x] two\n- [ ] six");
        // Any character, even one roster's keys would take: no command line,
        // no close, no pin.
        keys(&mut e, "jr:");
        assert_eq!(
            (e.command.as_deref(), e.lines[2].as_str()),
            (None, "- [:] six")
        );
        keys(&mut e, "rZrmrą");
        assert_eq!((e.pin, e.lines[2].as_str()), (None, "- [ą] six"));
        assert!(!e.state.borrow().lines.is_empty());
        // Esc after r: nothing replaced.
        keys(&mut e, "r\x1bx");
        assert_eq!(e.lines[2], "- [] six");
    }

    #[test]
    fn edits_act_where_the_cursor_shows_after_moving_to_a_shorter_line() {
        let long = "- [ ] a long line\n- [ ] x";
        let mut e = editor(Some(long), None);
        keys(&mut e, "$jCy\x1b");
        assert_eq!(e.text(), "- [ ] a long line\n- [ ] y");
        let mut e = editor(Some(long), None);
        keys(&mut e, "$jrz");
        assert_eq!(e.text(), "- [ ] a long line\n- [ ] z");
        let mut e = editor(Some(long), None);
        keys(&mut e, "$jD");
        assert_eq!(e.text(), "- [ ] a long line\n- [ ] ");
        // In insert mode, typing goes at the end of the shorter line.
        let mut e = editor(Some(long), None);
        keys(&mut e, "A");
        e.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        keys(&mut e, "ab\x1b");
        assert_eq!(e.text(), "- [ ] a long line\n- [ ] xab");
        // Moving on through a shorter line keeps the column, as in vim.
        let mut e = editor(Some("abcdef\nab\nabcdef"), None);
        keys(&mut e, "$jjx");
        assert_eq!(e.text(), "abcdef\nab\nabcde");
    }

    #[test]
    fn nothing_waits_on_a_key_across_editor() {
        // f, then Ctrl+G and back: the next x deletes, it isn't f's target.
        let mut e = editor(Some("abc x"), None);
        keys(&mut e, "f");
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(e.on_key(ctrl_g), Outcome::External);
        e.replace("abc x");
        keys(&mut e, "x");
        assert_eq!(e.text(), "bc x");
    }

    #[test]
    fn a_change_is_undone_at_once() {
        let mut e = editor(Some("one two"), None);
        keys(&mut e, "wCfour\x1bu");
        assert_eq!(e.text(), "one two");
        keys(&mut e, "ccx\x1bu");
        assert_eq!(e.text(), "one two");
    }

    #[test]
    fn r_with_a_letter_beyond_ascii_repeats_too() {
        let mut e = editor(Some("a\nb"), None);
        keys(&mut e, "rąj.");
        assert_eq!(e.text(), "ą\ną");
        keys(&mut e, "rĄ");
        assert_eq!(e.text(), "ą\nĄ");
    }

    #[test]
    fn keys_typed_quickly_leave_insert_mode() {
        let mut e = editor(None, None);
        e.escape = Some((vec!['j', 'k'], Duration::from_millis(300)));
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let typed = |e: &mut NoteEditor, c: char, ms| {
            e.on_key_at(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), at(ms));
        };
        typed(&mut e, 'i', 0);
        typed(&mut e, 'a', 0);
        // j held back (shown where it goes), then k: normal mode, no j.
        typed(&mut e, 'j', 10);
        assert_eq!((e.text().as_str(), e.held().as_str()), ("a", "j"));
        assert_eq!(e.deadline(), Some(at(310)));
        typed(&mut e, 'k', 100);
        assert_eq!((e.text().as_str(), e.mode()), ("a", EditorMode::Normal));
        // Too slow: j goes in when the time's up, then k is just a k.
        typed(&mut e, 'a', 200);
        typed(&mut e, 'j', 300);
        assert!(!e.tick(at(500)));
        assert!(e.tick(at(600)));
        assert_eq!((e.text().as_str(), e.held().as_str()), ("aj", ""));
        typed(&mut e, 'k', 700);
        assert_eq!((e.text().as_str(), e.mode()), ("ajk", EditorMode::Insert));
        // Another key after j: both go in; jj, then k: one j goes in.
        typed(&mut e, 'j', 800);
        typed(&mut e, 'x', 850);
        assert_eq!(e.text(), "ajkjx");
        typed(&mut e, 'j', 900);
        typed(&mut e, 'j', 950);
        typed(&mut e, 'k', 1000);
        assert_eq!(
            (e.text().as_str(), e.mode()),
            ("ajkjxj", EditorMode::Normal)
        );
        // Only in insert mode: j and k move in normal mode.
        typed(&mut e, 'j', 1100);
        typed(&mut e, 'k', 1150);
        assert_eq!(e.text(), "ajkjxj");
        // Enter after j: j goes in first.
        typed(&mut e, 'A', 1200);
        typed(&mut e, 'j', 1210);
        e.on_key_at(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), at(1220));
        assert_eq!(e.text(), "ajkjxjj\n");
    }

    #[test]
    fn keys_edtui_doesnt_know_do_nothing() {
        let mut e = editor(Some("one"), None);
        for code in [KeyCode::F(1), KeyCode::BackTab, KeyCode::Insert] {
            assert_eq!(
                e.on_key(KeyEvent::new(code, KeyModifiers::NONE)),
                Outcome::None
            );
        }
        assert_eq!(e.text(), "one");
    }

    #[test]
    fn searches_find_capitals() {
        let mut e = editor(Some("one\ntwo TODO"), None);
        keys(&mut e, "/TODO\n");
        let cursor = e.state.borrow().cursor;
        assert_eq!((cursor.row, cursor.col), (1, 4));
    }

    #[test]
    fn text_back_from_editor_keeps_the_pin_on_its_line() {
        let mut e = editor(Some("a\nPIN\nb"), Some(1));
        e.replace("new\na\nPIN\nb\n");
        assert_eq!(e.pin, Some(2));
        assert_eq!(e.text(), "new\na\nPIN\nb\n");
        assert!(e.dirty());
    }
}
