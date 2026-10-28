//! The sidebar: its title and filter line, and the list of shelves or projects with their sessions.

use super::*;

pub(super) fn draw_sidebar(f: &mut Frame, app: &App, area: Rect, now: i64) {
    let width = area.width as usize;
    // At the bottom, what the agents' accounts have used, once known.
    let usage = app
        .sidebar_usage
        .then(|| usage_line(app, width, now))
        .flatten();
    // Archived sessions too: what they left running.
    let over = (app.sessions.iter())
        .filter(|s| app.over_memory(s).is_some())
        .count();
    let [top, warning, sub, list, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(over > 0)),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(usage.is_some())),
    ])
    .areas(area);
    if let (true, Some(limit)) = (over > 0, app.memory_limit) {
        let sessions = if over == 1 { "session" } else { "sessions" };
        let text = format!(" ⚠ {over} {sessions} over {}", crate::procs::size(limit));
        let line = Line::from(fit(
            width,
            vec![Span::styled(text, Style::new().fg(ERROR))],
            Vec::new(),
        ));
        f.render_widget(Paragraph::new(line), warning);
    }
    if let Some(usage) = usage {
        f.render_widget(Paragraph::new(usage), bottom);
    }

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
            } => {
                let noted = match group {
                    Group::Project(id) => app.project(*id).is_some_and(|p| p.note.is_some()),
                    Group::Shelf(_) => false,
                };
                // Folded, a session in it using too much memory shows on it.
                let over = !*open
                    && (app.sessions.iter()).any(|s| {
                        crate::tui::list::group_of(s, app.grouped) == *group
                            && app.over_memory(s).is_some()
                    });
                vec![heading(*group, label, *count, *open, noted, over, content)]
            }
            Entry::Card(s) => {
                let noted = !app.grouped && project_noted(app, s);
                use crate::config::CardSubagents;
                let shown = app.subagents_shown(s, now);
                // How many, unless always unfolded: then the lines say it.
                let mark = match app.card_subagents {
                    CardSubagents::Count => subagents_mark(s, now, None),
                    CardSubagents::Folded => subagents_mark(s, now, Some(shown)),
                    CardSubagents::Unfolded => Vec::new(),
                };
                let mark = [memory_mark(app, s), mark].concat();
                let mut rows = card(s, hit(app, s), !app.grouped, noted, mark, content, now);
                if shown {
                    rows.extend(subagent_rows(app, s, content, now));
                }
                rows
            }
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

/// A group's heading: a project's (`noted`: with its note's mark) or a
/// shelf's, with how many it holds, and whether it's folded.
pub(super) fn heading(
    group: Group,
    label: &str,
    count: usize,
    open: bool,
    noted: bool,
    over_memory: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let chevron = Span::styled(if open { " ⌄ " } else { " › " }, Style::new().fg(MUTED));
    let chevron = match over_memory {
        true => vec![Span::styled(" ⚠", Style::new().fg(ERROR)), chevron],
        false => vec![chevron],
    };
    let chevron_width: usize = chevron.iter().map(Span::width).sum();
    let count = Span::styled(format!(" ({count}) "), Style::new().fg(MUTED));
    // A long name is cut, so its note's mark, the count and the chevron
    // always show.
    let room = width.saturating_sub(count.width() + chevron_width);
    let mut left = match group {
        Group::Project(_) => {
            let mark = match noted {
                true => vec![Span::styled(
                    format!(" {PROJECT_NOTE}"),
                    Style::new().fg(MUTED),
                )],
                false => Vec::new(),
            };
            let badge = badge(label);
            let mark_width: usize = mark.iter().map(Span::width).sum();
            let name = truncate(label, room.saturating_sub(badge.width() + 1 + mark_width));
            [vec![badge, Span::raw(" "), name.bold()], mark, vec![count]].concat()
        }
        Group::Shelf(_) => vec![
            Span::styled(
                truncate(label, room),
                Style::new().fg(MUTED).add_modifier(Modifier::BOLD),
            ),
            count,
        ],
    };
    let used: usize = left.iter().map(Span::width).sum::<usize>() + chevron_width;
    left.push(Span::styled(
        "─".repeat(width.saturating_sub(used)),
        Style::new().fg(FAINT),
    ));
    left.extend(chevron);
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

/// A session's lines: its project (badge and name, and `project_noted`, its
/// note's mark) unless the project's heading is right above; the title and
/// state; name, model and effort, age and agent, and its note's mark.
pub(super) fn card(
    s: &Session,
    hit: Option<&Hit>,
    with_project: bool,
    project_noted: bool,
    subagents: Vec<Span<'static>>,
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
            project_name(&s.project, &hit.project, project_noted, width),
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
        [
            subagents,
            note_mark(s),
            vec![Span::styled(
                format!("{} ", theme::ago(now - s.created_at)),
                Style::new().fg(FAINT),
            )],
        ]
        .concat(),
    );
    lines.extend([first, second]);
    lines
}

/// A line for each of `s`'s subagents at work, as they started: the model it
/// runs, what it's doing (its latest step; else what it was given to do),
/// cut to fit, and how long it's been at it.
fn subagent_rows(app: &App, s: &Session, width: usize, now: i64) -> Vec<Vec<Span<'static>>> {
    let mut running: Vec<_> = (s.subagents.iter())
        .filter(|(_, a)| now - a.heard <= crate::model::SUBAGENT_QUIET_MS)
        .collect();
    running.sort_by_key(|(id, a)| (a.started, *id));
    (running.into_iter())
        .map(|(id, a)| {
            let info = app.subagent_info.get(id);
            let what = (app.subagent_doing.get(id).cloned())
                .or_else(|| info.and_then(|i| i.task.clone()))
                .or_else(|| a.kind.clone())
                .unwrap_or_else(|| "a subagent".into());
            // Its parent's: the session's. Too narrow for it, what it's at
            // says more.
            let model = info.and_then(|i| i.model.as_deref()).and_then(|m| match m {
                "inherit" => s.running_model.as_deref(),
                m => Some(m),
            });
            let model = model
                .filter(|_| width >= 24)
                .and_then(crate::util::short_model);
            let mut left = vec![Span::raw("   "), Span::styled("↳ ", Style::new().fg(MUTED))];
            if let Some(model) = model {
                left.push(Span::styled(model, Style::new().fg(MUTED)));
                left.push(Span::styled(" · ", Style::new().fg(FAINT)));
            }
            left.push(Span::styled(
                crate::util::one_line(&what),
                Style::new().fg(MUTED),
            ));
            let right = vec![Span::styled(
                format!("{} ", theme::clock(now - a.started)),
                Style::new().fg(FAINT),
            )];
            fit(width, left, right)
        })
        .collect()
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
    let right = [memory_mark(app, s), note_mark(s), right].concat();
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
        // The name fits what the right side and the agent's mark leave
        // (`fit` keeps a space before the right side), so its note's mark
        // isn't cut off after it; the title goes first.
        let right_width: usize = right.iter().map(Span::width).sum();
        let mark_width: usize = mark.iter().map(Span::width).sum();
        let room = width.saturating_sub(mark_width + right_width + 1);
        let noted = project_noted(app, s);
        [
            mark,
            project_name(&s.project, &hit.project, noted, room),
            vec![slash],
            title,
        ]
        .concat()
    };
    fit(width, left, right)
}

/// How much memory `s`'s processes use, if more than they should.
pub(super) fn memory_mark(app: &App, s: &Session) -> Vec<Span<'static>> {
    match app.over_memory(s) {
        Some(memory) => vec![Span::styled(
            format!("⚠ {} ", crate::procs::size(memory)),
            Style::new().fg(ERROR),
        )],
        None => Vec::new(),
    }
}

fn note_mark(s: &Session) -> Vec<Span<'static>> {
    match s.note {
        Some(_) => vec![Span::styled("✎ ", Style::new().fg(MUTED))],
        None => Vec::new(),
    }
}

fn project_noted(app: &App, s: &Session) -> bool {
    app.project(s.project_id).is_some_and(|p| p.note.is_some())
}
