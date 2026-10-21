//! Drawing: the session list (shelves or projects) on the left, a header and the
//! selected agent's screen on the right, a footer with hints, and popups on top.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::app::{App, Dialog, Mode};
use super::form::{Form, Kind};
use super::list::{Entry, Group};
use super::picker::Picker;
use super::theme::{
    self, ACCENT, CLAUDE, ERROR, FAINT, INPUT, MUTED, SELECTED_BG, UNREAD, WORKING,
};
use crate::keys::{Action, Menu};
use crate::model::{Session, State};
use crate::search::Hit;
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
    } else if app.mode == Mode::Projects {
        draw_projects(f, app, main);
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

/// Where the text filter matched `s`, if it's on.
fn hit<'a>(app: &'a App, s: &Session) -> Option<&'a Hit> {
    app.hits.as_ref()?.get(&s.id)
}

/// `text` in `style`, with the characters at `marks` (indices of graphemes, in
/// order) highlighted, as matched by the text filter.
fn marked(text: &str, marks: &[usize], style: Style) -> Vec<Span<'static>> {
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
fn card(
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
        [
            vec![Span::raw("   ")],
            marked(&s.name, &hit.name, meta_style),
            vec![Span::styled(meta(s), meta_style)],
        ]
        .concat(),
        vec![
            Span::styled(
                format!("{} ", theme::ago(now - s.created_at)),
                Style::new().fg(FAINT),
            ),
            glyph(&s.agent),
            Span::raw(" "),
        ],
    );
    lines.extend([first, second]);
    lines
}

/// A stopped or archived session's line: project / title and its state.
fn row(app: &App, s: &Session, width: usize, now: i64) -> Vec<Span<'static>> {
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
    let left = if app.grouped && s.deleted_at.is_none() {
        [vec![Span::raw("   ")], title].concat()
    } else {
        let slash = Span::styled(" / ", Style::new().fg(FAINT));
        [project_name(&s.project, &hit.project), vec![slash], title].concat()
    };
    fit(width, left, right)
}

/// The project's badge and name, in its colour (`marks` matched the filter).
fn project_name(project: &str, marks: &[usize]) -> Vec<Span<'static>> {
    let style = Style::new().fg(theme::project_color(project));
    [
        vec![badge(project), Span::raw(" ")],
        marked(project, marks, style),
    ]
    .concat()
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

/// The card's second line after the name: model and effort (and the agent, if
/// not Claude).
fn meta(s: &Session) -> String {
    let mut meta = String::new();
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
    let [head, stats, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(header(app, head.width as usize, now))),
        head,
    );
    // The session's statistics: what roster counts and what the agent can tell.
    if let Some(s) = app.selected_session().filter(|s| s.deleted_at.is_none()) {
        let parts = crate::stats::summary(s, app.stats.as_ref(), now);
        let line = truncate(&parts.join(" · "), stats.width as usize);
        f.render_widget(
            Paragraph::new(Line::styled(line, Style::new().fg(MUTED))),
            stats,
        );
    }
    // The bottom of an agent's screen is where its prompt is; long lines are cut
    // (agents drawing full screen get this size, so theirs fit).
    app.preview_size.set(Some((body.width, body.height)));
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
                right.push(Span::styled("archived", Style::new().fg(MUTED)));
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

/// The key hints, and the latest message flush right, so the hints stay in
/// sight. Where the two would meet, the last hints give way, whole.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let message: Vec<Span> = app
        .message()
        .map(|m| {
            let style = Style::new().fg(if m.error { ERROR } else { INPUT });
            vec![
                Span::styled(truncate(&m.text, width.saturating_sub(2)), style),
                Span::raw(" "),
            ]
        })
        .unwrap_or_default();
    // A gap of three columns before the message, as between hints.
    let room = match message.iter().map(Span::width).sum::<usize>() {
        0 => width,
        w => width.saturating_sub(w + 3),
    };
    let mut hints = vec![Span::raw(" ")];
    let mut used = 1;
    for (i, (key, what)) in app.hints().into_iter().enumerate() {
        let gap = if i == 0 { 0 } else { 3 };
        let w = gap + key.width() + 1 + what.width();
        if used + w > room {
            break;
        }
        used += w;
        hints.push(Span::raw(" ".repeat(gap)));
        hints.push(Span::styled(key, Style::new().add_modifier(Modifier::BOLD)));
        hints.push(Span::styled(format!(" {what}"), Style::new().fg(MUTED)));
    }
    if used == 1 {
        hints.clear();
    }
    f.render_widget(Paragraph::new(Line::from(fit(width, hints, message))), area);
}

/// The tmux server and database report, full width.
/// Every project: name, directory, sessions, defaults; the selected one's
/// details below.
fn draw_projects(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let [head, _, table, details] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(7),
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
            let mut defaults = p.defaults_label(&app.model_names);
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
    let name_w = widest(&|i| app.projects[i].name.width(), 24).max(7);
    let dir_w = widest(&|i| rows[i][0].width(), width * 2 / 5).max(9);
    let sessions_w = widest(&|i| rows[i][1].width(), 24).max(8);
    let pad = |s: &str, w: usize| format!("{:<w$}  ", truncate(s, w), w = w);
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
        let content = vec![
            badge(&p.name),
            Span::raw(" "),
            Span::styled(
                pad(&p.name, name_w),
                Style::new().fg(theme::project_color(&p.name)),
            ),
            Span::styled(pad(dir, dir_w), Style::new().fg(MUTED)),
            Span::raw(pad(sessions, sessions_w)),
            Span::styled(defaults.clone(), Style::new().fg(MUTED)),
        ];
        let content = fit(width.saturating_sub(1), content, Vec::new());
        lines.push(framed(content, i == app.project_row, width));
    }
    f.render_widget(Paragraph::new(lines), table);

    if let Some(p) = app.projects.get(app.project_row) {
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
            capitalize(menu.name())
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
    const COLUMN: u16 = 46;
    let one = joined(&sections);
    // Too tall for the terminal: two columns, split where they're most even.
    let columns = if one.len() as u16 + 2 > area.height && area.width > COLUMN * 2 {
        let split = (1..sections.len())
            .min_by_key(|&i| {
                joined(&sections[..i])
                    .len()
                    .abs_diff(joined(&sections[i..]).len())
            })
            .unwrap_or(1);
        vec![joined(&sections[..split]), joined(&sections[split..])]
    } else {
        vec![one]
    };
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(0) as u16;
    let width = COLUMN * columns.len() as u16 + (columns.len() as u16 - 1);
    let rect = centered(area, width, (tallest + 2).min(area.height));
    let block = popup_block("keys")
        .title_bottom(Line::styled(" any key closes ", Style::new().fg(MUTED)).right_aligned());
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    let areas = Layout::horizontal(vec![Constraint::Length(COLUMN - 1); columns.len()])
        .spacing(2)
        .split(inner);
    for (column, area) in columns.into_iter().zip(areas.iter()) {
        f.render_widget(Paragraph::new(column), *area);
    }
}

fn draw_dialog(f: &mut Frame, dialog: &Dialog, area: Rect) {
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

/// Conversations to adopt: a filter line, one row per conversation (age, project,
/// title, and whether it's open in Claude or a deleted session's), and where the
/// selected one ran.
fn draw_picker(f: &mut Frame, picker: &Picker, area: Rect) {
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
    let hint = format!(" Enter adopt · {toggle} · Esc cancel ");
    let block = popup_block("Adopt a conversation")
        .title_bottom(Line::styled(hint, Style::new().fg(MUTED)).right_aligned());
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
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
            Span::styled(
                format!("{:<project_width$}  ", truncate(&project, project_width)),
                project_style,
            ),
            Span::raw(what),
        ];
        let right = match (&a.archived, c.open_in) {
            (Some(_), _) => vec![Span::styled("archived session ", Style::new().fg(MUTED))],
            (None, Some(_)) => vec![Span::styled("open in Claude ", Style::new().fg(INPUT))],
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
            && c.label() != crate::adopt::one_line(prompt)
        {
            let prompt = crate::adopt::one_line(prompt);
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

/// How many lines `text` takes wrapped at word boundaries to `width`.
fn wrapped_lines(text: &str, width: usize) -> usize {
    let mut lines = 1;
    let mut used = 0;
    for word in text.split_whitespace() {
        let w = word.width();
        if used > 0 && used + 1 + w > width {
            lines += 1;
            used = w;
        } else {
            used += w + usize::from(used > 0);
        }
    }
    lines
}

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
        a.created_at = now - 2 * 86_400_000;
        a.name = "api".into();
        a.project = "roster".into();
        a.title = Some("Fix scrollback".into());
        a.state = State::NeedsInput;
        a.unread = true;
        a.model = Some("opus".into());
        a.effort = Some("high".into());
        a.state_since = Some(now - 120_000);
        let mut b = session("b1");
        b.created_at = now - 2 * 86_400_000;
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
        assert!(screen.contains("1 needs input · 1 working"), "{screen}");
        assert!(screen.contains("Needs input (1)"), "{screen}");
        // Without project headings, each card names its project first.
        assert!(screen.contains("RO roster"), "{screen}");
        assert!(screen.contains("   Fix scrollback"), "{screen}");
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
    fn messages_sit_right_of_the_hints() {
        let mut app = app();
        app.flash("Started roster/api; Enter attaches");
        let screen = render(&app, 120, 16);
        let footer = screen.lines().last().unwrap();
        assert!(footer.starts_with(" Enter attach   s session"), "{footer}");
        assert!(
            footer.ends_with("   Started roster/api; Enter attaches "),
            "{footer}"
        );
        // Where they'd meet, the last hints give way, whole.
        let screen = render(&app, 70, 16);
        let footer = screen.lines().last().unwrap();
        assert!(
            footer.starts_with(" Enter attach   s session   "),
            "{footer}"
        );
        assert!(!footer.contains("help"), "{footer}");
        assert!(
            footer.ends_with(" Started roster/api; Enter attaches "),
            "{footer}"
        );
        // Longer than the line: only the message, cut.
        let screen = render(&app, 30, 16);
        let footer = screen.lines().last().unwrap();
        assert_eq!(footer, " Started roster/api; Enter a… ");
    }

    #[test]
    fn the_text_filter_shows_under_the_title_and_marks_what_matched() {
        let mut app = app();
        for c in "/fix".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 16)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let line = |y: u16| -> String { (0..40).map(|x| buffer[(x, y)].symbol()).collect() };
        assert!(line(1).starts_with(" / fix "), "{}", line(1));
        assert!(line(1).contains(" 1 of 2 │"), "{}", line(1));
        let y = (0..16)
            .find(|&y| line(y).contains("Fix scrollback"))
            .unwrap();
        let x = (0..40).find(|&x| buffer[(x, y)].symbol() == "F").unwrap();
        let lit = |x| buffer[(x, y)].modifier.contains(Modifier::UNDERLINED);
        assert!(lit(x) && lit(x + 1) && lit(x + 2) && !lit(x + 3));
        assert!(!render(&app, 120, 16).contains("Working"));
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(render(&app, 120, 16).contains("nothing matches"));
    }

    #[test]
    fn grouped_cards_leave_the_project_to_its_heading() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        let screen = render(&app, 120, 16);
        // The heading and the header; no project line in the cards.
        assert_eq!(screen.matches("RO roster").count(), 2, "{screen}");
        assert!(screen.contains("RO roster (2)"), "{screen}");
        assert!(screen.contains("   Fix scrollback"), "{screen}");
    }

    #[test]
    fn stopped_rows_name_their_project_too() {
        let mut app = app();
        let mut sessions = app.sessions.clone();
        sessions[1].state = State::Stopped;
        app.set_data(app.projects.clone(), sessions);
        // Unfold the Stopped shelf.
        let stopped = app.entries.len() - 1;
        app.selected = stopped;
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        let screen = render(&app, 120, 16);
        assert!(screen.contains("RO roster / db"), "{screen}");
    }

    #[test]
    fn working_and_archived_sit_at_the_bottom_while_all_fits() {
        let mut app = app();
        let mut sessions = app.sessions.clone();
        let mut archived = session("c1");
        archived.project = "roster".into();
        archived.title = Some("Old work".into());
        archived.deleted_at = Some(now_ms() - 3 * 86_400_000);
        sessions.push(archived);
        app.set_data(app.projects.clone(), sessions);
        let screen = render(&app, 120, 20);
        let lines: Vec<&str> = screen.lines().collect();
        let row = |text: &str| lines.iter().position(|l| l.contains(text)).unwrap();
        // The footer is the last line; Archived (folded) right above it.
        assert_eq!(row("Archived (1)"), lines.len() - 2, "{screen}");
        assert!(row("Working (1)") > row("api · opus high") + 2, "{screen}");
        // Unfolded: project / title, and when it was archived.
        app.selected = app.entries.len() - 1;
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        let screen = render(&app, 120, 20);
        assert!(screen.contains("RO roster / Old work"), "{screen}");
        assert!(screen.contains("archived 3d"), "{screen}");
        // Too long to fit: no gap, it scrolls (here: to the top).
        app.selected = 0;
        let screen = render(&app, 120, 10);
        assert!(screen.contains("Needs input (1)"), "{screen}");
        assert!(!screen.contains("Archived"), "{screen}");
    }

    #[test]
    fn the_projects_view_shows_each_project() {
        let mut app = app();
        app.model_names = [("opus".to_string(), "Opus 5.5".to_string())].into();
        let mut projects = app.projects.clone();
        projects[0].model = Some("opus".into());
        projects.push(Project {
            id: 2,
            name: "scratch".into(),
            origin: "/tmp/scratch".into(),
            agent: None,
            model: None,
            effort: None,
        });
        app.set_data(projects, app.sessions.clone());
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        let screen = render(&app, 120, 20);
        assert!(screen.contains("roster  projects"), "{screen}");
        assert!(screen.contains("2 projects"), "{screen}");
        assert!(screen.contains("PROJECT"), "{screen}");
        assert!(screen.contains("RO roster"), "{screen}");
        assert!(screen.contains("2 live"), "{screen}");
        assert!(screen.contains("claude · opus (Opus 5.5)"), "{screen}");
        assert!(screen.contains("/tmp/scratch"), "{screen}");
        assert!(screen.contains("none"), "{screen}");
        // The selected one's details, and the keys that act here.
        assert!(screen.contains("origin    /r"), "{screen}");
        // Its project keys work without "p".
        assert!(
            screen.contains("Enter show its sessions   n new   e edit   d remove   s session"),
            "{screen}"
        );
        assert!(screen.contains("Esc back"), "{screen}");
    }

    #[test]
    fn menu_popup_lists_its_keys() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let screen = render(&app, 120, 16);
        assert!(screen.contains(" session "), "{screen}");
        assert!(screen.contains("n new"), "{screen}");
        assert!(screen.contains("x stop"), "{screen}");
        assert!(screen.contains("d archive / restore"), "{screen}");
        assert!(screen.contains("D delete for good"), "{screen}");
    }

    #[test]
    fn the_picker_shows_conversations_and_where_they_ran() {
        use crate::tui::picker::tests::adoptable;
        let mut app = app();
        let mut open = adoptable("c2", "Busy elsewhere", Some("alpha"));
        open.conversation.open_in = Some(42);
        open.conversation.last_prompt = Some("and the\ntests".into());
        app.pick_conversation(vec![open, adoptable("c1", "Fix the parser", None)]);
        let screen = render(&app, 120, 30);
        assert!(screen.contains("Adopt a conversation"), "{screen}");
        assert!(
            screen.contains("in your projects (1 more elsewhere)"),
            "{screen}"
        );
        assert!(screen.contains("Busy elsewhere"), "{screen}");
        assert!(screen.contains("open in Claude"), "{screen}");
        assert!(!screen.contains("Fix the parser"), "{screen}");
        assert!(screen.contains("/alpha · c2"), "{screen}");
        assert!(screen.contains("last prompt: and the tests"), "{screen}");
        assert!(
            screen.contains("Enter adopt · Tab: all · Esc cancel"),
            "{screen}"
        );
        // Tab: the rest, with the project each would get.
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(
            screen.contains("+elsewhere") && screen.contains("Fix the parser"),
            "{screen}"
        );
        // Picked: the form, with what moving it means.
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("Adopting   Busy elsewhere"), "{screen}");
        assert!(
            screen.contains("it's open in Claude (pid 42), close it there"),
            "{screen}"
        );
        assert!(screen.contains("Enter adopt"), "{screen}");
    }

    #[test]
    fn long_questions_wrap_and_keep_their_keys() {
        assert_eq!(wrapped_lines("one two three", 9), 2);
        assert_eq!(wrapped_lines("one two", 7), 1);
        let mut app = app();
        app.dialog = Some(Dialog::Confirm {
            question: "word ".repeat(40),
            op: super::super::app::Op::Quit,
        });
        let screen = render(&app, 80, 30);
        assert!(screen.contains("y yes    n no"), "{screen}");
    }

    #[test]
    fn help_lists_every_key() {
        let mut app = app();
        app.help = true;
        // Tall enough: one column. Not: two, with nothing cut off.
        for height in [60, 30] {
            let screen = render(&app, 120, height);
            assert!(screen.contains("Session menu (s …)"), "{screen}");
            assert!(screen.contains("s n  new"), "{screen}");
            assert!(screen.contains("v p  projects"), "{screen}");
            assert!(screen.contains("p l  list"), "{screen}");
            assert!(
                screen.contains("in the projects view, without p"),
                "{screen}"
            );
            assert!(screen.contains("C-q  back to roster"), "{screen}");
        }
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
        app.stats = Some(crate::stats::AgentStats {
            prompts: Some(5),
            output_tokens: Some(92_000),
            ..Default::default()
        });
        let screen = render(&app, 120, 16);
        assert!(
            screen.contains("api · Opus 5.5 (1M) high") && screen.contains("2d ✱"),
            "{screen}"
        );
        assert!(
            screen.contains("created 2d · 5 prompts · 92k out"),
            "{screen}"
        );
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
