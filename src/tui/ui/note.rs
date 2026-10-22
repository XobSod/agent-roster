//! A session's note above its screen: its pinned lines, or the editor open on
//! it.

use edtui::{EditorMode, EditorTheme, EditorView};
use unicode_width::UnicodeWidthChar;

use super::*;
use crate::note::Note;
use crate::tui::note::{NoteEditor, TAB_WIDTH};

/// The note's pinned line and the one below (or its first two), and where
/// they are in it when it has more.
pub(super) fn note_band(note: &Note, width: usize) -> Vec<Line<'static>> {
    let shown = note.shown();
    let n = shown.lines.len();
    let place = (shown.total > n).then(|| match n {
        1 => format!("{} of {}", shown.first + 1, shown.total),
        _ => format!("{}–{} of {}", shown.first + 1, shown.first + n, shown.total),
    });
    shown
        .lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let mark = match i {
                0 => Span::styled("✎ ", Style::new().fg(ACCENT)),
                _ => Span::raw("  "),
            };
            let right = match &place {
                Some(place) if i + 1 == n => {
                    vec![Span::styled(place.clone(), Style::new().fg(FAINT))]
                }
                _ => Vec::new(),
            };
            let text = Span::raw(text.replace('\t', &" ".repeat(TAB_WIDTH)));
            Line::from(fit(width, vec![mark, text], right))
        })
        .collect()
}

/// The editor's height in `room` rows: its lines and one to add, at least
/// three, at most half the room, with its border.
pub(super) fn editor_height(editor: &NoteEditor, width: u16, room: u16) -> u16 {
    let width = width.saturating_sub(4) as usize;
    let rows: usize = editor.lines().iter().map(|l| screen_rows(l, width)).sum();
    let most = (room / 2).saturating_sub(2).max(3);
    (rows as u16 + 1).clamp(3, most) + 2
}

pub(super) fn draw_note_editor(f: &mut Frame, editor: &NoteEditor, area: Rect) {
    let (mode, color) = match editor.mode() {
        EditorMode::Normal => ("NORMAL", MUTED),
        EditorMode::Insert => ("INSERT", INPUT),
        EditorMode::Visual => ("VISUAL", ACCENT),
        EditorMode::Search => ("SEARCH", ACCENT),
    };
    let mut title = vec![Span::styled(
        " Note ",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if editor.dirty() {
        title.push(Span::styled("[+] ", Style::new().fg(INPUT)));
    }
    let (row, search) = {
        let state = editor.state.borrow();
        let search = (state.mode == EditorMode::Search).then(|| state.search_pattern());
        (state.cursor.row, search)
    };
    // What's typed after `:` or `/`, else the cursor's line.
    let typed = match (&editor.command, &search) {
        (Some(command), _) => Some(format!(":{command}")),
        (None, Some(pattern)) => Some(format!("/{pattern}")),
        (None, None) => None,
    };
    let bottom = match &typed {
        Some(typed) => Line::from(format!(" {typed} ")),
        None => Line::styled(
            format!(" {}/{} ", row + 1, editor.lines().len().max(1)),
            Style::new().fg(FAINT),
        )
        .right_aligned(),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(title))
        .title_top(Line::styled(format!(" {mode} "), Style::new().fg(color)).right_aligned())
        .title_bottom(bottom);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    if let Some(typed) = &typed {
        let x = area.x + 2 + typed.width() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.bottom() - 1));
    }
    // The pin's mark on the left of its line.
    let [gutter, content] =
        Layout::horizontal([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    // edtui can't wrap lines into fewer than two columns (it never returns).
    if content.width < 2 {
        return;
    }
    let theme = EditorTheme::default()
        .base(Style::new())
        .cursor_style(Style::new().add_modifier(Modifier::REVERSED))
        .selection_style(Style::new().bg(FAINT))
        .hide_status_line();
    let mut state = editor.state.borrow_mut();
    f.render_widget(
        EditorView::new(&mut state)
            .theme(theme)
            .wrap(true)
            .tab_width(TAB_WIDTH),
        content,
    );
    // Escape keys held back show where they'd go, as in vim.
    let held = editor.held();
    if let Some(at) = state.cursor_screen_position().filter(|_| !held.is_empty()) {
        let room = content.right().saturating_sub(at.x) as usize;
        f.buffer_mut()
            .set_stringn(at.x, at.y, &held, room, Style::new());
    }
    // Where the pinned line is: below the lines above it, each as many rows
    // as edtui wrapped it into.
    let top = state.viewport_offset().1;
    if let Some(pin) = editor.pin.filter(|&p| p >= top) {
        let y: usize = editor.lines()[top..pin.min(editor.lines().len())]
            .iter()
            .map(|l| screen_rows(l, content.width as usize))
            .sum();
        if y < content.height as usize {
            f.buffer_mut()
                .set_string(gutter.x, content.y + y as u16, "▸", Style::new().fg(ACCENT));
        }
    }
}

/// The rows a line takes, wrapped as edtui wraps it: a character that doesn't
/// fit goes on to the next row.
fn screen_rows(line: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let (mut rows, mut used) = (1, 0);
    for c in line.chars() {
        let w = match c {
            '\t' => TAB_WIDTH,
            c => c.width().unwrap_or(0),
        };
        if used + w > width {
            rows += 1;
            used = 0;
        }
        used += w;
    }
    rows
}
