//! Operators on what the motion after them moves over, as vim's: `d`, `c`
//! and `y` with any motion (`dw`, `c$`, `y2j`, `dfx`, `dG`), and on lines
//! with the operator's key again (`dd`, `3cc`, `yy`). Added by roster.

use jagged::index::RowIndex;

use super::motion::{MoveBigWordForward, MoveWordForward};
use super::object::{first_non_blank, row, Operator};
use super::{recounted, word, Action, Execute};
use crate::{
    clipboard::ClipboardTrait,
    helper::{copy_chars, cut_chars},
    state::selection::Selection,
    EditorMode, EditorState, Index2, Lines,
};

/// How an operator takes what a motion moves over: up to where it goes,
/// with the character there too, or the lines from one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionKind {
    /// `w`, `b`, `h`, `l`, `0`, `^`, `F`, `T`, `{` and `}`.
    Exclusive,
    /// `e`, `$`, `f`, `t` and `%`.
    Inclusive,
    /// `j`, `k`, `G`, `gg` and `_`.
    Linewise,
}

/// An operator on what a motion moves over from the cursor (`dw`), the
/// motion to come while it's `None`: the key handler gives it the next
/// one. A count is the motion's (`3dw` is `d3w`).
#[derive(Clone, Debug)]
pub struct Operate {
    pub operator: Operator,
    pub motion: Option<Box<Action>>,
}

impl Operate {
    /// The operator, its motion to come.
    #[must_use]
    pub fn new(operator: Operator) -> Self {
        Self {
            operator,
            motion: None,
        }
    }

    /// The operator on `motion` (`D` is `d$`).
    #[must_use]
    pub fn on(operator: Operator, motion: impl Into<Action>) -> Self {
        Self {
            operator,
            motion: Some(Box::new(motion.into())),
        }
    }
}

impl Execute for Operate {
    fn execute(&mut self, state: &mut EditorState) {
        let Some(motion) = self.motion.as_deref_mut() else {
            return;
        };
        let Some(mut kind) = motion.motion() else {
            return;
        };
        state.clamp_column();
        let from = state.cursor;
        // `cw` on a word is `ce`, as vim has it, but stopping at the end of
        // the word it's on.
        let moved = match words(motion) {
            Some((n, big))
                if self.operator == Operator::Change && !word::blank(&state.lines, from) =>
            {
                kind = MotionKind::Inclusive;
                state.cursor = word::end(&state.lines, from, n, big, true);
                true
            }
            _ => motion.move_after_operator(state),
        };
        let to = state.cursor;
        state.cursor = from;
        if !moved {
            // Nothing done, nor for `.` to repeat (`dfx` with no `x`).
            state.failed = true;
            return;
        }
        if state.lines.is_empty() {
            // An empty note, as an empty line: a change goes in there.
            if self.operator == Operator::Change {
                state.capture();
                state.mode = EditorMode::Insert;
            }
            return;
        }
        // Where a yank leaves the cursor, as vim's: on the characters'
        // start, or on the lines' first if it went up to it (`yk`), else
        // where it was (`yj`, `yy`).
        let first = match kind {
            MotionKind::Linewise if to.row < from.row => to,
            MotionKind::Linewise => from,
            _ if (to.row, to.col) < (from.row, from.col) => to,
            _ => from,
        };
        let mut text = Text::of(&state.lines, from, to, kind);
        if self.operator == Operator::Delete {
            text = text.whole_lines(&state.lines);
        }
        apply(state, self.operator, text, first);
    }

    fn is_repeatable(&self) -> bool {
        matches!(self.operator, Operator::Delete | Operator::Change)
    }

    /// A count is its motion's (`2.` after `d3w` is `d2w`).
    fn count(&mut self, n: usize) -> bool {
        if let Some(motion) = self.motion.take() {
            self.motion = Some(Box::new(recounted(*motion, n)));
        }
        true
    }
}

/// `w`'s or `W`'s count, and whether it's `W`.
fn words(motion: &Action) -> Option<(usize, bool)> {
    match motion {
        Action::MoveWordForward(MoveWordForward(n)) => Some((*n, false)),
        Action::MoveBigWordForward(MoveBigWordForward(n)) => Some((*n, true)),
        _ => None,
    }
}

/// What an operator acts on: the characters from one place up to another,
/// it left out (a line's end being after its last character, up to its line
/// break), or lines, the first and the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Text {
    Chars(Index2, Index2),
    Lines(usize, usize),
}

impl Text {
    /// What a motion from `from` to `to` moves over, taken as `kind` says.
    /// As vim's (`:help exclusive`), an exclusive motion ending at a later
    /// line's start ends at the end of the line before; it's whole lines if
    /// it began at its line's first non-blank or before (`d}` at a
    /// paragraph's start).
    fn of(lines: &Lines, from: Index2, to: Index2, kind: MotionKind) -> Text {
        let (start, end) = if (to.row, to.col) < (from.row, from.col) {
            (to, from)
        } else {
            (from, to)
        };
        match kind {
            MotionKind::Linewise => Text::Lines(start.row, end.row),
            MotionKind::Inclusive => {
                let len = row(lines, end.row).len();
                Text::Chars(start, Index2::new(end.row, (end.col + 1).min(len)))
            }
            MotionKind::Exclusive if end.col == 0 && end.row > start.row => {
                let above = end.row - 1;
                if start.col <= indent(lines, start.row) {
                    Text::Lines(start.row, above)
                } else {
                    Text::Chars(start, Index2::new(above, row(lines, above).len()))
                }
            }
            MotionKind::Exclusive => Text::Chars(start, end),
        }
    }

    /// Characters over lines, from the first's indent or before to the
    /// last's end, are those lines when deleted, as vim deletes them (`d}`
    /// to the end, `de` from a blank line).
    fn whole_lines(self, lines: &Lines) -> Text {
        match self {
            Text::Chars(start, end)
                if end.row > start.row
                    && start.col <= indent(lines, start.row)
                    && row(lines, end.row)
                        .get(end.col..)
                        .unwrap_or_default()
                        .iter()
                        .all(|c| c.is_whitespace()) =>
            {
                Text::Lines(start.row, end.row)
            }
            text => text,
        }
    }
}

/// How many whitespace characters a line starts with.
fn indent(lines: &Lines, at: usize) -> usize {
    row(lines, at)
        .iter()
        .take_while(|c| c.is_whitespace())
        .count()
}

/// The operator on `text`, `first` where a yank leaves the cursor.
fn apply(state: &mut EditorState, operator: Operator, text: Text, first: Index2) {
    match (operator, text) {
        (Operator::Delete, Text::Chars(start, end)) => {
            if start == end {
                return;
            }
            state.capture();
            let cut = cut_chars(&mut state.lines, start, end);
            state.clip.set_text(cut);
            state.cursor = start;
            state.clamp_column();
        }
        (Operator::Delete, Text::Lines(top, bottom)) => {
            state.capture();
            let cut = cut_lines(&mut state.lines, top, bottom);
            state.clip.set_text(cut);
            // As vim leaves it: a line at least.
            if state.lines.is_empty() {
                state.lines.push(vec![]);
            }
            let at = top.min(state.lines.len() - 1);
            state.cursor = Index2::new(at, first_non_blank(&state.lines, at));
        }
        (Operator::Change, Text::Chars(start, end)) => {
            state.capture();
            if start != end {
                let cut = cut_chars(&mut state.lines, start, end);
                state.clip.set_text(cut);
            }
            state.cursor = start;
            state.mode = EditorMode::Insert;
        }
        (Operator::Change, Text::Lines(top, bottom)) => {
            state.capture();
            let cut = cut_lines(&mut state.lines, top, bottom);
            state.clip.set_text(cut);
            // As vim leaves it: an empty line where they were.
            let at = top.min(state.lines.len());
            state.lines.insert(RowIndex::new(at), vec![]);
            state.cursor = Index2::new(at, 0);
            state.mode = EditorMode::Insert;
        }
        (Operator::Yank, text) => {
            let copied = match text {
                Text::Chars(start, end) if start == end => return,
                Text::Chars(start, end) => copy_chars(&state.lines, start, end),
                Text::Lines(top, bottom) => copy_lines(&state.lines, top, bottom),
            };
            state.clip.set_text(copied);
            state.cursor = first;
            state.clamp_column();
        }
        (Operator::Select, text) => {
            let selection = match text {
                Text::Chars(start, end) if start == end => return,
                Text::Chars(start, end) => {
                    Selection::new(start, Index2::new(end.row, end.col.saturating_sub(1)))
                }
                Text::Lines(top, bottom) => {
                    let end = row(&state.lines, bottom).len().saturating_sub(1);
                    Selection::new(Index2::new(top, 0), Index2::new(bottom, end)).line_mode()
                }
            };
            state.cursor = selection.end;
            state.selection = Some(selection);
            state.mode = EditorMode::Visual;
        }
    }
}

/// Lines `top` to `bottom`, as the clipboard has lines: after a line break.
fn copy_lines(lines: &Lines, top: usize, bottom: usize) -> String {
    let rows: Vec<String> = (top..=bottom)
        .map(|at| row(lines, at).iter().collect())
        .collect();
    format!("\n{}", rows.join("\n"))
}

fn cut_lines(lines: &mut Lines, top: usize, bottom: usize) -> String {
    let bottom = bottom.min(lines.len().saturating_sub(1));
    let text = copy_lines(lines, top, bottom);
    for _ in top..=bottom {
        lines.remove(RowIndex::new(top));
    }
    text
}

#[cfg(test)]
mod tests {
    use crate::clipboard::{ClipboardTrait, InternalClipboard};
    use crate::events::{KeyEventHandler, KeyInput};
    use crate::{EditorMode, EditorState, Index2, Lines};
    use crossterm::event::KeyCode;

    /// The text, what was yanked and where the cursor is after `keys` (Esc
    /// as `\x1b`, Right as `>`) from `at` in `text`, back in normal mode.
    fn typed(text: &str, at: (usize, usize), keys: &str) -> (String, String, (usize, usize)) {
        let mut state = EditorState::new(Lines::from(text));
        state.set_clipboard(InternalClipboard::default());
        state.cursor = Index2::new(at.0, at.1);
        let mut handler = KeyEventHandler::default();
        for c in keys.chars() {
            let key = match c {
                '\x1b' => KeyInput::new(KeyCode::Esc),
                '>' => KeyInput::new(KeyCode::Right),
                c if c.is_uppercase() => KeyInput::shift(c),
                c => KeyInput::new(c),
            };
            handler.on_event(key, &mut state);
        }
        assert_eq!(state.mode, EditorMode::Normal, "{keys}");
        let cursor = (state.cursor.row, state.cursor.col);
        (state.lines.to_string(), state.clip.get_text(), cursor)
    }

    type Case<'a> = (
        &'a str,
        (usize, usize),
        &'a str,
        &'a str,
        &'a str,
        (usize, usize),
    );

    /// Each: the text, the cursor, the keys; the text, what was yanked and
    /// the cursor after.
    fn check(cases: &[Case]) {
        for &(text, at, keys, after, yanked, cursor) in cases {
            assert_eq!(
                typed(text, at, keys),
                (after.into(), yanked.into(), cursor),
                "{keys} in {text:?} at {at:?}"
            );
        }
    }

    const A: &str = "one two three";
    const B: &str = "one two\n  six ten";
    const LINES: &str = "a1\nb2\nc3\nd4";
    const WORDS: &str = "a foo.bar b";
    const PARAGRAPHS: &str = "a b\nc d\n\ne f";

    #[test]
    fn w_to_the_next_word_its_line_end_at_most_and_cw_is_ce() {
        check(&[
            (A, (0, 4), "dw", "one three", "two ", (0, 4)),
            (A, (0, 4), "cwX\x1b", "one X three", "two", (0, 4)),
            (A, (0, 4), "yw", A, "two ", (0, 4)),
            (A, (0, 0), "d2w", "three", "one two ", (0, 0)),
            (A, (0, 0), "c2wX\x1b", "X three", "one two", (0, 0)),
            // Counts multiply; past the end, to the end.
            (A, (0, 0), "2d3w", "", A, (0, 0)),
            // The last word of a line: the line break stays.
            (B, (0, 4), "dw", "one \n  six ten", "two", (0, 3)),
            (B, (0, 4), "cwX\x1b", "one X\n  six ten", "two", (0, 4)),
            // The buffer's last character.
            (B, (1, 8), "dw", "one two\n  six te", "n", (1, 7)),
            // cw on a blank is dw; dw on an empty line takes it.
            (A, (0, 3), "cwX\x1b", "oneXtwo three", " ", (0, 3)),
            ("a\n\nb", (1, 0), "dw", "a\nb", "\n", (1, 0)),
            // W: anything but whitespace.
            (WORDS, (0, 2), "dw", "a .bar b", "foo", (0, 2)),
            (WORDS, (0, 2), "dW", "a b", "foo.bar ", (0, 2)),
            (WORDS, (0, 2), "cWX\x1b", "a X b", "foo.bar", (0, 2)),
            (WORDS, (0, 2), "yW", WORDS, "foo.bar ", (0, 2)),
            (WORDS, (0, 0), "d2W", "b", "a foo.bar ", (0, 0)),
        ]);
    }

    #[test]
    fn b_and_e_back_to_a_word_and_on_to_its_end() {
        check(&[
            (A, (0, 8), "db", "one three", "two ", (0, 4)),
            (A, (0, 8), "cbX\x1b", "one Xthree", "two ", (0, 4)),
            (A, (0, 8), "yb", A, "two ", (0, 4)),
            (A, (0, 8), "d2b", "three", "one two ", (0, 0)),
            // Over a line's start; at the buffer's, nothing.
            (B, (1, 2), "db", "one six ten", "two\n  ", (0, 4)),
            (A, (0, 0), "db", A, "", (0, 0)),
            (A, (0, 0), "cb", A, "", (0, 0)),
            (WORDS, (0, 10), "dB", "a b", "foo.bar ", (0, 2)),
            (WORDS, (0, 10), "yB", WORDS, "foo.bar ", (0, 2)),
            // e takes the word's last character.
            (A, (0, 0), "de", " two three", "one", (0, 0)),
            (A, (0, 0), "ceX\x1b", "X two three", "one", (0, 0)),
            (A, (0, 0), "ye", A, "one", (0, 0)),
            (A, (0, 0), "d2e", " three", "one two", (0, 0)),
            (A, (0, 12), "de", "one two thre", "e", (0, 11)),
            (B, (0, 6), "de", "one tw ten", "o\n  six", (0, 6)),
            (WORDS, (0, 2), "dE", "a  b", "foo.bar", (0, 2)),
            // From a blank line over to a line's end: those lines, deleted.
            ("a\n\nfoo", (1, 0), "de", "a", "\n\nfoo", (0, 0)),
            ("a\n\nfoo", (1, 0), "dE", "a", "\n\nfoo", (0, 0)),
            ("a\n\nfoo", (1, 0), "ceX\x1b", "a\nX", "\nfoo", (1, 0)),
        ]);
    }

    #[test]
    fn h_l_0_caret_and_dollar_along_the_line() {
        check(&[
            (A, (0, 4), "dh", "onetwo three", " ", (0, 3)),
            (A, (0, 4), "d2h", "ontwo three", "e ", (0, 2)),
            (A, (0, 4), "chX\x1b", "oneXtwo three", " ", (0, 3)),
            (A, (0, 4), "yh", A, " ", (0, 3)),
            (A, (0, 0), "dh", A, "", (0, 0)),
            // At the line's start, h goes nowhere: an empty change, as vim's.
            (A, (0, 0), "chX\x1b", "Xone two three", "", (0, 0)),
            // l: past the last character too, as x; s is cl.
            (A, (0, 0), "dl", "ne two three", "o", (0, 0)),
            (A, (0, 0), "3dl", " two three", "one", (0, 0)),
            (A, (0, 0), "d>", "ne two three", "o", (0, 0)),
            (A, (0, 12), "dl", "one two thre", "e", (0, 11)),
            (A, (0, 10), "5dl", "one two th", "ree", (0, 9)),
            (A, (0, 0), "clX\x1b", "Xne two three", "o", (0, 0)),
            (A, (0, 0), "yl", A, "o", (0, 0)),
            (A, (0, 0), "3sX\x1b", "X two three", "one", (0, 0)),
            // An empty line: nothing to delete, an empty change.
            ("a\n\nb", (1, 0), "dl", "a\n\nb", "", (1, 0)),
            ("a\n\nb", (1, 0), "clX\x1b", "a\nX\nb", "", (1, 0)),
            ("a\n\nb", (1, 0), "sX\x1b", "a\nX\nb", "", (1, 0)),
            ("a\n\nb", (1, 0), "ciwX\x1b", "a\nX\nb", "", (1, 0)),
            ("a\n\nb", (1, 0), "cawX\x1b", "a\nX\nb", "", (1, 0)),
            // 0 and ^: to the line's start, and its first non-blank.
            (A, (0, 4), "d0", "two three", "one ", (0, 0)),
            (A, (0, 4), "c0X\x1b", "Xtwo three", "one ", (0, 0)),
            (A, (0, 4), "y0", A, "one ", (0, 0)),
            (A, (0, 0), "d0", A, "", (0, 0)),
            ("  ab cd", (0, 5), "d^", "  cd", "ab ", (0, 2)),
            ("  ab cd", (0, 5), "c^X\x1b", "  Xcd", "ab ", (0, 2)),
            ("  ab cd", (0, 5), "y^", "  ab cd", "ab ", (0, 2)),
            ("  ab cd", (0, 0), "d^", "ab cd", "  ", (0, 0)),
            // $, D and C: to the line's end.
            (A, (0, 4), "d$", "one ", "two three", (0, 3)),
            (A, (0, 4), "c$X\x1b", "one X", "two three", (0, 4)),
            (A, (0, 4), "y$", A, "two three", (0, 4)),
            (A, (0, 12), "d$", "one two thre", "e", (0, 11)),
            (A, (0, 4), "D", "one ", "two three", (0, 3)),
            (A, (0, 4), "CX\x1b", "one X", "two three", (0, 4)),
            // $ counted: to the end n-1 lines down; on the last line, none.
            (B, (0, 4), "2D", "one ", "two\n  six ten", (0, 3)),
            (B, (0, 4), "d2$", "one ", "two\n  six ten", (0, 3)),
            (B, (1, 2), "2D", B, "", (1, 2)),
        ]);
    }

    #[test]
    fn f_t_and_backward_to_a_character() {
        check(&[
            (A, (0, 0), "dft", "wo three", "one t", (0, 0)),
            (A, (0, 0), "cftX\x1b", "Xwo three", "one t", (0, 0)),
            (A, (0, 0), "yft", A, "one t", (0, 0)),
            (A, (0, 0), "d2ft", "hree", "one two t", (0, 0)),
            (A, (0, 11), "dfe", "one two thr", "ee", (0, 10)),
            (A, (0, 0), "dfz", A, "", (0, 0)),
            (A, (0, 0), "dtt", "two three", "one ", (0, 0)),
            (A, (0, 0), "cttX\x1b", "Xtwo three", "one ", (0, 0)),
            (A, (0, 0), "ytt", A, "one ", (0, 0)),
            // Right before it already: the character under the cursor.
            (A, (0, 3), "dtt", "onetwo three", " ", (0, 3)),
            (A, (0, 0), "dtz", A, "", (0, 0)),
            // F and T leave the cursor's character.
            (A, (0, 12), "dFt", "one two e", "thre", (0, 8)),
            (A, (0, 12), "cFtX\x1b", "one two Xe", "thre", (0, 8)),
            (A, (0, 12), "yFt", A, "thre", (0, 8)),
            (A, (0, 12), "d2Ft", "one e", "two thre", (0, 4)),
            (A, (0, 12), "dFz", A, "", (0, 12)),
            (A, (0, 0), "dFo", A, "", (0, 0)),
            (A, (0, 12), "dTt", "one two te", "hre", (0, 9)),
            (A, (0, 12), "cTtX\x1b", "one two tXe", "hre", (0, 9)),
            (A, (0, 12), "yTt", A, "hre", (0, 9)),
            // Counted: the nth; too many, nothing.
            ("a,b,c,d", (0, 0), "d2t,", ",c,d", "a,b", (0, 0)),
            ("a,b,c,d", (0, 0), "2dt,", ",c,d", "a,b", (0, 0)),
            ("a1a2a3", (0, 5), "d2Ta", "a1a3", "2a", (0, 3)),
            ("a,b,c,d", (0, 0), "d9t,", "a,b,c,d", "", (0, 0)),
        ]);
    }

    #[test]
    fn j_k_g_and_gg_take_lines() {
        check(&[
            (LINES, (0, 1), "dj", "c3\nd4", "\na1\nb2", (0, 0)),
            (LINES, (0, 1), "cjX\x1b", "X\nc3\nd4", "\na1\nb2", (0, 0)),
            (LINES, (0, 1), "yj", LINES, "\na1\nb2", (0, 1)),
            (LINES, (0, 0), "d2j", "d4", "\na1\nb2\nc3", (0, 0)),
            (LINES, (1, 0), "d5j", "a1", "\nb2\nc3\nd4", (0, 0)),
            (LINES, (2, 0), "dj", "a1\nb2", "\nc3\nd4", (1, 0)),
            (LINES, (3, 0), "dj", LINES, "", (3, 0)),
            (LINES, (1, 1), "dk", "c3\nd4", "\na1\nb2", (0, 0)),
            (LINES, (1, 0), "ckX\x1b", "X\nc3\nd4", "\na1\nb2", (0, 0)),
            (LINES, (1, 1), "yk", LINES, "\na1\nb2", (0, 1)),
            (LINES, (3, 0), "d2k", "a1", "\nb2\nc3\nd4", (0, 0)),
            (LINES, (0, 0), "dk", LINES, "", (0, 0)),
            (LINES, (1, 0), "dG", "a1", "\nb2\nc3\nd4", (0, 0)),
            (LINES, (1, 0), "cGX\x1b", "a1\nX", "\nb2\nc3\nd4", (1, 0)),
            (LINES, (1, 1), "yG", LINES, "\nb2\nc3\nd4", (1, 1)),
            (LINES, (0, 0), "d3G", "d4", "\na1\nb2\nc3", (0, 0)),
            (LINES, (3, 0), "dG", "a1\nb2\nc3", "\nd4", (2, 0)),
            (LINES, (2, 0), "dgg", "d4", "\na1\nb2\nc3", (0, 0)),
            (LINES, (2, 1), "ygg", LINES, "\na1\nb2\nc3", (0, 0)),
            (LINES, (3, 0), "d2gg", "a1", "\nb2\nc3\nd4", (0, 0)),
            (LINES, (0, 0), "dgg", "b2\nc3\nd4", "\na1", (0, 0)),
        ]);
    }

    #[test]
    fn the_operators_key_again_takes_its_lines() {
        check(&[
            (LINES, (1, 0), "dd", "a1\nc3\nd4", "\nb2", (1, 0)),
            (LINES, (1, 0), "2dd", "a1\nd4", "\nb2\nc3", (1, 0)),
            (LINES, (1, 0), "d2d", "a1\nd4", "\nb2\nc3", (1, 0)),
            (LINES, (1, 0), "d2_", "a1\nd4", "\nb2\nc3", (1, 0)),
            (LINES, (1, 0), "ccX\x1b", "a1\nX\nc3\nd4", "\nb2", (1, 0)),
            (LINES, (1, 0), "SX\x1b", "a1\nX\nc3\nd4", "\nb2", (1, 0)),
            (LINES, (1, 1), "3yy", LINES, "\nb2\nc3\nd4", (1, 1)),
            // yy and y_ leave the cursor where it was.
            (LINES, (1, 1), "yy", LINES, "\nb2", (1, 1)),
            (LINES, (1, 1), "y_", LINES, "\nb2", (1, 1)),
            // As many as there are; but none past the last line.
            (LINES, (2, 0), "5dd", "a1\nb2", "\nc3\nd4", (1, 0)),
            (LINES, (3, 0), "2dd", LINES, "", (3, 0)),
            // The only line: an empty one left, as vim's, which p puts below.
            ("solo", (0, 1), "dd", "", "\nsolo", (0, 0)),
            ("solo", (0, 1), "ddp", "\nsolo", "\nsolo", (1, 0)),
        ]);
    }

    #[test]
    fn paragraphs_and_brackets() {
        check(&[
            // From a paragraph's start, its lines; from within, to its end.
            (PARAGRAPHS, (0, 0), "d}", "\ne f", "\na b\nc d", (0, 0)),
            (
                PARAGRAPHS,
                (0, 0),
                "c}X\x1b",
                "X\n\ne f",
                "\na b\nc d",
                (0, 0),
            ),
            (PARAGRAPHS, (0, 0), "y}", PARAGRAPHS, "\na b\nc d", (0, 0)),
            (PARAGRAPHS, (0, 2), "d}", "a \n\ne f", "b\nc d", (0, 1)),
            (PARAGRAPHS, (3, 0), "d}", "a b\nc d\n\n", "e f", (3, 0)),
            (PARAGRAPHS, (1, 2), "d{", "d\n\ne f", "a b\nc ", (0, 0)),
            (PARAGRAPHS, (3, 2), "d{", "a b\nc d\nf", "\ne ", (2, 0)),
            // Counted: as far as that many go; with some left, nothing.
            (PARAGRAPHS, (0, 0), "d2}", "", "\na b\nc d\n\ne f", (0, 0)),
            ("foo bar baz", (0, 0), "d2}", "foo bar baz", "", (0, 0)),
            ("foo bar baz", (0, 4), "d2{", "foo bar baz", "", (0, 4)),
            ("foo bar baz", (0, 0), "c2}", "foo bar baz", "", (0, 0)),
            // From a paragraph's start to the end, over lines: those lines.
            ("a\n\nb\nc", (2, 0), "d}", "a\n", "\nb\nc", (1, 0)),
            // % takes both brackets, from either.
            ("f(a, b) x", (0, 1), "d%", "f x", "(a, b)", (0, 1)),
            ("f(a, b) x", (0, 6), "d%", "f x", "(a, b)", (0, 1)),
            ("f(a, b) x", (0, 1), "c%X\x1b", "fX x", "(a, b)", (0, 1)),
            ("f(a, b) x", (0, 1), "y%", "f(a, b) x", "(a, b)", (0, 1)),
            ("f(a, b) x", (0, 8), "d%", "f(a, b) x", "", (0, 8)),
        ]);
    }

    #[test]
    fn text_objects_and_what_isnt_a_motion() {
        check(&[
            (A, (0, 5), "daw", "one three", "two ", (0, 4)),
            (A, (0, 5), "ciwX\x1b", "one X three", "two", (0, 4)),
            (A, (0, 5), "yiw", A, "two", (0, 4)),
            // Not a motion, another operator, Esc: nothing, and the key after
            // is a key of its own.
            (A, (0, 0), "dxl", A, "", (0, 1)),
            (A, (0, 0), "dcw", A, "", (0, 4)),
            (A, (0, 0), "d\x1bx", "ne two three", "o", (0, 0)),
            (A, (0, 0), "di\x1bx", "ne two three", "o", (0, 0)),
            (A, (0, 0), "dfz", A, "", (0, 0)),
            // An empty note, as an empty line: j, b, f fail; c with l, w,
            // its line, goes in.
            ("", (0, 0), "cj", "", "", (0, 0)),
            ("", (0, 0), "cb", "", "", (0, 0)),
            ("", (0, 0), "cfx", "", "", (0, 0)),
            ("", (0, 0), "dd", "", "", (0, 0)),
            ("", (0, 0), "ccX\x1b", "X", "", (0, 0)),
            ("", (0, 0), "sX\x1b", "X", "", (0, 0)),
            ("", (0, 0), "cwX\x1b", "X", "", (0, 0)),
        ]);
    }

    #[test]
    fn dot_repeats_an_operator_with_its_motion_and_count() {
        check(&[
            (A, (0, 0), "dw.", "three", "two ", (0, 0)),
            (A, (0, 0), "cwX\x1bw.", "X X three", "two", (0, 2)),
            (A, (0, 0), "dft.", "hree", "wo t", (0, 0)),
            (LINES, (0, 0), "dj.", "", "\nc3\nd4", (0, 0)),
            ("a b c d e f", (0, 0), "dw3.", "e f", "b c d ", (0, 0)),
            ("a b c d e f", (0, 0), "d2w.", "e f", "c d ", (0, 0)),
            // A failed one isn't either: f finding nothing, j on the last
            // line.
            (A, (0, 0), "xdfz.", "e two three", "n", (0, 0)),
            (LINES, (3, 0), "xdj.", "a1\nb2\nc3\n", "4", (3, 0)),
            // A yank isn't a change: . repeats the one before.
            (A, (0, 0), "xyw.", "e two three", "n", (0, 0)),
            // One u undoes a change, its text too.
            (A, (0, 4), "cwfoo\x1bu", A, "two", (0, 4)),
        ]);
    }
}
