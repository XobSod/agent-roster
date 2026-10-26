//! The full-width views: projects, and the tmux server and database report.

use super::*;

/// Every project: name, directory, sessions, defaults, whether it has a note;
/// the selected one's details below, or its note open in the editor.
pub(super) fn draw_projects(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let below = match &app.note {
        Some(open) => editor_height(open.shown(), area.width, area.height),
        None => 8,
    };
    let [head, _, table, details] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(below),
    ])
    .areas(area);
    let count = match app.projects.len() {
        1 => "1 project".to_string(),
        n => format!("{n} projects"),
    };
    let title = vec![
        Span::raw(" "),
        "roster".bold(),
        Span::styled("  projects", Style::new().fg(MUTED)),
    ];
    f.render_widget(
        Paragraph::new(Line::from(fit(
            width,
            title,
            vec![Span::styled(count, Style::new().fg(MUTED)), Span::raw(" ")],
        ))),
        head,
    );
    if app.projects.is_empty() {
        f.render_widget(Paragraph::new(app.empty_info()), table);
        return;
    }

    // Columns: badge and name, directory, sessions, defaults (and the env file).
    let rows: Vec<[String; 3]> = app
        .projects
        .iter()
        .map(|p| {
            let mut defaults = p.defaults_label(app.default_agent(), &app.models.names);
            if p.origin.join(crate::paths::PROJECT_ENV).exists() {
                defaults.push_str(" · env file");
            }
            if app.chat_project == Some(p.id) {
                defaults.push_str(" · built-in, for chats");
            }
            [
                crate::util::short_path(&p.origin),
                p.sessions_label(&app.sessions),
                defaults,
            ]
        })
        .collect();
    let widest = |of: &dyn Fn(usize) -> usize, cap: usize| {
        (0..app.projects.len()).map(of).max().unwrap_or(0).min(cap)
    };
    // A name and its note's mark.
    let named = |p: &crate::model::Project| p.name.width() + if p.note.is_some() { 2 } else { 0 };
    let name_w = widest(&|i| named(&app.projects[i]), 24).max(7);
    let dir_w = widest(&|i| rows[i][0].width(), width * 2 / 5).max(9);
    let sessions_w = widest(&|i| rows[i][1].width(), 24).max(8);
    let pad = |s: &str, w: usize| pad(s, w) + "  ";
    let header = Line::styled(
        format!(
            "    {}{}{}DEFAULTS",
            pad("PROJECT", name_w),
            pad("DIRECTORY", dir_w),
            pad("SESSIONS", sessions_w)
        ),
        Style::new().fg(FAINT),
    );
    let height = (table.height as usize).saturating_sub(1);
    let first = app.project_row.saturating_sub(height.saturating_sub(1));
    let mut lines = vec![header];
    for (i, p) in app.projects.iter().enumerate().skip(first).take(height) {
        let [dir, sessions, defaults] = &rows[i];
        // Its badge and a space before the name.
        let mut content = project_name(&p.name, &[], p.note.is_some(), name_w + 3);
        let used: usize = content.iter().map(Span::width).sum();
        content.extend([
            Span::raw(" ".repeat(name_w + 5 - used)),
            Span::styled(pad(dir, dir_w), Style::new().fg(MUTED)),
            Span::raw(pad(sessions, sessions_w)),
            Span::styled(defaults.clone(), Style::new().fg(MUTED)),
        ]);
        let content = fit(width.saturating_sub(1), content, Vec::new());
        lines.push(framed(content, i == app.project_row, width));
    }
    f.render_widget(Paragraph::new(lines), table);

    if let Some(open) = &app.note {
        draw_note_editor(f, open, details);
    } else if let Some(p) = app.projects.get(app.project_row) {
        let mut text = vec![Line::styled(
            format!(" {}", p.name),
            Style::new().add_modifier(Modifier::BOLD),
        )];
        text.extend(app.project_info(p).lines.into_iter().map(|mut l| {
            l.spans.insert(0, Span::raw(" "));
            l
        }));
        f.render_widget(Paragraph::new(text).style(Style::new().fg(MUTED)), details);
    }
}

/// The tmux server and database report, full width.
pub(super) fn draw_server(f: &mut Frame, app: &App, area: Rect) {
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

/// Every setting, full width: by section, each with its value (yours, or the
/// default, dimmed) and what it does; the selected one's details below.
pub(super) fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let [head, _, table, details] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(area);
    let all = app.all_settings();
    let yours = all.iter().filter(|s| s.set).count();
    let title = vec![
        Span::raw(" "),
        "roster".bold(),
        Span::styled("  settings", Style::new().fg(MUTED)),
    ];
    let count = match yours {
        0 => "all defaults".to_string(),
        1 => "1 of yours".to_string(),
        n => format!("{n} of yours"),
    };
    f.render_widget(
        Paragraph::new(Line::from(fit(
            width,
            title,
            vec![Span::styled(count, Style::new().fg(MUTED)), Span::raw(" ")],
        ))),
        head,
    );

    let name_w = all.iter().map(|s| s.name().width()).max().unwrap_or(0);
    let value_w = all
        .iter()
        .map(|s| s.value.width())
        .max()
        .unwrap_or(0)
        .clamp(8, 24);
    // Headings and settings, the row of each setting noted to keep it in view.
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    let mut i = 0;
    for section in &app.settings {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let heading = match &section.agent {
            Some((name, note)) => vec![
                Span::raw("   "),
                glyph(name),
                Span::raw(" "),
                name.clone().bold(),
                Span::styled(
                    if note.is_empty() {
                        String::new()
                    } else {
                        format!("  {note}")
                    },
                    Style::new().fg(MUTED),
                ),
            ],
            None => vec![Span::raw("   "), section.title.clone().bold()],
        };
        lines.push(Line::from(heading));
        for s in &section.settings {
            let style = if s.set {
                Style::new()
            } else {
                Style::new().fg(MUTED)
            };
            let content = vec![
                Span::raw("     "),
                Span::styled(pad(s.name(), name_w) + "  ", style),
                Span::styled(
                    pad(&s.value, value_w) + "  ",
                    if s.set {
                        style.add_modifier(Modifier::BOLD)
                    } else {
                        style
                    },
                ),
                Span::styled(s.doc.clone(), Style::new().fg(FAINT)),
            ];
            let content = fit(width.saturating_sub(1), content, Vec::new());
            rows.push(lines.len());
            lines.push(framed(content, i == app.setting_row, width));
            i += 1;
        }
    }
    // The selected setting in view, its heading too when there's room.
    let height = table.height as usize;
    let at = rows.get(app.setting_row).copied().unwrap_or(0);
    let first = at.saturating_sub(height.saturating_sub(2));
    let first = first.min(lines.len().saturating_sub(height));
    let shown: Vec<Line> = lines.into_iter().skip(first).take(height).collect();
    f.render_widget(Paragraph::new(shown), table);

    if let Some(s) = app.selected_setting() {
        let from = if s.set {
            format!("in {}", crate::util::short_path(&app.config_file))
        } else {
            "the default: config.toml doesn't set it".to_string()
        };
        let text = vec![
            Line::from(vec![Span::raw(" "), s.dotted().bold()]),
            Line::styled(format!(" {}", from), Style::new().fg(MUTED)),
        ];
        f.render_widget(Paragraph::new(text), details);
    }
}
