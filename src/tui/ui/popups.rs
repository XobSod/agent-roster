//! Popups over the list: menus, help, dialogs, forms and the conversation picker.

use super::*;

/// Draws a popup's box over `rect`, titled, with `hint` at its bottom right
/// (none if empty); returns the area inside it.
fn popup(f: &mut Frame, rect: Rect, title: &str, hint: &str) -> Rect {
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MUTED))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().add_modifier(Modifier::BOLD),
        ));
    if !hint.is_empty() {
        block = block.title_bottom(
            Line::styled(format!(" {hint} "), Style::new().fg(MUTED)).right_aligned(),
        );
    }
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    inner
}

/// The open menu's keys, in a box at the bottom left.
pub(super) fn draw_menu(f: &mut Frame, app: &App, menu: Menu, area: Rect) {
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
                Span::raw(format!(" {}", action.describe_in(menu))),
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
    let inner = popup(f, rect, menu.name(), "Esc");
    f.render_widget(Paragraph::new(lines), inner);
}

/// Every key, by where it works.
pub(super) fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let keymap = &app.keymap;
    let key = |k: String| {
        Span::styled(
            format!("{k:>7}  "),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )
    };
    let title = |t: String| Line::styled(t, Style::new().add_modifier(Modifier::BOLD));
    // Sections: anywhere, each menu, attached.
    let mut sections = vec![vec![title("Anywhere".into())]];
    for (k, action) in keymap.top_items() {
        sections[0].push(Line::from(vec![
            key(k.to_string()),
            Span::raw(action.describe()),
        ]));
    }
    for menu in Menu::ALL {
        let Some(open) = keymap.label(Action::Open(menu)) else {
            continue;
        };
        let mut section = vec![title(format!(
            "{} menu ({open} …)",
            crate::util::capitalize(menu.name())
        ))];
        if menu == Menu::Project {
            section.push(Line::styled(
                format!("         in the projects view, without {open}"),
                Style::new().fg(MUTED),
            ));
        }
        for (k, action) in keymap.menu_items(menu) {
            section.push(Line::from(vec![
                key(format!("{open} {k}")),
                Span::raw(action.describe_in(menu)),
            ]));
        }
        sections.push(section);
    }
    sections.push(vec![
        title("Attached to an agent".into()),
        Line::from(vec![
            key(keymap.detach.clone()),
            Span::raw("back to roster (marks it read)"),
        ]),
    ]);
    // Sections one after the other, a blank line between them.
    let height = |sections: &[Vec<Line<'static>>]| {
        sections.iter().map(Vec::len).sum::<usize>() + sections.len().saturating_sub(1)
    };
    let joined = |sections: &[Vec<Line<'static>>]| -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (i, s) in sections.iter().enumerate() {
            if i > 0 {
                lines.push(Line::default());
            }
            lines.extend(s.iter().cloned());
        }
        lines
    };
    // A column's width, and the space between two.
    const COLUMN: u16 = 45;
    const GAP: u16 = 2;
    // Too tall for the terminal: two columns, split where they're most even.
    let two = (COLUMN + 1) * 2 + GAP;
    let columns = if height(&sections) as u16 + 2 > area.height && area.width >= two {
        let split = (1..sections.len())
            .min_by_key(|&i| height(&sections[..i]).abs_diff(height(&sections[i..])))
            .unwrap_or(1);
        vec![joined(&sections[..split]), joined(&sections[split..])]
    } else {
        vec![joined(&sections)]
    };
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(0) as u16;
    let n = columns.len() as u16;
    let width = COLUMN * n + GAP * (n - 1) + 2;
    let rect = centered(area, width, (tallest + 2).min(area.height));
    let inner = popup(f, rect, "keys", "any key closes");
    let areas = Layout::horizontal(vec![Constraint::Length(COLUMN); columns.len()])
        .spacing(GAP)
        .split(inner);
    for (column, area) in columns.into_iter().zip(areas.iter()) {
        f.render_widget(Paragraph::new(column), *area);
    }
}

pub(super) fn draw_dialog(f: &mut Frame, dialog: &Dialog, area: Rect) {
    let width = area.width.saturating_sub(4).min(76);
    match dialog {
        Dialog::Form(form) => draw_form(f, form, area),
        Dialog::Picker(picker) => draw_picker(f, picker, area),
        Dialog::Confirm { question, .. } => {
            let lines = wrapped_lines(question, width.saturating_sub(2) as usize);
            let rect = centered(area, width, lines as u16 + 4);
            let key = |k: &'static str| {
                Span::styled(k, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
            };
            let text = Text::from(vec![
                Line::from(question.clone()),
                Line::default(),
                Line::from(vec![key("y"), " yes    ".into(), key("n"), " no".into()]),
            ]);
            let inner = popup(f, rect, "confirm", "");
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), inner);
        }
        Dialog::Input { prompt, value, .. } => {
            let rect = centered(area, width, 5);
            let inner = popup(f, rect, "input", "Enter ok · Esc cancel");
            let [prompt_area, _, value_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            // Show the end of a value that's longer than the box.
            let room = inner.width.saturating_sub(3) as usize;
            let shown = tail(value, room);
            f.render_widget(Paragraph::new(prompt.clone()), prompt_area);
            f.render_widget(Paragraph::new(format!("> {shown}")), value_area);
            f.set_cursor_position((value_area.x + 2 + shown.width() as u16, value_area.y));
        }
    }
}

/// A form: one line per visible field, the focused one marked, with its hint on
/// the right; errors and notes below.
pub(super) fn draw_form(f: &mut Frame, form: &Form, area: Rect) {
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
    let hint = format!("Enter {} · Esc cancel", form.verb());
    let inner = popup(f, rect, &form.title, &hint);

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

/// Conversations to adopt: a filter line, one row per conversation (age, project,
/// title, and whether it's open in its agent or an archived session's), and where the
/// selected one ran.
pub(super) fn draw_picker(f: &mut Frame, picker: &Picker, area: Rect) {
    let now = now_ms();
    let shown = picker.shown();
    let rows = (area.height.saturating_sub(8) as usize).clamp(1, 14);
    // Filter, blank, the rows, blank, two lines about the selected one, borders.
    let height = (rows.min(shown.len().max(1)) + 7) as u16;
    let rect = centered(area, area.width.saturating_sub(4).min(100), height);
    let toggle = if picker.all {
        "Tab: only your projects"
    } else {
        "Tab: all"
    };
    let hint = format!("Enter adopt · {toggle} · Esc cancel");
    let inner = popup(f, rect, "Adopt a conversation", &hint);
    let width = inner.width as usize;

    let scope = match (picker.all, picker.outside()) {
        (true, _) => "all of Claude's conversations".to_string(),
        (false, 0) => "in your projects".to_string(),
        (false, n) => format!("in your projects ({n} more elsewhere)"),
    };
    let mut lines = vec![
        Line::from(fit(
            width,
            vec![
                Span::styled("› ", Style::new().fg(ACCENT)),
                Span::raw(picker.query.clone()),
            ],
            vec![Span::styled(scope, Style::new().fg(FAINT)), Span::raw(" ")],
        )),
        Line::default(),
    ];
    if shown.is_empty() {
        lines.push(Line::styled("  nothing matches", Style::new().fg(MUTED)));
    }
    let first = picker.selected.saturating_sub(rows - 1);
    let project_width = shown
        .iter()
        .map(|a| {
            a.project
                .as_ref()
                .map_or(a.new_project.width() + 1, |p| p.name.width())
        })
        .max()
        .unwrap_or(0)
        .min(18);
    for (i, a) in shown.iter().enumerate().skip(first).take(rows) {
        let c = &a.conversation;
        let selected = i == picker.selected;
        let (project, project_style) = match &a.project {
            Some(p) => (
                p.name.clone(),
                Style::new().fg(theme::project_color(&p.name)),
            ),
            None => (format!("+{}", a.new_project), Style::new().fg(MUTED)),
        };
        let what = c.label();
        let left = vec![
            Span::styled(if selected { "▸ " } else { "  " }, Style::new().fg(ACCENT)),
            Span::styled(
                format!("{:>4}  ", crate::util::ago(now - c.modified)),
                Style::new().fg(MUTED),
            ),
            glyph(&c.agent),
            Span::raw(" "),
            Span::styled(pad(&project, project_width) + "  ", project_style),
            Span::raw(what),
        ];
        let right = match (&a.archived, c.open_in) {
            (Some(_), _) => vec![Span::styled("archived session ", Style::new().fg(MUTED))],
            (None, Some(_)) => vec![Span::styled(
                format!("open in {} ", crate::util::capitalize(&c.agent)),
                Style::new().fg(INPUT),
            )],
            (None, None) => vec![],
        };
        let mut line = Line::from(fit(width, left, right));
        if selected {
            line = line.style(Style::new().bg(SELECTED_BG));
        }
        lines.push(line);
    }
    lines.push(Line::default());
    if let Some(a) = shown.get(picker.selected) {
        let c = &a.conversation;
        let branch = c
            .branch
            .as_ref()
            .map_or(String::new(), |b| format!(" ({b})"));
        let short_id: String = c.id.chars().take(8).collect();
        let place = match &a.project {
            Some(_) => String::new(),
            None => format!(" · new project {}", a.new_project),
        };
        lines.push(Line::styled(
            truncate(
                &format!("  {}{branch} · {short_id}{place}", c.cwd.display()),
                width,
            ),
            Style::new().fg(MUTED),
        ));
        if let Some(prompt) = &c.last_prompt
            && c.label() != crate::util::one_line(prompt)
        {
            let prompt = crate::util::one_line(prompt);
            lines.push(Line::styled(
                truncate(&format!("  last prompt: {prompt}"), width),
                Style::new().fg(FAINT),
            ));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    let query_width = picker.query.width().min(width.saturating_sub(3));
    f.set_cursor_position((inner.x + 2 + query_width as u16, inner.y));
}
