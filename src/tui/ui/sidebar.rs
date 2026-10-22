//! The sidebar: its title and filter line, and the list of shelves or projects with their sessions.

use super::*;

pub(super) fn draw_sidebar(f: &mut Frame, app: &App, area: Rect, now: i64) {
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
        let verb = if needs == 1 { "needs" } else { "need" };
        summary.push(Span::styled(
            format!("{needs} {verb} input"),
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

    // The filters: by project and by text, and how many match.
    let mut filters = Vec::new();
    if let Some(p) = app.filter.and_then(|id| app.project(id)) {
        filters.push(Span::styled(
            format!(" only {}", p.name),
            Style::new().fg(MUTED).italic(),
        ));
    }
    let mut count = Vec::new();
    if app.typing.is_some() || !app.search.is_empty() {
        if !filters.is_empty() {
            filters.push(Span::styled(" ·", Style::new().fg(FAINT)));
        }
        filters.push(Span::styled(" / ", Style::new().fg(ACCENT)));
        filters.push(Span::raw(app.search.clone()));
        if let Some(hits) = &app.hits {
            let total = app
                .sessions
                .iter()
                .filter(|s| app.filter.is_none_or(|p| s.project_id == p))
                .count();
            count.push(Span::styled(
                format!("{} of {total} ", hits.len()),
                Style::new().fg(MUTED),
            ));
        }
    }
    let typed = filters.iter().map(Span::width).sum::<usize>();
    f.render_widget(Paragraph::new(Line::from(fit(width, filters, count))), sub);
    if app.typing.is_some() {
        let x = typed.min(width.saturating_sub(1)) as u16;
        f.set_cursor_position((sub.x + x, sub.y));
    }

    let (mut lines, mut spans) = list_lines(app, width, now);
    if lines.is_empty() && app.hits.is_some() {
        lines.push(Line::styled("  nothing matches", Style::new().fg(MUTED)));
    }
    let height = list.height as usize;
    // What's handed off (working) or finished (archived) sits at the bottom
    // while everything fits.
    if lines.len() < height
        && let Some(first) = app.entries.iter().position(Entry::at_bottom)
    {
        let gap = height - lines.len();
        let at = spans[first].0;
        lines.splice(at..at, std::iter::repeat_n(Line::default(), gap));
        for span in &mut spans[first..] {
            span.0 += gap;
        }
    }
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
pub(super) fn list_lines(
    app: &App,
    width: usize,
    now: i64,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
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
            Entry::Card(s) => card(s, hit(app, s), !app.grouped, content, now),
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
pub(super) fn framed(content: Vec<Span<'static>>, selected: bool, width: usize) -> Line<'static> {
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

pub(super) fn heading(
    group: Group,
    label: &str,
    count: usize,
    open: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let chevron = Span::styled(if open { " ⌄ " } else { " › " }, Style::new().fg(MUTED));
    let count = Span::styled(format!(" ({count}) "), Style::new().fg(MUTED));
    // A long name is cut, so the count and the chevron always show.
    let room = width.saturating_sub(count.width() + chevron.width());
    let mut left = match group {
        Group::Project(_) => {
            let badge = badge(label);
            let name = truncate(label, room.saturating_sub(badge.width() + 1));
            vec![badge, Span::raw(" "), name.bold(), count]
        }
        Group::Shelf(_) => vec![
            Span::styled(
                truncate(label, room),
                Style::new().fg(MUTED).add_modifier(Modifier::BOLD),
            ),
            count,
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

/// Where the text filter matched `s`, if it's on.
pub(super) fn hit<'a>(app: &'a App, s: &Session) -> Option<&'a Hit> {
    app.hits.as_ref()?.get(&s.id)
}

/// `text` in `style`, with the characters at `marks` (indices of graphemes, in
/// order) highlighted, as matched by the text filter.
pub(super) fn marked(text: &str, marks: &[usize], style: Style) -> Vec<Span<'static>> {
    let lit = style
        .fg(ACCENT)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    for (i, g) in text.graphemes(true).enumerate() {
        let is_lit = marks.binary_search(&i).is_ok();
        if is_lit != run_lit && !run.is_empty() {
            let s = if run_lit { lit } else { style };
            spans.push(Span::styled(std::mem::take(&mut run), s));
        }
        run_lit = is_lit;
        run.push_str(g);
    }
    if !run.is_empty() || spans.is_empty() {
        spans.push(Span::styled(run, if run_lit { lit } else { style }));
    }
    spans
}

/// A session's lines: its project (badge and name) unless the project's heading
/// is right above; the title and state; name, model and effort, age and agent.
pub(super) fn card(
    s: &Session,
    hit: Option<&Hit>,
    with_project: bool,
    width: usize,
    now: i64,
) -> Vec<Vec<Span<'static>>> {
    let none = Hit::default();
    let hit = hit.unwrap_or(&none);
    let working = s.state == State::Working;
    let title_style = match (s.state, s.unread) {
        (State::NeedsInput, _) | (State::Idle, true) => Style::new().add_modifier(Modifier::BOLD),
        (State::Working, _) => Style::new().fg(MUTED),
        _ => Style::new(),
    };
    let meta_color = if working { FAINT } else { MUTED };
    let mut lines = Vec::new();
    if with_project {
        lines.push(fit(
            width,
            project_name(&s.project, &hit.project),
            Vec::new(),
        ));
    }
    let first = fit(
        width,
        [
            vec![Span::raw("   ")],
            marked(s.display_title(), &hit.title, title_style),
        ]
        .concat(),
        [status(s, now), vec![Span::raw(" ")]].concat(),
    );
    let meta_style = Style::new().fg(meta_color);
    let second = fit(
        width,
        [vec![Span::raw("   ")], details(s, &hit.name, meta_style)].concat(),
        vec![Span::styled(
            format!("{} ", theme::ago(now - s.created_at)),
            Style::new().fg(FAINT),
        )],
    );
    lines.extend([first, second]);
    lines
}

/// A stopped or archived session's line: its agent's mark, project / title,
/// and its state.
pub(super) fn row(app: &App, s: &Session, width: usize, now: i64) -> Vec<Span<'static>> {
    let right = match s.deleted_at {
        Some(at) => vec![Span::styled(
            format!("archived {} ", theme::ago(now - at)),
            Style::new().fg(FAINT),
        )],
        None => [status(s, now), vec![Span::raw(" ")]].concat(),
    };
    let none = Hit::default();
    let hit = hit(app, s).unwrap_or(&none);
    let title = marked(s.display_title(), &hit.title, Style::new().fg(MUTED));
    // Grouped by project, the heading right above says it; archived sessions
    // come from every project.
    // Its agent's mark first, as on a card's details.
    let left = if app.grouped && s.deleted_at.is_none() {
        [
            vec![Span::raw("   "), glyph(&s.agent), Span::raw(" ")],
            title,
        ]
        .concat()
    } else {
        let slash = Span::styled(" / ", Style::new().fg(FAINT));
        let mark = vec![glyph(&s.agent), Span::raw(" ")];
        [
            mark,
            project_name(&s.project, &hit.project),
            vec![slash],
            title,
        ]
        .concat()
    };
    fit(width, left, right)
}
