//! Drawing: the session list (shelves or projects) on the left, a header and the
//! selected agent's screen on the right, a footer with hints, and popups on top.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::app::{App, Dialog, Mode};
use super::form::{Form, Kind};
use super::list::{Entry, Group};
use super::theme::{
    self, ACCENT, CLAUDE, ERROR, FAINT, INPUT, MUTED, SELECTED_BG, UNREAD, WORKING,
};
use crate::keys::{Action, Menu};
use crate::model::{Session, State};
use crate::util::now_ms;

pub fn draw(f: &mut Frame, app: &App) {
    let now = now_ms();
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let [side, divider, right] = Layout::horizontal([
        Constraint::Length(sidebar_width(main.width)),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(main);
    if app.mode == Mode::Server {
        draw_server(f, app, main);
    } else {
        draw_sidebar(f, app, side, now);
        f.render_widget(
            Paragraph::new(vec![Line::from("│"); divider.height as usize])
                .style(Style::new().fg(FAINT)),
            divider,
        );
        draw_main(f, app, right, now);
    }
    draw_footer(f, app, footer);
    if let Some(menu) = app.menu {
        draw_menu(f, app, menu, main);
    }
    if app.help {
        draw_help(f, app, main);
    }
    if let Some(dialog) = &app.dialog {
        draw_dialog(f, dialog, main);
    }
}

/// True while something on screen animates (a working session's spinner).
pub fn animating(app: &App) -> bool {
    app.sessions
        .iter()
        .any(|s| s.deleted_at.is_none() && s.state == State::Working)
}

fn sidebar_width(total: u16) -> u16 {
    (total * 3 / 10)
        .clamp(34, 50)
        .min(total.saturating_sub(20).max(10))
}

// Sidebar

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect, now: i64) {
    let [top, sub, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    let width = area.width as usize;

    let live: Vec<&Session> = app
        .sessions
        .iter()
        .filter(|s| s.deleted_at.is_none())
        .collect();
    let needs = live
        .iter()
        .filter(|s| s.state == State::NeedsInput || (s.state == State::Idle && s.unread))
        .count();
    let working = live.iter().filter(|s| s.state == State::Working).count();
    let mut summary = Vec::new();
    if needs > 0 {
        summary.push(Span::styled(
            format!("{needs} need you"),
            Style::new().fg(UNREAD),
        ));
    }
    if working > 0 {
        if !summary.is_empty() {
            summary.push(Span::styled(" · ", Style::new().fg(FAINT)));
        }
        summary.push(Span::styled(
            format!("{working} working"),
            Style::new().fg(WORKING),
        ));
    }
    summary.push(Span::raw(" "));
    f.render_widget(
        Paragraph::new(Line::from(fit(
            width,
            vec![Span::raw(" "), "roster".bold()],
            summary,
        ))),
        top,
    );

    let note = match (app.mode, app.filter.and_then(|id| app.project(id))) {
        (Mode::Deleted, _) => Some(" deleted sessions".to_string()),
        (Mode::Live, Some(p)) => Some(format!(" only {}", p.name)),
        _ => None,
    };
    if let Some(note) = note {
        f.render_widget(
            Paragraph::new(Line::styled(
                truncate(&note, width),
                Style::new().fg(MUTED).italic(),
            )),
            sub,
        );
    }

    let (lines, spans) = list_lines(app, width, now);
    let height = list.height as usize;
    // Keep the selected entry in view.
    let mut scroll = app.scroll.get().min(lines.len().saturating_sub(height));
    if let Some(&(start, len)) = spans.get(app.selected) {
        if start + len > scroll + height {
            scroll = (start + len).saturating_sub(height);
        }
        if start < scroll {
            scroll = start;
        }
    }
    app.scroll.set(scroll);
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(height).collect();
    f.render_widget(Paragraph::new(visible), list);
}

/// The list's lines, and each entry's first line and line count.
fn list_lines(app: &App, width: usize, now: i64) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut previous: Option<&Entry> = None;
    let content = width.saturating_sub(1);
    for (i, entry) in app.entries.iter().enumerate() {
        // A blank line before headings and around cards; rows sit tight.
        match (previous, entry) {
            (Some(_), Entry::Heading { .. })
            | (Some(Entry::Card(_)), Entry::Card(_) | Entry::Row(_))
            | (Some(Entry::Row(_)), Entry::Card(_)) => lines.push(Line::default()),
            _ => {}
        }
        let rows: Vec<Vec<Span<'static>>> = match entry {
            Entry::Heading {
                label,
                count,
                open,
                group,
            } => vec![heading(*group, label, *count, *open, content)],
            Entry::Card(s) => card(s, !app.grouped, content, now),
            Entry::Row(s) => vec![row(app, s, content, now)],
        };
        spans.push((lines.len(), rows.len()));
        for row in rows {
            lines.push(framed(row, i == app.selected, width));
        }
        previous = Some(entry);
    }
    (lines, spans)
}

/// A list line with the selection bar and background.
fn framed(content: Vec<Span<'static>>, selected: bool, width: usize) -> Line<'static> {
    let mut spans = vec![if selected {
        Span::styled("▎", Style::new().fg(ACCENT))
    } else {
        Span::raw(" ")
    }];
    spans.extend(content);
    let mut line = Line::from(spans);
    if selected {
        let pad = width.saturating_sub(line.width());
        line.spans.push(Span::raw(" ".repeat(pad)));
        line = line.style(Style::new().bg(SELECTED_BG));
    }
    line
}

fn heading(
    group: Group,
    label: &str,
    count: usize,
    open: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let chevron = Span::styled(if open { " ⌄ " } else { " › " }, Style::new().fg(MUTED));
    let mut left = match group {
        Group::Project(_) => vec![
            badge(label),
            Span::raw(" "),
            label.to_string().bold(),
            Span::styled(format!(" ({count}) "), Style::new().fg(MUTED)),
        ],
        Group::Shelf(_) => vec![
            Span::styled(
                label.to_string(),
                Style::new().fg(MUTED).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" ({count}) "), Style::new().fg(MUTED)),
        ],
    };
    let used: usize = left.iter().map(Span::width).sum::<usize>() + chevron.width();
    left.push(Span::styled(
        "─".repeat(width.saturating_sub(used)),
        Style::new().fg(FAINT),
    ));
    left.push(chevron);
    left
}

/// A session's two lines; `with_badge` unless its project heading is right above.
fn card(s: &Session, with_badge: bool, width: usize, now: i64) -> Vec<Vec<Span<'static>>> {
    let working = s.state == State::Working;
    let title_style = match (s.state, s.unread) {
        (State::NeedsInput, _) | (State::Idle, true) => Style::new().add_modifier(Modifier::BOLD),
        (State::Working, _) => Style::new().fg(MUTED),
        _ => Style::new(),
    };
    let meta_color = if working { FAINT } else { MUTED };
    let first = fit(
        width,
        vec![
            project_mark(&s.project, with_badge),
            Span::raw(" "),
            Span::styled(s.display_title().to_string(), title_style),
        ],
        [status(s, now), vec![Span::raw(" ")]].concat(),
    );
    let second = fit(
        width,
        vec![
            Span::raw("   "),
            Span::styled(meta(s), Style::new().fg(meta_color)),
        ],
        vec![glyph(&s.agent), Span::raw(" ")],
    );
    vec![first, second]
}

fn row(app: &App, s: &Session, width: usize, now: i64) -> Vec<Span<'static>> {
    let right = if app.mode == Mode::Deleted {
        let ago = s.deleted_at.map_or(String::new(), |t| theme::ago(now - t));
        vec![Span::styled(
            format!("deleted {ago} "),
            Style::new().fg(MUTED),
        )]
    } else {
        [status(s, now), vec![Span::raw(" ")]].concat()
    };
    let title = if app.mode == Mode::Deleted {
        s.label()
    } else {
        s.display_title().to_string()
    };
    fit(
        width,
        vec![
            project_mark(&s.project, !app.grouped || app.mode == Mode::Deleted),
            Span::raw(" "),
            Span::styled(title, Style::new().fg(MUTED)),
        ],
        right,
    )
}

/// The badge, or blank space of the same width.
fn project_mark(project: &str, badge_shown: bool) -> Span<'static> {
    if badge_shown {
        badge(project)
    } else {
        Span::raw("  ")
    }
}

/// The colored two-letter project badge.
fn badge(project: &str) -> Span<'static> {
    Span::styled(
        theme::monogram(project),
        Style::new()
            .fg(theme::project_color(project))
            .add_modifier(Modifier::BOLD),
    )
}

fn glyph(agent: &str) -> Span<'static> {
    if agent == crate::agent::CLAUDE {
        Span::styled("✱", Style::new().fg(CLAUDE))
    } else {
        Span::styled("◇", Style::new().fg(MUTED))
    }
}

/// The card's second line: name, then model and effort (and the agent, if not Claude).
fn meta(s: &Session) -> String {
    let mut meta = s.name.clone();
    if s.agent != crate::agent::CLAUDE {
        meta.push_str(&format!(" · {}", s.agent));
    }
    match (s.model_label(), s.effort_label()) {
        (Some(m), Some(e)) => meta.push_str(&format!(" · {m} {e}")),
        (Some(m), None) => meta.push_str(&format!(" · {m}")),
        (None, Some(e)) => meta.push_str(&format!(" · {e}")),
        (None, None) => {}
    }
    meta
}

/// The state at the right of a card: a word, a spinner with a clock, or an age.
fn status(s: &Session, now: i64) -> Vec<Span<'static>> {
    let since = now - s.state_since.unwrap_or(s.created_at);
    let span = match (s.state, s.unread) {
        (State::NeedsInput, _) => {
            Span::styled("Input", Style::new().fg(INPUT).add_modifier(Modifier::BOLD))
        }
        (State::Working, _) => Span::styled(
            format!("{} {}", theme::spinner(now), theme::clock(since)),
            Style::new().fg(WORKING),
        ),
        (State::Idle, true) => {
            Span::styled(format!("● {}", theme::ago(since)), Style::new().fg(UNREAD))
        }
        (State::Idle, false) => Span::styled(theme::ago(since), Style::new().fg(MUTED)),
        (State::Stopped, _) => match s.exit_status {
            Some(code) if code != 0 => Span::styled(format!("exit {code}"), Style::new().fg(ERROR)),
            _ => Span::styled("stopped", Style::new().fg(FAINT)),
        },
    };
    vec![span]
}

/// The state in words, for the header.
fn state_words(s: &Session, now: i64) -> Span<'static> {
    let since = now - s.state_since.unwrap_or(s.created_at);
    match (s.state, s.unread) {
        (State::NeedsInput, _) => Span::styled("needs input", Style::new().fg(INPUT)),
        (State::Working, _) => Span::styled(
            format!("working {}", theme::clock(since)),
            Style::new().fg(WORKING),
        ),
        (State::Idle, true) => Span::styled(
            format!("finished {}", theme::ago(since)),
            Style::new().fg(UNREAD),
        ),
        (State::Idle, false) => Span::styled(
            format!("idle {}", theme::ago(since)),
            Style::new().fg(MUTED),
        ),
        (State::Stopped, _) => match s.exit_status {
            Some(code) if code != 0 => {
                Span::styled(format!("exited {code}"), Style::new().fg(ERROR))
            }
            _ => Span::styled("stopped", Style::new().fg(MUTED)),
        },
    }
}

// Main area

fn draw_main(f: &mut Frame, app: &App, area: Rect, now: i64) {
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    let [head, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(header(app, head.width as usize, now))),
        head,
    );
    // The bottom of an agent's screen is where its prompt is; long lines are cut.
    let lines = &app.preview.lines;
    let skip = lines.len().saturating_sub(body.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines[skip..].to_vec())), body);
}

fn header(app: &App, width: usize, now: i64) -> Vec<Span<'static>> {
    let sep = || Span::styled(" · ", Style::new().fg(FAINT));
    match app.selected_entry() {
        Some(Entry::Card(s) | Entry::Row(s)) => {
            let left = vec![
                badge(&s.project),
                Span::raw(" "),
                Span::styled(s.project.clone(), Style::new().fg(MUTED)),
                Span::styled(" / ", Style::new().fg(FAINT)),
                Span::styled(
                    s.display_title().to_string(),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ];
            let mut right = vec![
                glyph(&s.agent),
                Span::raw(" "),
                Span::styled(s.agent.clone(), Style::new().fg(MUTED)),
            ];
            for value in [s.model_label(), s.effort_label()].into_iter().flatten() {
                right.push(sep());
                right.push(Span::styled(value, Style::new().fg(MUTED)));
            }
            right.push(sep());
            if s.deleted_at.is_some() {
                right.push(Span::styled("deleted", Style::new().fg(MUTED)));
            } else {
                right.push(state_words(s, now));
            }
            fit(width, left, right)
        }
        Some(Entry::Heading {
            label,
            count,
            group,
            ..
        }) => {
            let mut left = Vec::new();
            if matches!(group, Group::Project(_)) {
                left.extend([badge(label), Span::raw(" ")]);
            }
            left.push(Span::styled(
                label.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ));
            let noun = if *count == 1 { "session" } else { "sessions" };
            fit(
                width,
                left,
                vec![Span::styled(
                    format!("{count} {noun}"),
                    Style::new().fg(MUTED),
                )],
            )
        }
        None => vec!["roster".bold()],
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = match app.message() {
        Some(m) => Line::styled(
            format!(" {}", m.text),
            Style::new().fg(if m.error { ERROR } else { INPUT }),
        ),
        None => {
            let mut spans = vec![Span::raw(" ")];
            for (key, what) in app.hints() {
                spans.push(Span::styled(key, Style::new().add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(format!(" {what}   "), Style::new().fg(MUTED)));
            }
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

/// The tmux server and database report, full width.
fn draw_server(f: &mut Frame, app: &App, area: Rect) {
    use crate::doctor::Level;
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    let [head, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    let state = match app.problems {
        0 => Span::styled("no problems", Style::new().fg(UNREAD)),
        1 => Span::styled("1 problem", Style::new().fg(ERROR)),
        n => Span::styled(format!("{n} problems"), Style::new().fg(ERROR)),
    };
    let title = vec![
        "roster".bold(),
        Span::styled("  tmux server and database", Style::new().fg(MUTED)),
    ];
    f.render_widget(
        Paragraph::new(Line::from(fit(head.width as usize, title, vec![state]))),
        head,
    );
    let lines: Vec<Line> = app
        .report
        .iter()
        .skip(app.report_scroll)
        .map(|line| {
            let (mark, style) = match line.level {
                Level::Heading => ("", Style::new().add_modifier(Modifier::BOLD)),
                Level::Info => (" ", Style::new()),
                Level::Ok => ("✓", Style::new().fg(UNREAD)),
                Level::Note => ("·", Style::new().fg(MUTED)),
                Level::Problem => ("✗", Style::new().fg(ERROR)),
            };
            match line.level {
                Level::Heading => Line::styled(line.text.clone(), style),
                _ if line.text.is_empty() => Line::default(),
                _ => Line::from(vec![
                    Span::styled(mark, style),
                    Span::styled(line.text[1..].to_string(), style),
                ]),
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), body);
}

// Popups

fn popup_block(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MUTED))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().add_modifier(Modifier::BOLD),
        ))
}

/// The open menu's keys, in a box at the bottom left.
fn draw_menu(f: &mut Frame, app: &App, menu: Menu, area: Rect) {
    let cells: Vec<Vec<Span>> = app
        .keymap
        .menu_items(menu)
        .into_iter()
        .map(|(key, action)| {
            vec![
                Span::styled(
                    key.to_string(),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {}", action.describe())),
            ]
        })
        .collect();
    let cell_width = cells
        .iter()
        .map(|c| c.iter().map(Span::width).sum::<usize>())
        .max()
        .unwrap_or(0)
        + 3;
    let rows = cells.len().div_ceil(2);
    let mut lines = Vec::new();
    for r in 0..rows {
        let mut spans = vec![Span::raw(" ")];
        for cell in cells.iter().skip(r * 2).take(2) {
            let width: usize = cell.iter().map(Span::width).sum();
            spans.extend(cell.iter().cloned());
            spans.push(Span::raw(" ".repeat(cell_width - width)));
        }
        lines.push(Line::from(spans));
    }
    let width = (cell_width * 2 + 3).max(menu.name().len() + 10) as u16;
    let height = rows as u16 + 2;
    let rect = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(height),
        width: width.min(area.width.saturating_sub(1)),
        height: height.min(area.height),
    };
    let block = popup_block(menu.name())
        .title_bottom(Line::styled(" Esc ", Style::new().fg(MUTED)).right_aligned());
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Every key, by where it works.
fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let keymap = &app.keymap;
    let key = |k: String| {
        Span::styled(
            format!("{k:>7}  "),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )
    };
    let title = |t: String| Line::styled(t, Style::new().add_modifier(Modifier::BOLD));
    let mut lines = vec![title("Anywhere".into())];
    for (k, action) in keymap.top_items() {
        lines.push(Line::from(vec![
            key(k.to_string()),
            Span::raw(action.describe()),
        ]));
    }
    for menu in Menu::ALL {
        let Some(open) = keymap.label(Action::Open(menu)) else {
            continue;
        };
        lines.push(Line::default());
        lines.push(title(format!(
            "{} menu ({open} …)",
            capitalize(menu.name())
        )));
        for (k, action) in keymap.menu_items(menu) {
            lines.push(Line::from(vec![
                key(format!("{open} {k}")),
                Span::raw(action.describe()),
            ]));
        }
    }
    lines.push(Line::default());
    lines.push(title("Attached to an agent".into()));
    lines.push(Line::from(vec![
        key(keymap.detach.clone()),
        Span::raw("back to roster (marks it read)"),
    ]));
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = centered(area, 46, height);
    let block = popup_block("keys")
        .title_bottom(Line::styled(" any key closes ", Style::new().fg(MUTED)).right_aligned());
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_dialog(f: &mut Frame, dialog: &Dialog, area: Rect) {
    let width = area.width.saturating_sub(4).min(76);
    match dialog {
        Dialog::Form(form) => draw_form(f, form, area),
        Dialog::Confirm { question, .. } => {
            let rect = centered(area, width, 6);
            let key = |k: &'static str| {
                Span::styled(k, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
            };
            let text = Text::from(vec![
                Line::from(question.clone()),
                Line::default(),
                Line::from(vec![key("y"), " yes    ".into(), key("n"), " no".into()]),
            ]);
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .block(popup_block("confirm")),
                rect,
            );
        }
        Dialog::Input { prompt, value, .. } => {
            let rect = centered(area, width, 5);
            let block = popup_block("input").title_bottom(
                Line::styled(" Enter ok · Esc cancel ", Style::new().fg(MUTED)).right_aligned(),
            );
            let inner = block.inner(rect);
            f.render_widget(Clear, rect);
            f.render_widget(block, rect);
            let [prompt_area, _, value_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            // Show the end of a value that's longer than the box.
            let room = inner.width.saturating_sub(3) as usize;
            let count = value.chars().count();
            let shown: String = value.chars().skip(count.saturating_sub(room)).collect();
            f.render_widget(Paragraph::new(prompt.clone()), prompt_area);
            f.render_widget(Paragraph::new(format!("> {shown}")), value_area);
            f.set_cursor_position((value_area.x + 2 + shown.width() as u16, value_area.y));
        }
    }
}

/// A form: one line per visible field, the focused one marked, with its hint on
/// the right; errors and notes below.
fn draw_form(f: &mut Frame, form: &Form, area: Rect) {
    const LABEL: usize = 10;
    let visible: Vec<usize> = (0..form.fields.len())
        .filter(|&i| !form.fields[i].hidden)
        .collect();
    let notes: Vec<Line> = [
        form.error
            .as_ref()
            .map(|e| Line::styled(format!("  {e}"), Style::new().fg(ERROR))),
        form.note
            .as_ref()
            .map(|n| Line::styled(format!("  {n}"), Style::new().fg(MUTED))),
    ]
    .into_iter()
    .flatten()
    .collect();
    let height = (visible.len() + notes.len() + 4) as u16;
    let rect = centered(area, area.width.saturating_sub(4).min(84), height);
    let hint = format!(" Enter {} · Esc cancel ", form.verb());
    let block = popup_block(&form.title)
        .title_bottom(Line::styled(hint, Style::new().fg(MUTED)).right_aligned());
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    let width = inner.width as usize;
    let mut lines = vec![Line::default()];
    let mut cursor = None;
    for &i in &visible {
        let field = &form.fields[i];
        let focused = i == form.focus;
        let label_style = if focused {
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(MUTED)
        };
        let mut left = vec![
            Span::styled(if focused { "▸ " } else { "  " }, Style::new().fg(ACCENT)),
            Span::styled(format!("{:<LABEL$} ", field.label), label_style),
        ];
        let right = if focused {
            vec![
                Span::styled(field.hint(), Style::new().fg(FAINT)),
                Span::raw(" "),
            ]
        } else {
            vec![]
        };
        let room = width.saturating_sub(LABEL + 3 + field.hint().width() + 6);
        let shown = tail(&field.value, room);
        match &field.kind {
            Kind::Text { placeholder, .. } if field.value.is_empty() => {
                left.push(Span::styled(
                    placeholder.clone(),
                    Style::new().fg(FAINT).add_modifier(Modifier::ITALIC),
                ));
                if focused {
                    cursor = Some((LABEL + 3, lines.len()));
                }
            }
            Kind::Text { .. } => {
                if focused {
                    cursor = Some((LABEL + 3 + shown.width(), lines.len()));
                }
                left.push(Span::raw(shown));
            }
            Kind::Choice(_) | Kind::Combo(_) => {
                let style = if field.value == "default" {
                    Style::new().fg(MUTED)
                } else {
                    Style::new()
                };
                if focused {
                    left.push(Span::styled("‹ ", Style::new().fg(FAINT)));
                    left.push(Span::styled(shown.clone(), style));
                    left.push(Span::styled(" ›", Style::new().fg(FAINT)));
                    if matches!(field.kind, Kind::Combo(_)) {
                        cursor = Some((LABEL + 5 + shown.width(), lines.len()));
                    }
                } else {
                    left.push(Span::styled(shown, style));
                }
                // What a model alias turned out to be, e.g. "opus · Opus 5.5".
                if let Some(note) = field.notes.get(&field.value).filter(|n| **n != field.value) {
                    left.push(Span::styled(format!("  {note}"), Style::new().fg(MUTED)));
                }
            }
            Kind::Info => left.push(Span::styled(shown, Style::new().fg(MUTED))),
        }
        lines.push(Line::from(fit(width, left, right)));
    }
    lines.push(Line::default());
    lines.extend(notes);
    f.render_widget(Paragraph::new(lines), inner);
    if let Some((x, y)) = cursor {
        f.set_cursor_position((inner.x + x as u16, inner.y + y as u16));
    }
}

/// The end of `s` if it's wider than `width`, with "…" in front.
fn tail(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for c in chars.iter().rev() {
        if out.width() + c.to_string().width() + 1 > width {
            break;
        }
        out.insert(0, *c);
    }
    format!("…{out}")
}

// Helpers

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let (width, height) = (width.min(area.width), height.min(area.height));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or(String::new(), |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// `s` cut to `width` columns, with "…" if it was longer.
fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if out.width() + c.to_string().width() + 1 > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// A line `width` columns wide: `left` cut to fit, `right` flush right.
fn fit(width: usize, left: Vec<Span<'static>>, right: Vec<Span<'static>>) -> Vec<Span<'static>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Keys;
    use crate::keys::Keymap;
    use crate::model::Project;
    use crate::model::tests::session;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app() -> App {
        let agents = vec![("claude".to_string(), true)];
        let mut app = App::new(Keymap::new(&Keys::default()).unwrap(), "/".into(), agents);
        let now = now_ms();
        let mut a = session("a1");
        a.name = "api".into();
        a.project = "roster".into();
        a.title = Some("Fix scrollback".into());
        a.state = State::NeedsInput;
        a.unread = true;
        a.model = Some("opus".into());
        a.effort = Some("high".into());
        a.state_since = Some(now - 120_000);
        let mut b = session("b1");
        b.name = "db".into();
        b.project = "roster".into();
        b.state = State::Working;
        b.state_since = Some(now - 200_000);
        app.set_data(
            vec![Project {
                id: 1,
                name: "roster".into(),
                origin: "/r".into(),
                agent: None,
                model: None,
                effort: None,
            }],
            vec![a, b],
        );
        app.selected = 1;
        app.preview = Text::from("agent screen line");
        app
    }

    #[test]
    fn shows_shelves_cards_header_and_hints() {
        let screen = render(&app(), 120, 16);
        assert!(screen.contains("1 need you · 1 working"), "{screen}");
        assert!(screen.contains("Needs you (1)"), "{screen}");
        assert!(screen.contains("RO Fix scrollback"), "{screen}");
        assert!(screen.contains("Input"), "{screen}");
        assert!(screen.contains("api · opus high"), "{screen}");
        assert!(screen.contains("Working (1)"), "{screen}");
        assert!(screen.contains("3m"), "{screen}");
        assert!(screen.contains("roster / Fix scrollback"), "{screen}");
        assert!(screen.contains("needs input"), "{screen}");
        assert!(screen.contains("agent screen line"), "{screen}");
        assert!(screen.contains("Enter attach"), "{screen}");
    }

    #[test]
    fn menu_popup_lists_its_keys() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let screen = render(&app, 120, 16);
        assert!(screen.contains(" session "), "{screen}");
        assert!(screen.contains("n new"), "{screen}");
        assert!(screen.contains("x stop"), "{screen}");
        assert!(screen.contains("d delete"), "{screen}");
    }

    #[test]
    fn help_lists_every_key() {
        let mut app = app();
        app.help = true;
        let screen = render(&app, 120, 40);
        assert!(screen.contains("Session menu (s …)"), "{screen}");
        assert!(screen.contains("s n  new"), "{screen}");
        assert!(screen.contains("C-q  back to roster"), "{screen}");
    }

    #[test]
    fn forms_show_fields_choices_and_hints() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        let screen = render(&app, 120, 24);
        assert!(screen.contains(" New session "), "{screen}");
        assert!(screen.contains("Project    roster"), "{screen}");
        assert!(screen.contains("▸ Name       automatic"), "{screen}");
        assert!(screen.contains("Effort     default"), "{screen}");
        assert!(
            screen.contains("optional: typed in once the agent is ready"),
            "{screen}"
        );
        assert!(screen.contains("Enter start · Esc cancel"), "{screen}");
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&app, 120, 24);
        assert!(screen.contains("▸ Agent      ‹ claude ›"), "{screen}");
        assert!(screen.contains("←→ change"), "{screen}");
        assert_eq!(tail("/a/very/long/path", 8), "…ng/path");
    }

    #[test]
    fn server_view_shows_the_report() {
        use crate::doctor::{Diagnosis, Level, Line};
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        let line = |level, text: &str| Line {
            level,
            text: text.into(),
        };
        app.set_report(Diagnosis {
            lines: vec![
                line(Level::Heading, "tmux sessions (1)"),
                line(Level::Problem, "  r-x  -  alive  ← orphan"),
            ],
            orphans: vec!["r-x".into()],
            problems: 1,
        });
        let screen = render(&app, 120, 12);
        assert!(screen.contains("tmux server and database"), "{screen}");
        assert!(screen.contains("tmux sessions (1)"), "{screen}");
        assert!(screen.contains("✗ r-x"), "{screen}");
        assert!(screen.contains("Enter kill orphans"), "{screen}");
        assert!(screen.contains("Esc back"), "{screen}");
        assert!(!screen.contains("Needs you"), "{screen}");
    }

    #[test]
    fn cards_and_forms_show_the_model_claude_reports() {
        let mut app = app();
        let mut sessions = app.sessions.clone();
        sessions[0].running_model = Some("claude-opus-5-5[1m]".into());
        app.set_data(app.projects.clone(), sessions);
        app.model_names = [("opus".to_string(), "Opus 5.5".to_string())].into();
        let screen = render(&app, 120, 16);
        assert!(screen.contains("api · Opus 5.5 (1M) high"), "{screen}");
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        // From the name: agent, then model.
        for _ in 0..2 {
            app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let screen = render(&app, 120, 24);
        assert!(screen.contains("‹ opus ›  Opus 5.5"), "{screen}");
    }

    #[test]
    fn long_titles_are_cut_with_an_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        let line: String = fit(
            10,
            vec![Span::raw("long title here")],
            vec![Span::raw("3m")],
        )
        .iter()
        .map(|s| s.content.to_string())
        .collect();
        assert_eq!(line, "long t… 3m");
    }
}
