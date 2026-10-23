//! Fitting text to a width, measured in terminal cells.

use super::*;

/// The end of `s` if it's wider than `width`, with "…" in front.
pub(super) fn tail(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let (mut start, mut used) = (s.len(), 0);
    for (i, g) in s.grapheme_indices(true).rev() {
        if used + g.width() + 1 > width {
            break;
        }
        used += g.width();
        start = i;
    }
    format!("…{}", &s[start..])
}

/// How many lines `text` takes wrapped at word boundaries to `width`; a word
/// wider than that is broken over several.
pub(super) fn wrapped_lines(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut lines = 1;
    let mut used = 0;
    for word in text.split_whitespace() {
        let w = word.width().max(1);
        if used > 0 && used + 1 + w <= width {
            used += 1 + w;
        } else {
            if used > 0 {
                lines += 1;
            }
            lines += (w - 1) / width;
            used = (w - 1) % width + 1;
        }
    }
    lines
}

/// `text` broken between words into lines of at most `width` columns (a
/// word wider than that is cut).
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.width() + 1 + word.width() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        if line.width() > width {
            lines.push(truncate(&line, width));
            line.clear();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

pub(super) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let (width, height) = (width.min(area.width), height.min(area.height));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// `s` cut to `width` columns, with "…" if it was longer.
pub(crate) fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        if used + g.width() + 1 > width {
            break;
        }
        used += g.width();
        out.push_str(g);
    }
    out.push('…');
    out
}

/// `s` cut or padded to exactly `width` columns (wide characters take two).
pub(super) fn pad(s: &str, width: usize) -> String {
    let s = truncate(s, width);
    let fill = width.saturating_sub(s.width());
    s + &" ".repeat(fill)
}

/// A line `width` columns wide: `left` cut to fit, `right` flush right.
pub(crate) fn fit(
    width: usize,
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
) -> Vec<Span<'static>> {
    let right_width: usize = right.iter().map(Span::width).sum();
    let room = width.saturating_sub(right_width + usize::from(right_width > 0));
    let mut spans = Vec::new();
    let mut used = 0;
    for span in left {
        let w = span.width();
        if used + w <= room {
            used += w;
            spans.push(span);
        } else {
            let cut = truncate(&span.content, room - used);
            used += cut.width();
            spans.push(Span::styled(cut, span.style));
            break;
        }
    }
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + right_width)),
    ));
    spans.extend(right);
    spans
}
