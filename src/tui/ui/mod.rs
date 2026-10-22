//! Drawing: the session list (shelves or projects) on the left, a header and the
//! selected agent's screen on the right, a footer with hints, and popups on top.

mod popups;
mod sidebar;
mod spans;
mod text;
mod views;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use popups::*;
use sidebar::*;
pub use spans::status_line;
// For the previews of sessions without a screen (app::texts).
use spans::*;
pub(crate) use spans::{agent_spans, badge};
use text::*;
use views::*;

use super::app::{App, Dialog, Mode};
use super::form::{Form, Kind};
use super::list::{Entry, Group};
use super::picker::Picker;
use super::theme::{self, ACCENT, ERROR, FAINT, INPUT, MUTED, SELECTED_BG, UNREAD, WORKING};
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
    } else if app.mode == Mode::Settings {
        draw_settings(f, app, main);
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
    // Only the session list has spinners.
    app.mode == Mode::Sessions
        && app
            .sessions
            .iter()
            .any(|s| s.deleted_at.is_none() && s.state == State::Working)
}

/// The session list's width: 30% of the terminal, 34 to 40 columns (wider
/// only spread cards out; asked for in real use), the preview keeping 20.
fn sidebar_width(total: u16) -> u16 {
    (total * 3 / 10)
        .clamp(34, 40)
        .min(total.saturating_sub(20).max(10))
}

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
    if let Some(s) = app.selected_session() {
        let parts = crate::stats::summary(s, app.preview.stats.as_ref(), now);
        let line = truncate(&parts.join(" · "), stats.width as usize);
        f.render_widget(
            Paragraph::new(Line::styled(line, Style::new().fg(MUTED))),
            stats,
        );
    }
    // The bottom of an agent's screen is where its prompt is; long lines are cut
    // (agents drawing full screen get this size, so theirs fit).
    app.preview.size.set(Some((body.width, body.height)));
    // Drawn line by line from the preview itself: it's redrawn as often as
    // the spinners move, and copying a colourful screen each time adds up.
    let lines = &app.preview.text.lines;
    let skip = lines.len().saturating_sub(body.height as usize);
    for (line, row) in lines[skip..].iter().zip(body.rows()) {
        f.render_widget(line, row);
    }
}

fn header(app: &App, width: usize, now: i64) -> Vec<Span<'static>> {
    let sep = || Span::styled(" · ", Style::new().fg(FAINT));
    match app.selected_entry() {
        Some(Entry::Card(s) | Entry::Row(s)) => {
            let left = [vec![badge(&s.project), Span::raw(" ")], whereabouts(s)].concat();
            let mut right = agent_spans(s);
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

#[cfg(test)]
mod tests;
