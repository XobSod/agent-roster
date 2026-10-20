//! The TUI: projects and sessions on the left, the selected
//! agent's screen on the right. Enter hands the terminal to tmux; the back key
//! returns here.

mod app;
mod ui;

use std::io::stdout;
use std::path::Path;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::text::{Line, Text};

use crate::engine::Roster;
use crate::keys::{Action, Keymap};
use app::{App, Op, Row, RowKey, View};

/// How often states and the preview refresh.
const TICK: Duration = Duration::from_secs(1);

pub fn run(roster: &mut Roster) -> Result<()> {
    let keymap = Keymap::new(&roster.config.keys)?;
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let mut app = App::new(keymap, cwd);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, roster, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, roster: &mut Roster, app: &mut App) -> Result<()> {
    reload(roster, app);
    let mut last_reload = Instant::now();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(TICK.saturating_sub(last_reload.elapsed()))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let (selected, view) = (app.selected_key(), app.view);
            match app.on_key(key) {
                Some(Op::Quit) => return Ok(()),
                Some(op) => {
                    perform(terminal, roster, app, op)?;
                    reload(roster, app);
                    last_reload = Instant::now();
                }
                None if app.selected_key() != selected || app.view != view => {
                    update_preview(roster, app)
                }
                None => {}
            }
        }
        if last_reload.elapsed() >= TICK {
            reload(roster, app);
            last_reload = Instant::now();
        }
    }
}

/// Folds hook events, checks the agents, and redraws the data from the database.
fn reload(roster: &mut Roster, app: &mut App) {
    if let Err(e) = roster.refresh() {
        app.fail(format!("{e:#}"));
    }
    match (roster.projects(), roster.sessions(true)) {
        (Ok(projects), Ok(sessions)) => app.set_data(projects, sessions),
        (Err(e), _) | (_, Err(e)) => app.fail(format!("{e:#}")),
    }
    update_preview(roster, app);
}

fn update_preview(roster: &Roster, app: &mut App) {
    let (title, text) = match app.selected_row().cloned() {
        None => (Line::default(), app.empty_info()),
        Some(Row::Project(p)) => (Line::from(p.name.clone()), app.project_info(&p)),
        Some(Row::Session(s)) if s.deleted_at.is_some() => (
            Line::from(format!("{} (deleted)", s.label())),
            app.deleted_info(&s),
        ),
        Some(Row::Session(s)) => {
            let title = Line::from(vec![format!("{} · ", s.label()).into(), ui::state_span(&s)]);
            let text = match roster.screen(&s.id, true) {
                Ok(screen) => screen_text(&screen),
                Err(_) => {
                    let key = app.keymap.label(Action::Start).unwrap_or("?");
                    Text::from(format!(
                        "Not running. Press {key} to start it; it resumes its conversation."
                    ))
                }
            };
            (title, text)
        }
    };
    app.preview_title = title;
    app.preview = text;
}

/// A captured screen as styled text, without the empty lines below the content.
fn screen_text(screen: &str) -> Text<'static> {
    let mut text = screen
        .into_text()
        .unwrap_or_else(|_| Text::raw(screen.to_string()));
    while text
        .lines
        .last()
        .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
    {
        text.lines.pop();
    }
    text
}

/// Carries out an op. Failures become messages; only terminal errors end the TUI.
fn perform(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    op: Op,
) -> Result<()> {
    let outcome: Result<String> = match op {
        Op::Attach(id) => attach(terminal, roster, &id)?.map(|()| String::new()),
        Op::Start(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Starting {label}…"))?;
            roster.start(&id).map(|()| format!("Started {label}"))
        }
        Op::Stop(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Stopping {label}…"))?;
            roster.stop(&id).map(|stopped| {
                if stopped {
                    format!("Stopped {label}")
                } else {
                    format!("{label} wasn't running")
                }
            })
        }
        Op::Restart(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Restarting {label}…"))?;
            roster.restart(&id).map(|()| format!("Restarted {label}"))
        }
        Op::Delete(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Deleting {label}…"))?;
            let key = app.keymap.label(Action::Deleted).unwrap_or("?").to_string();
            roster
                .delete(&id)
                .map(|()| format!("Deleted {label}; {key} shows deleted sessions"))
        }
        Op::Restore { id, name } => roster.restore(&id, name.as_deref()).map(|s| {
            app.set_view(View::Live);
            app.select(RowKey::Session(s.id.clone()));
            format!("Restored {} (stopped)", s.label())
        }),
        Op::SetUnread(id, unread) => roster.set_unread(&id, unread).map(|()| String::new()),
        Op::NewSession { project, name } => {
            busy(terminal, app, format!("Starting a session in {project}…"))?;
            roster
                .new_session(&project, name.as_deref(), "claude")
                .map(|s| {
                    app.select(RowKey::Session(s.id.clone()));
                    let key = app.keymap.label(Action::Attach).unwrap_or("?");
                    format!("Started {}; {key} attaches", s.label())
                })
        }
        Op::AddProject { name, path } => roster.add_project(&name, Path::new(&path)).map(|p| {
            app.select(RowKey::Project(p.id));
            format!("Added project {}", p.name)
        }),
        Op::RemoveProject(name) => roster
            .remove_project(&name)
            .map(|_| format!("Removed project {name}")),
        Op::Quit => Ok(String::new()),
    };
    match outcome {
        Ok(message) if message.is_empty() => {}
        Ok(message) => app.flash(message),
        Err(e) => app.fail(format!("{e:#}")),
    }
    Ok(())
}

/// Shows a message while a slow operation runs.
fn busy(terminal: &mut DefaultTerminal, app: &mut App, message: String) -> Result<()> {
    app.flash(message);
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(())
}

/// Hands the terminal to `tmux attach` until the user detaches. The outer result
/// is the terminal's, the inner one the attach's.
fn attach(terminal: &mut DefaultTerminal, roster: &mut Roster, id: &str) -> Result<Result<()>> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    let result = roster.attach(id);
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(result)
}
