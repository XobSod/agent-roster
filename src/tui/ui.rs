//! Drawing: the session list on the left, the selected agent's screen on the
//! right, a footer with key hints or the latest message, and dialogs on top.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{App, Dialog, Row, View};
use crate::model::{Session, State};

pub fn draw(f: &mut Frame, app: &App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let [left, right] = Layout::horizontal([
        Constraint::Length(list_width(app, main.width)),
        Constraint::Min(1),
    ])
    .areas(main);
    draw_list(f, app, left);
    draw_preview(f, app, right);
    draw_footer(f, app, footer);
    if let Some(dialog) = &app.dialog {
        draw_dialog(f, dialog, main);
    }
}

pub fn state_span(s: &Session) -> Span<'static> {
    match s.state {
        State::Working => Span::styled("working", Style::new().fg(Color::Yellow)),
        State::NeedsInput => Span::styled("needs input", Style::new().fg(Color::Magenta).bold()),
        State::Idle => Span::styled("idle", Style::new().fg(Color::Green)),
        State::Stopped => {
            let text = match s.exit_status {
                Some(code) if code != 0 => format!("stopped (exit {code})"),
                _ => "stopped".to_string(),
            };
            Span::styled(text, Style::new().fg(Color::DarkGray))
        }
    }
}

/// What the list shows as a session's name.
fn shown_name(s: &Session, view: View) -> String {
    match view {
        View::Live => s.name.clone(),
        View::Deleted => s.label(),
    }
}

fn list_width(app: &App, total: u16) -> u16 {
    let longest = app
        .rows
        .iter()
        .map(|r| match r {
            Row::Project(p) => p.name.chars().count(),
            Row::Session(s) => shown_name(s, app.view).chars().count() + 4,
        })
        .max()
        .unwrap_or(0);
    // Name plus marker, padding, the longest state and borders; never more than 40%.
    let wanted = (longest + 24) as u16;
    wanted.clamp(28, (total * 2 / 5).max(28))
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.view {
        View::Live => " roster ",
        View::Deleted => " deleted sessions ",
    };
    let name_width = app
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Session(s) => Some(shown_name(s, app.view).chars().count()),
            Row::Project(_) => None,
        })
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| match row {
            Row::Project(p) => ListItem::new(Line::from(Span::styled(
                p.name.clone(),
                Style::new().bold(),
            ))),
            Row::Session(s) => ListItem::new(session_line(s, name_width, app.view)),
        })
        .collect();
    let mut state =
        ListState::default().with_selected((!app.rows.is_empty()).then_some(app.selected));
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().reversed());
    f.render_stateful_widget(list, area, &mut state);
}

fn session_line(s: &Session, name_width: usize, view: View) -> Line<'static> {
    let marker = if s.unread {
        Span::styled("● ", Style::new().fg(Color::Cyan).bold())
    } else {
        Span::raw("  ")
    };
    let name = shown_name(s, view);
    let padding = " ".repeat(name_width - name.chars().count() + 2);
    let state = match view {
        View::Live => state_span(s),
        View::Deleted => Span::styled("deleted", Style::new().fg(Color::DarkGray)),
    };
    Line::from(vec![
        Span::raw("  "),
        marker,
        Span::raw(format!("{name}{padding}")),
        state,
    ])
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let mut block = Block::bordered();
    if !app.preview_title.spans.is_empty() {
        let mut title = vec![Span::raw(" ")];
        title.extend(app.preview_title.spans.iter().cloned());
        title.push(Span::raw(" "));
        block = block.title(Line::from(title));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    // The bottom of an agent's screen is where its prompt is; long lines are cut.
    let lines = &app.preview.lines;
    let skip = lines.len().saturating_sub(inner.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines[skip..].to_vec())), inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = match app.message() {
        Some(m) if m.error => Line::from(Span::styled(m.text.clone(), Style::new().fg(Color::Red))),
        Some(m) => Line::from(Span::styled(m.text.clone(), Style::new().fg(Color::Yellow))),
        None => {
            let mut spans = Vec::new();
            for (key, what) in app.hints() {
                spans.push(Span::styled(key, Style::new().bold()));
                spans.push(Span::styled(
                    format!(" {what}  "),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_dialog(f: &mut Frame, dialog: &Dialog, area: Rect) {
    let width = area.width.saturating_sub(4).min(76);
    match dialog {
        Dialog::Confirm { question, .. } => {
            let rect = centered(area, width, 6);
            let text = Text::from(vec![
                Line::from(question.clone()),
                Line::from(""),
                Line::from(vec![
                    "y".bold(),
                    " yes    ".into(),
                    "n".bold(),
                    " no".into(),
                ]),
            ]);
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .block(Block::bordered().title(" confirm ")),
                rect,
            );
        }
        Dialog::Input { prompt, value, .. } => {
            let rect = centered(area, width, 5);
            let block = Block::bordered().title(" Enter: ok · Esc: cancel ");
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
            f.set_cursor_position((
                value_area.x + 2 + shown.chars().count() as u16,
                value_area.y,
            ));
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Keys;
    use crate::keys::Keymap;
    use crate::model::Project;
    use crate::model::tests::session;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
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

    #[test]
    fn shows_projects_sessions_states_and_hints() {
        let mut app = App::new(Keymap::new(&Keys::default()).unwrap(), "/".into());
        let mut s = session("s1");
        s.name = "api".into();
        s.unread = true;
        s.state = State::NeedsInput;
        app.set_data(
            vec![Project {
                id: 1,
                name: "demo".into(),
                origin: "/d".into(),
            }],
            vec![s],
        );
        app.preview = Text::from("agent screen line");
        let screen = render(&app);
        assert!(screen.contains("demo"), "{screen}");
        assert!(screen.contains("● api  needs input"), "{screen}");
        assert!(screen.contains("agent screen line"), "{screen}");
        assert!(screen.contains("Enter attach"), "{screen}");
    }

    #[test]
    fn preview_shows_the_bottom_of_a_tall_screen() {
        let mut app = App::new(Keymap::new(&Keys::default()).unwrap(), "/".into());
        app.preview = Text::from(
            (1..=40)
                .map(|i| Line::from(format!("line {i}")))
                .collect::<Vec<_>>(),
        );
        let screen = render(&app);
        assert!(
            screen.contains("line 40") && !screen.contains("line 20"),
            "{screen}"
        );
    }
}
