//! Text objects, as vim's: `iw`/`aw`, `iW`/`aW`, `ip`/`ap`, quotes (`i"`,
//! `a'`, …) and brackets (`i(`, `a{`, …), with the operator acting on them
//! (`d`, `c`, `y`, and in visual mode selecting them). Added by roster.

use jagged::index::RowIndex;

use super::word::kind;
use super::Execute;
use crate::{
    clipboard::ClipboardTrait,
    helper::{copy_chars, cut_chars},
    state::selection::Selection,
    EditorMode, EditorState, Index2, Lines,
};

/// What a text object is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextObject {
    /// A word (`w`: letters, digits and `_`, or other marks, apart), or a
    /// WORD (`W`: anything but whitespace).
    Word { big: bool },
    /// Lines up to a blank one (`p`), whole lines.
    Paragraph,
    /// Between a pair of this quote on the cursor's line.
    Quote(char),
    /// Between these brackets, the ones in them skipped, across lines.
    Bracket(char, char),
}

impl TextObject {
    fn is_word(self) -> bool {
        matches!(self, TextObject::Word { .. })
    }
}

/// What's done with a text object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Delete,
    /// Deleted, then insert mode.
    Change,
    Yank,
    /// Selected, in visual mode.
    Select,
}

/// An operator on a text object around the cursor: `daw`, `ci(`, `yi"`,
/// `vap`. With `around`, the object's surroundings too: the space after a
/// word (else before it), the blank lines after a paragraph, the quotes or
/// brackets.
#[derive(Clone, Copy, Debug)]
pub struct OnObject {
    pub operator: Operator,
    pub object: TextObject,
    pub around: bool,
    /// How many: words and paragraphs on (`d2aw`), brackets out (`2i(`
    /// the pair around the one around), quotes with the quotes (`2i"`).
    /// None if there aren't as many.
    pub count: usize,
}

impl Execute for OnObject {
    fn execute(&mut self, state: &mut EditorState) {
        let Some(span) = span(
            &state.lines,
            state.cursor,
            self.object,
            self.around,
            self.count,
        ) else {
            // A word on an empty line is an empty one: a change goes in
            // there, as vim's. Else there's no such object: nothing done.
            let empty = row(&state.lines, state.cursor.row).is_empty();
            if self.operator == Operator::Change && self.object.is_word() && empty {
                state.capture();
                state.mode = EditorMode::Insert;
            } else {
                state.failed = true;
            }
            return;
        };
        match self.operator {
            Operator::Delete => {
                delete(state, whole_lines(&state.lines, span));
            }
            Operator::Change => {
                if span.is_empty() {
                    state.capture();
                    state.cursor = span.start;
                } else if span.lines {
                    // As vim leaves it: an empty line where they were, the
                    // one left if they were all.
                    let all = span.start.row == 0 && span.end.row + 1 >= state.lines.len();
                    delete(state, span);
                    let row = span.start.row.min(state.lines.len());
                    if !all {
                        state.lines.insert(RowIndex::new(row), vec![]);
                    }
                    state.cursor = Index2::new(row.min(state.lines.len() - 1), 0);
                } else {
                    delete(state, span);
                    state.cursor = span.start;
                }
                state.mode = EditorMode::Insert;
            }
            Operator::Yank => {
                if !span.is_empty() {
                    state.clip.set_text(text(&state.lines, span));
                    state.cursor = span.start;
                }
            }
            Operator::Select => {
                if !span.is_empty() {
                    let selection = Selection::new(span.start, span.end);
                    state.selection = Some(if span.lines {
                        selection.line_mode()
                    } else {
                        selection
                    });
                    state.cursor = span.end;
                    state.mode = EditorMode::Visual;
                }
            }
        }
    }

    fn is_repeatable(&self) -> bool {
        matches!(self.operator, Operator::Delete | Operator::Change)
    }

    fn count(&mut self, n: usize) -> bool {
        self.count = n;
        true
    }
}

/// What a text object covers: from `start` to `end`, both in (`end` before
/// `start` where it's empty, as `i()`'s), whole lines with `lines`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: Index2,
    pub end: Index2,
    pub lines: bool,
}

impl Span {
    fn chars(start: Index2, end: Index2) -> Span {
        Span {
            start,
            end,
            lines: false,
        }
    }

    fn rows(lines: &Lines, start: usize, end: usize) -> Span {
        Span {
            start: Index2::new(start, 0),
            end: Index2::new(end, lines.len_col(end).unwrap_or(0).saturating_sub(1)),
            lines: true,
        }
    }

    /// Where its characters end, the last left out.
    fn past(&self) -> Index2 {
        Index2::new(self.end.row, self.end.col + 1)
    }

    pub fn is_empty(&self) -> bool {
        !self.lines && (self.end.row, self.end.col) < (self.start.row, self.start.col)
    }
}

/// What `object` covers around `cursor`, if anything.
#[must_use]
pub fn span(
    lines: &Lines,
    cursor: Index2,
    object: TextObject,
    around: bool,
    count: usize,
) -> Option<Span> {
    match object {
        TextObject::Word { big } => word(lines, cursor, big, around, count),
        TextObject::Paragraph => paragraph(lines, cursor, around, count),
        TextObject::Quote(quote) => quoted(lines, cursor, quote, around, count),
        TextObject::Bracket(open, close) => bracketed(lines, cursor, (open, close), around, count),
    }
}

/// `span`, or the lines it's on if it's characters over lines, from their
/// first's first non-blank or before to their last's end: as vim deletes
/// them (`daB` of a block on lines of its own).
fn whole_lines(lines: &Lines, span: Span) -> Span {
    let first = row(lines, span.start.row);
    let last = row(lines, span.end.row);
    let indent = first.iter().take_while(|c| c.is_whitespace()).count();
    let after = last.get(span.end.col + 1..).unwrap_or_default();
    if !span.lines
        && span.end.row > span.start.row
        && span.start.col <= indent
        && after.iter().all(|c| c.is_whitespace())
    {
        return Span::rows(lines, span.start.row, span.end.row);
    }
    span
}

/// Deletes what `span` covers into the clipboard.
fn delete(state: &mut EditorState, span: Span) {
    if span.is_empty() {
        return;
    }
    state.capture();
    state.clip.set_text(text(&state.lines, span));
    if span.lines {
        for _ in span.start.row..=span.end.row {
            if span.start.row < state.lines.len() {
                state.lines.remove(RowIndex::new(span.start.row));
            }
        }
        // As vim leaves it: a line at least.
        if state.lines.is_empty() {
            state.lines.push(vec![]);
        }
        let row = span.start.row.min(state.lines.len() - 1);
        state.cursor = Index2::new(row, first_non_blank(&state.lines, row));
    } else {
        // The characters only: the line break after the last stays.
        let _ = cut_chars(&mut state.lines, span.start, span.past());
        state.cursor = span.start;
        state.clamp_column();
    }
}

/// What `span` covers, as the clipboard has it: whole lines after a line
/// break.
fn text(lines: &Lines, span: Span) -> String {
    if span.lines {
        let rows: Vec<String> = (span.start.row..=span.end.row)
            .map(|row| row_text(lines, row))
            .collect();
        format!("\n{}", rows.join("\n"))
    } else {
        copy_chars(lines, span.start, span.past())
    }
}

pub(super) fn row(lines: &Lines, row: usize) -> &[char] {
    lines.get(RowIndex::new(row)).map_or(&[], Vec::as_slice)
}

fn row_text(lines: &Lines, at: usize) -> String {
    row(lines, at).iter().collect()
}

pub(super) fn first_non_blank(lines: &Lines, at: usize) -> usize {
    let chars = row(lines, at);
    chars
        .iter()
        .position(|c| !c.is_whitespace())
        .unwrap_or_else(|| chars.len().saturating_sub(1))
}

/// Runs of one kind among `len` things, by `kind`: a line's words and the
/// whitespace between, or the paragraphs and the blank lines between; the
/// first and the last of each, and its kind.
fn runs<K: Copy + PartialEq>(len: usize, kind: impl Fn(usize) -> K) -> Vec<(usize, usize, K)> {
    let mut runs: Vec<(usize, usize, K)> = Vec::new();
    for i in 0..len {
        let k = kind(i);
        match runs.last_mut() {
            Some(last) if last.2 == k => last.1 = i,
            _ => runs.push((i, i, k)),
        }
    }
    runs
}

/// The first and last of what `count` objects from run `at` take, if there
/// are as many: inner, runs of either kind (`2iw` a word and the space
/// after); around, runs not `blank`, with the blank run after them, else the
/// one before. On a blank run around, it and the next (or with nothing
/// after, alone, and with `before` the one before too, as a word's).
fn take<K: Copy + PartialEq>(
    runs: &[(usize, usize, K)],
    at: usize,
    blank: K,
    around: bool,
    count: usize,
    before: bool,
) -> Option<(usize, usize)> {
    let count = count.max(1);
    if !around {
        let last = runs.get(at + count - 1)?;
        return Some((runs[at].0, last.1));
    }
    let mut taken = 0;
    let mut last = at;
    loop {
        if runs[last].2 != blank {
            taken += 1;
            if taken == count {
                break;
            }
        }
        last += 1;
        if last == runs.len() {
            if count == 1 && runs[at].2 == blank {
                let first = if before && at > 0 { at - 1 } else { at };
                return Some((runs[first].0, runs[at].1));
            }
            return None;
        }
    }
    let mut first = at;
    if runs[at].2 != blank {
        if last + 1 < runs.len() && runs[last + 1].2 == blank {
            last += 1;
        } else if first > 0 && runs[first - 1].2 == blank {
            first -= 1;
        }
    }
    Some((runs[first].0, runs[last].1))
}

fn word(lines: &Lines, cursor: Index2, big: bool, around: bool, count: usize) -> Option<Span> {
    let chars = row(lines, cursor.row);
    if chars.is_empty() {
        return None;
    }
    let col = cursor.col.min(chars.len() - 1);
    let runs = runs(chars.len(), |i| kind(chars[i], big));
    let at = runs.iter().position(|run| run.1 >= col)?;
    let (start, end) = take(&runs, at, 0, around, count, true)?;
    let at = |col| Index2::new(cursor.row, col);
    Some(Span::chars(at(start), at(end)))
}

fn paragraph(lines: &Lines, cursor: Index2, around: bool, count: usize) -> Option<Span> {
    let len = lines.len();
    if len == 0 {
        return None;
    }
    let blank = |r: usize| row(lines, r).iter().all(|c| c.is_whitespace());
    let runs = runs(len, blank);
    let at = runs
        .iter()
        .position(|run| run.1 >= cursor.row.min(len - 1))?;
    let (start, end) = take(&runs, at, true, around, count, false)?;
    Some(Span::rows(lines, start, end))
}

fn quoted(lines: &Lines, cursor: Index2, quote: char, around: bool, count: usize) -> Option<Span> {
    let chars = row(lines, cursor.row);
    let quotes: Vec<usize> = (0..chars.len()).filter(|&i| chars[i] == quote).collect();
    // The pair the cursor is in or on, else the first after it.
    let (open, close) = quotes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[open, close]| (open, close))
        .find(|&(_, close)| close >= cursor.col)?;
    let at = |col| Index2::new(cursor.row, col);
    if !around && count > 1 {
        // The quotes too, without the space: vim's `2i"`.
        return Some(Span::chars(at(open), at(close)));
    }
    if !around {
        // Empty (`""`): nothing, but where a change goes.
        let end = if close == open + 1 { open } else { close - 1 };
        return Some(Span::chars(at(open + 1), at(end)));
    }
    // With the space after it, else the space before.
    let (mut start, mut end) = (open, close);
    let space = |i: usize| chars[i].is_whitespace();
    if end + 1 < chars.len() && space(end + 1) {
        while end + 1 < chars.len() && space(end + 1) {
            end += 1;
        }
    } else {
        while start > 0 && space(start - 1) {
            start -= 1;
        }
    }
    Some(Span::chars(at(start), at(end)))
}

fn bracketed(
    lines: &Lines,
    cursor: Index2,
    (open, close): (char, char),
    around: bool,
    count: usize,
) -> Option<Span> {
    // The pair the cursor is in (on, if it's on one), and as many more out
    // as counted (`2i(`).
    let (mut opened, mut closed) = enclosing(lines, cursor, (open, close), true)?;
    for _ in 1..count {
        (opened, closed) = enclosing(lines, before(lines, opened)?, (open, close), false)?;
    }
    if around {
        return Some(Span::chars(opened, closed));
    }
    // A block, its brackets ending and starting lines of their own: the
    // lines between, whole.
    let ends_line = opened.col + 1 >= row(lines, opened.row).len();
    let starts_line = row(lines, closed.row)[..closed.col]
        .iter()
        .all(|c| c.is_whitespace());
    if ends_line && starts_line && closed.row > opened.row {
        if closed.row == opened.row + 1 {
            return Some(Span::chars(
                Index2::new(closed.row, 0),
                Index2::new(opened.row, opened.col),
            ));
        }
        return Some(Span::rows(lines, opened.row + 1, closed.row - 1));
    }
    let start = after(lines, opened)?;
    if start == closed {
        return Some(Span::chars(start, opened));
    }
    let end = before(lines, closed)?;
    Some(Span::chars(start, end))
}

/// The brackets around `from`: backwards to the opening one, those closed
/// after it skipped (with `on`, the one `from` is on), and forwards to the
/// one closing it.
fn enclosing(
    lines: &Lines,
    from: Index2,
    (open, close): (char, char),
    on: bool,
) -> Option<(Index2, Index2)> {
    let get = |at: Index2| row(lines, at.row).get(at.col).copied();
    let mut depth = 0;
    let mut at = Index2::new(from.row, from.col.min(row(lines, from.row).len()));
    let opened = loop {
        match get(at) {
            Some(c) if c == close && !(on && at == from) => depth += 1,
            Some(c) if c == open && depth == 0 => break at,
            Some(c) if c == open => depth -= 1,
            _ => {}
        }
        at = before(lines, at)?;
    };
    let mut depth = 0;
    let mut at = opened;
    let closed = loop {
        at = after(lines, at)?;
        match get(at) {
            Some(c) if c == open => depth += 1,
            Some(c) if c == close && depth == 0 => break at,
            Some(c) if c == close => depth -= 1,
            _ => {}
        }
    };
    Some((opened, closed))
}

/// The position after `at`, the next line's first after a line's last (an
/// empty line's only one being its start).
fn after(lines: &Lines, at: Index2) -> Option<Index2> {
    if at.col + 1 < row(lines, at.row).len() {
        return Some(Index2::new(at.row, at.col + 1));
    }
    (at.row + 1 < lines.len()).then(|| Index2::new(at.row + 1, 0))
}

/// The position before `at`, the line before's last before a line's first.
fn before(lines: &Lines, at: Index2) -> Option<Index2> {
    if at.col > 0 {
        return Some(Index2::new(at.row, at.col - 1));
    }
    let row_before = at.row.checked_sub(1)?;
    Some(Index2::new(
        row_before,
        row(lines, row_before).len().saturating_sub(1),
    ))
}

/// The text objects, by the key naming them after `i` or `a`.
const OBJECTS: &[(char, TextObject)] = &[
    ('w', TextObject::Word { big: false }),
    ('W', TextObject::Word { big: true }),
    ('p', TextObject::Paragraph),
    ('"', TextObject::Quote('"')),
    ('\'', TextObject::Quote('\'')),
    ('`', TextObject::Quote('`')),
    ('(', TextObject::Bracket('(', ')')),
    (')', TextObject::Bracket('(', ')')),
    ('b', TextObject::Bracket('(', ')')),
    ('[', TextObject::Bracket('[', ']')),
    (']', TextObject::Bracket('[', ']')),
    ('{', TextObject::Bracket('{', '}')),
    ('}', TextObject::Bracket('{', '}')),
    ('B', TextObject::Bracket('{', '}')),
    ('<', TextObject::Bracket('<', '>')),
    ('>', TextObject::Bracket('<', '>')),
];

/// The text object a key names after an operator's `i` or `a` (`w` of
/// `daw`, `(` of `ci(`): the key handler asks.
#[must_use]
pub fn named(key: char) -> Option<TextObject> {
    OBJECTS
        .iter()
        .find(|&&(k, _)| k == key)
        .map(|&(_, object)| object)
}

/// The keys of the text objects in visual mode (`aw`, `i(`, …), selecting
/// them. After an operator they're the key handler's (`daw`, `ci(`).
#[must_use]
pub fn keys() -> Vec<(Vec<char>, EditorMode, super::Action)> {
    let mut keys = Vec::new();
    for &(key, object) in OBJECTS {
        for (side, around) in [('i', false), ('a', true)] {
            let select = OnObject {
                operator: Operator::Select,
                object,
                around,
                count: 1,
            };
            keys.push((vec![side, key], EditorMode::Visual, select.into()));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::InternalClipboard;

    fn state(text: &str, row: usize, col: usize) -> EditorState {
        let mut state = EditorState::new(Lines::from(text));
        state.set_clipboard(InternalClipboard::default());
        state.cursor = Index2::new(row, col);
        state
    }

    fn on(state: &mut EditorState, operator: Operator, object: TextObject, around: bool) {
        counted(state, operator, object, around, 1);
    }

    fn counted(
        state: &mut EditorState,
        operator: Operator,
        object: TextObject,
        around: bool,
        count: usize,
    ) {
        OnObject {
            operator,
            object,
            around,
            count,
        }
        .execute(state);
    }

    const WORD: TextObject = TextObject::Word { big: false };

    #[test]
    fn a_word_takes_its_space_after_else_before() {
        let mut s = state("one two three", 0, 5);
        on(&mut s, Operator::Delete, WORD, true);
        assert_eq!(s.lines.to_string(), "one three");
        assert_eq!(s.clip.get_text(), "two ");
        // The last word: the space before it.
        let mut s = state("one two", 0, 5);
        on(&mut s, Operator::Delete, WORD, true);
        assert_eq!(s.lines.to_string(), "one");
        // Inner: the word alone; a WORD takes its marks.
        let mut s = state("a foo.bar b", 0, 3);
        on(&mut s, Operator::Delete, WORD, false);
        assert_eq!(s.lines.to_string(), "a .bar b");
        let mut s = state("a foo.bar b", 0, 3);
        on(
            &mut s,
            Operator::Delete,
            TextObject::Word { big: true },
            true,
        );
        assert_eq!(s.lines.to_string(), "a b");
        // Letters beyond ASCII are a word's.
        let mut s = state("zażółć gęślą", 0, 2);
        on(&mut s, Operator::Delete, WORD, true);
        assert_eq!(s.lines.to_string(), "gęślą");
    }

    #[test]
    fn a_change_of_lines_leaves_one_empty_line() {
        // Every line (a note of one paragraph): the one line left, empty.
        for (text, around) in [
            ("foo bar baz", false),
            ("foo bar baz", true),
            ("x", false),
            ("a\nb\nc", false),
            ("\n", false),
        ] {
            let mut s = state(text, 0, 0);
            on(&mut s, Operator::Change, TextObject::Paragraph, around);
            let after = (s.lines.len(), s.lines.to_string(), s.cursor, s.mode);
            let empty = (1, String::new(), Index2::new(0, 0), EditorMode::Insert);
            assert_eq!(after, empty, "{text:?}");
        }
        // Some: an empty line where they were.
        let mut s = state("a\n\nb", 2, 0);
        on(&mut s, Operator::Change, TextObject::Paragraph, false);
        assert_eq!(
            (s.lines.to_string(), s.cursor),
            ("a\n\n".into(), Index2::new(2, 0))
        );
    }

    #[test]
    fn a_whole_lines_characters_leave_its_line_break() {
        // diw, daw, ciw of a line's only word: the line, empty, stays.
        for (operator, around) in [
            (Operator::Delete, false),
            (Operator::Delete, true),
            (Operator::Change, false),
        ] {
            let mut s = state("foo\nbar", 0, 1);
            on(&mut s, operator, WORD, around);
            assert_eq!(
                (s.lines.to_string(), s.cursor),
                ("\nbar".into(), Index2::new(0, 0))
            );
        }
        // Quotes and brackets around; the last line too.
        let mut s = state("\"foo\"\nbar", 0, 1);
        on(&mut s, Operator::Delete, TextObject::Quote('"'), true);
        assert_eq!(s.lines.to_string(), "\nbar");
        let mut s = state("(foo)\nbar", 0, 1);
        on(
            &mut s,
            Operator::Delete,
            TextObject::Bracket('(', ')'),
            true,
        );
        assert_eq!(s.lines.to_string(), "\nbar");
        let mut s = state("a\nfoo", 1, 1);
        on(&mut s, Operator::Delete, WORD, false);
        assert_eq!(
            (s.lines.to_string(), s.cursor),
            ("a\n".into(), Index2::new(1, 0))
        );
        // A yank copies the word alone.
        let mut s = state("foo\nbar", 0, 1);
        on(&mut s, Operator::Yank, WORD, false);
        assert_eq!(s.clip.get_text(), "foo");
        // Every line deleted: one left, as vim has it.
        let mut s = state("one\ntwo", 0, 0);
        on(&mut s, Operator::Delete, TextObject::Paragraph, true);
        assert_eq!((s.lines.len(), s.lines.to_string()), (1, String::new()));
    }

    #[test]
    fn a_block_on_lines_of_its_own_is_deleted_as_lines() {
        let brace = TextObject::Bracket('{', '}');
        let mut s = state("a\n{\n    b;\n}\nc", 2, 4);
        on(&mut s, Operator::Delete, brace, true);
        assert_eq!(s.lines.to_string(), "a\nc");
        assert_eq!(s.clip.get_text(), "\n{\n    b;\n}");
        // Not from its line's start: the characters, as they are.
        let mut s = state("if x {\n    b;\n}\nc", 1, 4);
        on(&mut s, Operator::Delete, brace, true);
        assert_eq!(s.lines.to_string(), "if x \nc");
        // A change takes the characters.
        let mut s = state("a\n{\n    b;\n}\nc", 2, 4);
        on(&mut s, Operator::Change, brace, true);
        assert_eq!(s.lines.to_string(), "a\n\nc");
    }

    #[test]
    fn a_word_on_an_empty_line_is_changed_and_none_is_no_change() {
        // ciw, caw: insert mode, nothing deleted; diw: nothing.
        for around in [false, true] {
            let mut s = state("a\n\nb", 1, 0);
            on(&mut s, Operator::Change, WORD, around);
            assert_eq!(
                (s.mode, s.lines.to_string()),
                (EditorMode::Insert, "a\n\nb".into())
            );
        }
        let mut s = state("a\n\nb", 1, 0);
        on(&mut s, Operator::Delete, WORD, false);
        assert_eq!(
            (s.mode, s.lines.to_string()),
            (EditorMode::Normal, "a\n\nb".into())
        );
        // No object there: nothing done, and . repeats what came before.
        let mut s = state("abc", 0, 0);
        s.execute_recorded(crate::actions::RemoveChar(1).into());
        let none = OnObject {
            operator: Operator::Delete,
            object: TextObject::Bracket('(', ')'),
            around: false,
            count: 1,
        };
        s.execute_recorded(none.into());
        s.execute_recorded(crate::actions::RepeatLastChange.into());
        assert_eq!(s.lines.to_string(), "c");
    }

    #[test]
    fn a_count_widens_an_object_and_fails_past_what_there_is() {
        // Words on: 2aw two words and the space after; 3iw a word, a space,
        // a word.
        let mut s = state("foo bar baz qux", 0, 0);
        counted(&mut s, Operator::Yank, WORD, true, 2);
        assert_eq!(s.clip.get_text(), "foo bar ");
        counted(&mut s, Operator::Yank, WORD, false, 3);
        assert_eq!(s.clip.get_text(), "foo bar");
        counted(&mut s, Operator::Delete, WORD, true, 2);
        assert_eq!(s.lines.to_string(), "baz qux");
        // More than there are: nothing.
        counted(&mut s, Operator::Delete, WORD, true, 3);
        assert_eq!((s.lines.to_string(), s.failed), ("baz qux".into(), true));
        // The last words: the space before them.
        let mut s = state("a b c", 0, 2);
        counted(&mut s, Operator::Delete, WORD, true, 2);
        assert_eq!(s.lines.to_string(), "a");
        // Brackets out, as many as there are; quotes with the quotes.
        let paren = TextObject::Bracket('(', ')');
        let mut s = state("f(a, g(b), c)", 0, 7);
        counted(&mut s, Operator::Delete, paren, false, 2);
        assert_eq!(s.lines.to_string(), "f()");
        let mut s = state("(a)(b)", 0, 4);
        counted(&mut s, Operator::Delete, paren, false, 2);
        assert_eq!(s.lines.to_string(), "(a)(b)");
        let mut s = state(r#"x "a b" y"#, 0, 3);
        counted(&mut s, Operator::Delete, TextObject::Quote('"'), false, 2);
        assert_eq!(s.lines.to_string(), "x  y");
        // Paragraphs on, with the blank lines after.
        let mut s = state("a\n\nb\n\nc", 0, 0);
        counted(&mut s, Operator::Delete, TextObject::Paragraph, true, 2);
        assert_eq!(s.lines.to_string(), "c");
    }

    #[test]
    fn a_change_deletes_and_inserts_and_a_yank_copies() {
        let mut s = state("say hi there", 0, 4);
        on(&mut s, Operator::Change, WORD, true);
        assert_eq!(
            (s.lines.to_string(), s.mode, s.cursor),
            ("say there".into(), EditorMode::Insert, Index2::new(0, 4))
        );
        let mut s = state("say hi there", 0, 5);
        on(&mut s, Operator::Yank, WORD, false);
        assert_eq!((s.clip.get_text(), s.cursor.col), ("hi".into(), 4));
        assert_eq!(s.lines.to_string(), "say hi there");
    }

    #[test]
    fn quotes_on_the_line_the_cursor_is_in_or_before() {
        let mut s = state(r#"x = "a b" + 'c'"#, 0, 6);
        on(&mut s, Operator::Delete, TextObject::Quote('"'), false);
        assert_eq!(s.lines.to_string(), r#"x = "" + 'c'"#);
        // Before the pair: the first after the cursor; around takes the space.
        let mut s = state(r#"x = "a b" + 'c'"#, 0, 0);
        on(&mut s, Operator::Delete, TextObject::Quote('"'), true);
        assert_eq!(s.lines.to_string(), "x = + 'c'");
        // Empty: change goes in between.
        let mut s = state(r#"say "" now"#, 0, 1);
        on(&mut s, Operator::Change, TextObject::Quote('"'), false);
        assert_eq!((s.mode, s.cursor.col), (EditorMode::Insert, 5));
    }

    #[test]
    fn brackets_nest_and_span_lines() {
        let paren = TextObject::Bracket('(', ')');
        let mut s = state("f(a, g(b), c)", 0, 3);
        on(&mut s, Operator::Delete, paren, false);
        assert_eq!(s.lines.to_string(), "f()");
        let mut s = state("f(a, g(b), c)", 0, 7);
        on(&mut s, Operator::Delete, paren, true);
        assert_eq!(s.lines.to_string(), "f(a, g, c)");
        // On a bracket: the pair it's of.
        let mut s = state("f(a, g(b), c)", 0, 12);
        on(&mut s, Operator::Delete, paren, false);
        assert_eq!(s.lines.to_string(), "f()");
        // A block: the lines between, whole; changed, an empty line left.
        let brace = TextObject::Bracket('{', '}');
        let mut s = state("if x {\n    a;\n    b;\n}", 1, 2);
        on(&mut s, Operator::Delete, brace, false);
        assert_eq!(s.lines.to_string(), "if x {\n}");
        assert_eq!(s.clip.get_text(), "\n    a;\n    b;");
        let mut s = state("if x {\n    a;\n}", 1, 2);
        on(&mut s, Operator::Change, brace, false);
        assert_eq!(s.lines.to_string(), "if x {\n\n}");
        assert_eq!((s.cursor, s.mode), (Index2::new(1, 0), EditorMode::Insert));
        // None about the cursor: nothing.
        let mut s = state("no brackets", 0, 3);
        on(&mut s, Operator::Delete, paren, false);
        assert_eq!(s.lines.to_string(), "no brackets");
    }

    #[test]
    fn a_paragraph_is_whole_lines_and_its_blank_ones_after() {
        let text = "one\ntwo\n\nthree\n\n";
        let mut s = state(text, 1, 0);
        on(&mut s, Operator::Delete, TextObject::Paragraph, false);
        assert_eq!(s.lines.to_string(), "\nthree\n\n");
        assert_eq!(s.clip.get_text(), "\none\ntwo");
        let mut s = state(text, 0, 0);
        on(&mut s, Operator::Delete, TextObject::Paragraph, true);
        assert_eq!(s.lines.to_string(), "three\n\n");
        // Selected: whole lines, in visual mode.
        let mut s = state(text, 3, 2);
        on(&mut s, Operator::Select, TextObject::Paragraph, false);
        let selection = s.selection.unwrap();
        assert!(selection.line_mode && s.mode == EditorMode::Visual);
        assert_eq!((selection.start.row, selection.end.row), (3, 3));
    }
}
