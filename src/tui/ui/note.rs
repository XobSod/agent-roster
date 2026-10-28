//! The notes above a session's screen: its project's pinned lines and its
//! own, or the editor open on them.

use edtui::{EditorMode, EditorTheme, EditorView};
use unicode_width::UnicodeWidthChar;

use super::*;
use crate::note::Note;
use crate::tui::note::{NoteEditor, OpenNotes, Side, TAB_WIDTH};

/// A project's note's mark, as `✎` is a session's.
pub const PROJECT_NOTE: &str = "▤";

fn note_mark(side: Side) -> &'static str {
    match side {
        Side::Session => "✎",
        Side::Project => PROJECT_NOTE,
    }
}

/// Columns each of two notes needs to show side by side; narrower, they're
/// stacked.
const SIDE_BY_SIDE: usize = 25;

/// The notes' pinned lines and the ones below (`count` of each), each by its
/// mark, in a box:
/// both side by side, the project's on the left, with a line between them
/// (stacked, the project's on top, where they don't fit); nothing for notes
/// that aren't there.
pub(super) fn note_bands(
    project: Option<&Note>,
    session: Option<&Note>,
    width: usize,
    count: usize,
) -> Vec<Line<'static>> {
    let faint = |text: String| Span::styled(text, Style::new().fg(FAINT));
    let line = |text: String| Line::from(faint(text));
    let rule = |n: usize| "─".repeat(n);
    // Inside the box: a border and a space on each side.
    let inner = width.saturating_sub(4);
    let boxed = |rows: Vec<Line<'static>>| {
        rows.into_iter().map(|row| {
            let mut spans = vec![faint("│ ".into())];
            spans.extend(row.spans);
            spans.push(faint(" │".into()));
            Line::from(spans)
        })
    };
    let top = |at: Option<usize>, corners: (&str, &str)| -> Line<'static> {
        let edge = match at {
            Some(at) => format!("{}┬{}", rule(at), rule(width - 3 - at)),
            None => rule(width.saturating_sub(2)),
        };
        line(format!("{}{edge}{}", corners.0, corners.1))
    };
    let bottom = |at: Option<usize>| match at {
        Some(at) => line(format!("╰{}┴{}╯", rule(at), rule(width - 3 - at))),
        None => line(format!("╰{}╯", rule(width.saturating_sub(2)))),
    };
    let mut lines = Vec::new();
    match (project, session) {
        (None, None) => {}
        (Some(project), Some(session)) if inner < 2 * SIDE_BY_SIDE + 3 => {
            lines.push(top(None, ("╭", "╮")));
            lines.extend(boxed(note_band(project, Side::Project, inner, count)));
            lines.push(line(format!("├{}┤", rule(width.saturating_sub(2)))));
            lines.extend(boxed(note_band(session, Side::Session, inner, count)));
            lines.push(bottom(None));
        }
        (Some(project), Some(session)) => {
            let left_width = (inner - 3) / 2;
            let right_width = inner - 3 - left_width;
            let left = note_band(project, Side::Project, left_width, count);
            let right = note_band(session, Side::Session, right_width, count);
            let half = |lines: &[Line<'static>], i: usize, width: usize| match lines.get(i) {
                Some(line) => line.spans.clone(),
                None => vec![Span::raw(" ".repeat(width))],
            };
            let rows = (0..left.len().max(right.len())).map(|i| {
                let mut spans = half(&left, i, left_width);
                spans.push(faint(" │ ".into()));
                spans.extend(half(&right, i, right_width));
                Line::from(spans)
            });
            // Where the line between them meets the box.
            let at = Some(left_width + 2);
            lines.push(top(at, ("╭", "╮")));
            lines.extend(boxed(rows.collect()));
            lines.push(bottom(at));
        }
        (Some(note), None) | (None, Some(note)) => {
            let side = match project {
                Some(_) => Side::Project,
                None => Side::Session,
            };
            lines.push(top(None, ("╭", "╮")));
            lines.extend(boxed(note_band(note, side, inner, count)));
            lines.push(bottom(None));
        }
    }
    lines
}

/// `lines` of the note from its pinned one (or its first), and where they
/// are in it when it has more.
fn note_band(note: &Note, side: Side, width: usize, lines: usize) -> Vec<Line<'static>> {
    let shown = note.shown(lines);
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
                0 => Span::styled(format!("{} ", note_mark(side)), Style::new().fg(ACCENT)),
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

/// The editor, titled with the note shown and the other there is to show
/// (its key, its mark), each marked if it has unsaved changes, and if it has
/// an external change since it was opened; the shown one with its conflicts
/// instead, if it has any.
pub(super) fn draw_note_editor(f: &mut Frame, open: &OpenNotes, area: Rect) {
    let editor = open.shown();
    let (mode, color) = match editor.mode() {
        EditorMode::Normal => ("NORMAL", MUTED),
        EditorMode::Insert => ("INSERT", INPUT),
        EditorMode::Visual => ("VISUAL", ACCENT),
        EditorMode::Search => ("SEARCH", ACCENT),
    };
    let unsaved = || Span::styled("[+] ", Style::new().fg(INPUT));
    let mut title = vec![Span::styled(
        format!(" {} {} ", note_mark(open.side()), editor.label),
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if editor.dirty() {
        title.push(unsaved());
    }
    let warning = match editor.conflicts() {
        0 if editor.external => Some("external change".to_string()),
        0 => None,
        1 => Some("1 conflict".to_string()),
        n => Some(format!("{n} conflicts")),
    };
    if let Some(warning) = warning {
        title.push(Span::styled(format!("{warning} "), Style::new().fg(ERROR)));
    }
    let other = match open.side() {
        Side::Session => Some(("[", Side::Project, "project")),
        Side::Project => open
            .session
            .as_ref()
            .map(|_| ("]", Side::Session, "session")),
    };
    if let Some((key, side, whose)) = other {
        title.push(Span::styled(
            format!(" {key} {} {whose} ", note_mark(side)),
            Style::new().fg(FAINT),
        ));
        if open.other().is_some_and(NoteEditor::dirty) {
            title.push(unsaved());
        }
        if open.other().is_some_and(|e| e.external) {
            title.push(Span::styled("external change ", Style::new().fg(ERROR)));
        }
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
            f.buffer_mut().set_string(
                gutter.x,
                content.y + y as u16,
                note_mark(open.side()),
                Style::new().fg(ACCENT),
            );
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
