//! Vim's word motions, as its `fwd_word`, `bck_word` and `end_word` go:
//! `w`, `b` and `e`, and `W`, `B` and `E` (`big`), over lines, stopping on
//! empty ones. A position can be a line's end, past its last character, as
//! vim's can while it moves. Added by roster.

use jagged::index::RowIndex;

use crate::{Index2, Lines};

/// A character's kind, for words: whitespace, a word's (letters, digits,
/// `_`, in any script), or another mark; for WORDs, whitespace or not.
pub(crate) fn kind(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn row(lines: &Lines, at: usize) -> &[char] {
    lines.get(RowIndex::new(at)).map_or(&[], Vec::as_slice)
}

/// The kind at `at`, a line's end being whitespace's.
fn class(lines: &Lines, at: Index2, big: bool) -> u8 {
    row(lines, at.row).get(at.col).map_or(0, |&c| kind(c, big))
}

fn on_empty_line(lines: &Lines, at: Index2) -> bool {
    at.col == 0 && row(lines, at.row).is_empty()
}

/// How a step from one position to the next, or the one before, went.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Along,
    /// Forward on to a line's end.
    OntoEnd,
    /// To another line.
    Across,
    /// None: the buffer ends there.
    Stuck,
}

impl Step {
    fn crossed(self) -> bool {
        matches!(self, Step::OntoEnd | Step::Across)
    }
}

fn next(lines: &Lines, at: &mut Index2) -> Step {
    let len = row(lines, at.row).len();
    if at.col < len {
        at.col += 1;
        return if at.col < len {
            Step::Along
        } else {
            Step::OntoEnd
        };
    }
    if at.row + 1 < lines.len() {
        *at = Index2::new(at.row + 1, 0);
        return Step::Across;
    }
    Step::Stuck
}

fn prev(lines: &Lines, at: &mut Index2) -> Step {
    if at.col > 0 {
        at.col -= 1;
        return Step::Along;
    }
    if at.row > 0 {
        at.row -= 1;
        at.col = row(lines, at.row).len();
        return Step::Across;
    }
    Step::Stuck
}

/// Where `count` words forward go from `at` (`w`), and whether it was
/// further than the buffer's last character. After an operator (`eol`),
/// the last one stops at its line's end (`dw` of a line's last word).
pub(crate) fn forward(
    lines: &Lines,
    mut at: Index2,
    count: usize,
    big: bool,
    eol: bool,
) -> (Index2, bool) {
    let class = |at| class(lines, at, big);
    for left in (0..count).rev() {
        let stop = eol && left == 0;
        let start = class(at);
        let last_line = at.row + 1 >= lines.len();
        let step = next(lines, &mut at);
        if step == Step::Stuck || step.crossed() && last_line {
            return (at, false);
        }
        if step.crossed() && stop {
            return (at, true);
        }
        // Past the word it's on, then the whitespace after it.
        if start != 0 {
            while class(at) == start {
                let step = next(lines, &mut at);
                if step == Step::Stuck || step.crossed() && stop {
                    return (at, true);
                }
            }
        }
        while class(at) == 0 && !on_empty_line(lines, at) {
            let step = next(lines, &mut at);
            if step == Step::Stuck || step.crossed() && stop {
                return (at, true);
            }
        }
    }
    (at, true)
}

/// Where `count` words back go from `at` (`b`), and whether one didn't
/// start at the buffer's start.
pub(crate) fn backward(lines: &Lines, mut at: Index2, count: usize, big: bool) -> (Index2, bool) {
    let class = |at| class(lines, at, big);
    for _ in 0..count {
        if prev(lines, &mut at) == Step::Stuck {
            return (at, false);
        }
        // Back over the whitespace before the word, to an empty line at
        // most, then to the word's start.
        'word: {
            while class(at) == 0 {
                if on_empty_line(lines, at) {
                    break 'word;
                }
                if prev(lines, &mut at) == Step::Stuck {
                    return (at, true);
                }
            }
            let start = class(at);
            while class(at) == start {
                if prev(lines, &mut at) == Step::Stuck {
                    return (at, true);
                }
            }
            next(lines, &mut at);
        }
    }
    (at, true)
}

/// Where `count` ends of words forward go from `at` (`e`). With `stop`,
/// the first is the end of the word `at` is on, even if it's there already
/// (`cw`, as `ce` that stops there).
pub(crate) fn end(
    lines: &Lines,
    mut at: Index2,
    count: usize,
    big: bool,
    mut stop: bool,
) -> Index2 {
    let class = |at| class(lines, at, big);
    for _ in 0..count {
        let start = class(at);
        if next(lines, &mut at) == Step::Stuck {
            return at;
        }
        if class(at) == start && start != 0 {
            while class(at) == start {
                if next(lines, &mut at) == Step::Stuck {
                    return at;
                }
            }
        } else if !stop || start == 0 {
            while class(at) == 0 {
                if next(lines, &mut at) == Step::Stuck {
                    return at;
                }
            }
            let word = class(at);
            while class(at) == word {
                if next(lines, &mut at) == Step::Stuck {
                    return at;
                }
            }
        }
        prev(lines, &mut at);
        stop = false;
    }
    at
}

/// Whether `at` is on whitespace, or a line's end.
pub(crate) fn blank(lines: &Lines, at: Index2) -> bool {
    class(lines, at, true) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(row: usize, col: usize) -> Index2 {
        Index2::new(row, col)
    }

    #[test]
    fn forward_stops_at_a_lines_end_after_an_operator() {
        let lines = Lines::from("one two\n  three");
        // A motion: to the next line's first word.
        assert_eq!(forward(&lines, at(0, 4), 1, false, false), (at(1, 2), true));
        // After an operator: the line's end.
        assert_eq!(forward(&lines, at(0, 4), 1, false, true), (at(0, 7), true));
        // Two: over the line's end, then to the end of the buffer.
        assert_eq!(forward(&lines, at(0, 4), 2, false, true), (at(1, 7), true));
        // From the buffer's last character: its end, and not ok.
        assert_eq!(forward(&lines, at(1, 6), 1, false, true), (at(1, 7), false));
    }

    #[test]
    fn forward_and_back_stop_on_empty_lines() {
        let lines = Lines::from("a\n\nb");
        assert_eq!(forward(&lines, at(0, 0), 1, false, false).0, at(1, 0));
        assert_eq!(backward(&lines, at(2, 0), 1, false), (at(1, 0), true));
        assert_eq!(backward(&lines, at(1, 0), 1, false), (at(0, 0), true));
        assert_eq!(backward(&lines, at(0, 0), 1, false), (at(0, 0), false));
    }

    #[test]
    fn back_from_a_lines_start_goes_to_the_last_word_before() {
        let lines = Lines::from("one two  \nsix");
        assert_eq!(backward(&lines, at(1, 0), 1, false).0, at(0, 4));
    }

    #[test]
    fn words_are_letters_of_any_script_and_words_marks_apart() {
        let lines = Lines::from("zażółć gęślą foo.bar");
        assert_eq!(forward(&lines, at(0, 0), 1, false, false).0, at(0, 7));
        assert_eq!(end(&lines, at(0, 0), 1, false, false), at(0, 5));
        assert_eq!(forward(&lines, at(0, 13), 1, false, false).0, at(0, 16));
        assert_eq!(forward(&lines, at(0, 13), 1, true, false).0, at(0, 20));
        assert_eq!(end(&lines, at(0, 13), 1, true, false), at(0, 19));
        assert_eq!(backward(&lines, at(0, 19), 1, true).0, at(0, 13));
    }

    #[test]
    fn end_with_stop_stays_at_the_end_of_the_word_its_on() {
        let lines = Lines::from("ab cd");
        assert_eq!(end(&lines, at(0, 1), 1, false, false), at(0, 4));
        assert_eq!(end(&lines, at(0, 1), 1, false, true), at(0, 1));
        assert_eq!(end(&lines, at(0, 1), 2, false, true), at(0, 4));
        assert_eq!(end(&lines, at(0, 0), 1, false, true), at(0, 1));
    }
}
