//! The TUI: sessions on shelves (or grouped by project) on the left, the selected
//! agent's screen on the right. Enter hands the terminal to tmux; the back key
//! returns here.

mod app;
mod form;
mod list;
mod picker;
mod theme;
mod ui;

pub use ui::status_line;

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
use ratatui::text::Text;

use crate::agent;
use crate::engine::{Adopt, Defaults, NewSession, Roster};
use crate::keys::{Action, Keymap, Menu};
use app::{App, Mode, Op};
use list::{Entry, EntryKey, Group};

/// How often states and the preview refresh.
const TICK: Duration = Duration::from_secs(1);
/// The smallest size agents are fitted to; a narrower preview cuts their lines.
const MIN_FIT: (u16, u16) = (60, 10);

pub fn run(roster: &mut Roster) -> Result<()> {
    let keymap = Keymap::new(&roster.config.keys)?;
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let agents = agent::available(&roster.config)
        .into_iter()
        .map(|name| {
            let resumes = agent::adapter(&name).resumes();
            (name, resumes)
        })
        .collect();
    let mut app = App::new(keymap, cwd, agents);
    app.order = roster.config.order.clone();
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
        // Redraw often enough to move the spinners, refresh the data once a tick.
        let until_reload = TICK.saturating_sub(last_reload.elapsed());
        let wait = if ui::animating(app) {
            until_reload.min(Duration::from_millis(theme::SPINNER_MS as u64))
        } else {
            until_reload
        };
        if event::poll(wait)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let (selected, mode) = (app.selected_key(), app.mode);
            match app.on_key(key) {
                Some(Op::Quit) => return Ok(()),
                Some(op) => {
                    perform(terminal, roster, app, op)?;
                    reload(roster, app);
                    last_reload = Instant::now();
                }
                None if app.mode != mode => reload(roster, app),
                None if app.selected_key() != selected => update_preview(roster, app),
                None => {}
            }
        }
        if last_reload.elapsed() >= TICK {
            reload(roster, app);
            last_reload = Instant::now();
        }
    }
}

/// Folds hook events, checks the agents, and reloads the data from the database.
fn reload(roster: &mut Roster, app: &mut App) {
    // The server report refreshes too.
    let refreshed = if app.mode == Mode::Server {
        roster.diagnose().map(|d| app.set_report(d))
    } else {
        roster.refresh()
    };
    if let Err(e) = refreshed {
        app.fail(format!("{e:#}"));
    }
    match (roster.projects(), roster.sessions(true)) {
        (Ok(projects), Ok(sessions)) => app.set_data(projects, sessions),
        (Err(e), _) | (_, Err(e)) => app.fail(format!("{e:#}")),
    }
    if let Ok(names) = roster.db.model_names() {
        app.model_names = names;
    }
    app.chat_project = roster.chat_project_id().ok().flatten();
    fit(roster, app);
    update_preview(roster, app);
}

/// Agents drawing full screen sized to the preview, so their screens fit it.
fn fit(roster: &mut Roster, app: &mut App) {
    let Some((w, h)) = app.preview_size.get() else {
        return;
    };
    let size = (w.max(MIN_FIT.0), h.max(MIN_FIT.1));
    if app.fitted_size != Some(size) {
        app.fitted.clear();
        app.fitted_size = Some(size);
    }
    if let Err(e) = roster.fit_screens(size, &mut app.fitted) {
        app.fail(format!("{e:#}"));
    }
}

fn update_preview(roster: &Roster, app: &mut App) {
    app.preview = match app.selected_entry().cloned() {
        None => app.empty_info(),
        Some(Entry::Heading {
            group: Group::Project(id),
            ..
        }) => app
            .project(id)
            .cloned()
            .map_or_else(Text::default, |p| app.project_info(&p)),
        Some(Entry::Heading {
            group: Group::Shelf(shelf),
            count,
            ..
        }) => app.shelf_info(shelf, count),
        Some(Entry::Card(s) | Entry::Row(s)) if s.deleted_at.is_some() => app.archived_info(&s),
        Some(Entry::Card(s) | Entry::Row(s)) => match roster.screen(&s.id, true) {
            Ok(screen) => screen_text(&screen),
            Err(_) => {
                let key = app.chord(Menu::Session, Action::Start);
                let then = if app.resumes(&s.agent) {
                    "it resumes its conversation"
                } else {
                    "it starts a new conversation"
                };
                Text::from(format!("Not running. Press {key} to start it; {then}."))
            }
        },
    };
    app.stats = app
        .selected_session()
        .cloned()
        .and_then(|s| agent_stats(app, &s));
}

/// What the agent can tell about a session, its transcript read incrementally.
fn agent_stats(app: &mut App, s: &crate::model::Session) -> Option<crate::stats::AgentStats> {
    if s.deleted_at.is_some() {
        return None;
    }
    let path = std::path::Path::new(s.transcript_path.as_deref()?);
    // After /clear the conversation, and its transcript, change.
    if app.transcripts.get(&s.id).is_none_or(|t| t.path() != path) {
        let reader = agent::adapter(&s.agent).transcript(path)?;
        app.transcripts.insert(s.id.clone(), reader);
    }
    let reader = app.transcripts.get_mut(&s.id)?;
    reader.update().ok()?;
    Some(reader.stats(s.running_model.as_deref()))
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
        Op::Attach(id) => {
            app.attaching(&id);
            let result = attach(terminal, roster, &id)?.map(|prompted| {
                app.detached(prompted);
                String::new()
            });
            // Attaching sized it to the terminal: fit it to the preview again.
            app.fitted.clear();
            result
        }
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
        Op::Archive(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Archiving {label}…"))?;
            roster
                .archive(&id)
                .map(|()| format!("Archived {label}; it's under Archived"))
        }
        Op::Delete(id) => roster
            .delete(&id)
            .map(|s| format!("Deleted {} for good", s.label())),
        Op::Restore { id, name } => roster.restore(&id, name.as_deref()).map(|s| {
            app.select(EntryKey::Session(s.id.clone()));
            format!("Restored {} (stopped)", s.label())
        }),
        Op::SetUnread(id, unread) => roster.set_unread(&id, unread).map(|()| String::new()),
        Op::NewSession {
            project,
            agent,
            name,
            model,
            effort,
            prompt,
        } => {
            busy(terminal, app, format!("Starting {agent} in {project}…"))?;
            let prompted = prompt.is_some();
            let request = NewSession {
                project,
                name,
                agent: Some(agent),
                model,
                effort,
                prompt,
            };
            roster.new_session(request).map(|s| {
                app.select(EntryKey::Session(s.id.clone()));
                let key = app.keymap.label(Action::Confirm).unwrap_or("?");
                let prompt = if prompted {
                    "; the prompt is typed in once it's ready"
                } else {
                    ""
                };
                format!("Started {}{prompt}; {key} attaches", s.label())
            })
        }
        Op::EditSession {
            id,
            name,
            title,
            model,
            effort,
        } => roster
            .update_session(&id, &name, title, model, effort)
            .map(|s| {
                if s.state == crate::model::State::Stopped {
                    format!("Saved {}", s.label())
                } else {
                    let chord = app.chord(Menu::Session, Action::Restart);
                    format!(
                        "Saved {}; a new model or effort applies after a restart ({chord})",
                        s.label()
                    )
                }
            }),
        Op::AddProject {
            name,
            path,
            agent,
            model,
            effort,
        } => roster
            .add_project(
                &name,
                Path::new(&path),
                Defaults {
                    agent,
                    model,
                    effort,
                },
            )
            .map(|p| {
                app.select_project(&p.name);
                let chord = app.chord(Menu::Session, Action::NewSession);
                format!("Added project {}; {chord} starts a session in it", p.name)
            }),
        Op::EditProject {
            id,
            name,
            path,
            agent,
            model,
            effort,
        } => roster
            .update_project(
                id,
                &name,
                Path::new(&path),
                Defaults {
                    agent,
                    model,
                    effort,
                },
            )
            .map(|p| {
                app.select_project(&p.name);
                format!("Saved project {}", p.name)
            }),
        Op::RetitleSession(id) => {
            let label = app.session_label(&id);
            busy(
                terminal,
                app,
                format!("Asking Haiku for a title for {label}…"),
            )?;
            roster
                .generate_title(&id)
                .map(|title| format!("New title: {title}"))
        }
        Op::NewChat => roster.chat_project().and_then(|chats| {
            app.chat_project = Some(chats.id);
            app.open_chat_form(&roster.projects()?, &chats);
            Ok(String::new())
        }),
        Op::FindConversations => roster.adoptable(true).map(|found| {
            app.pick_conversation(found);
            String::new()
        }),
        Op::Adopt {
            conversation,
            project,
            name,
            model,
            effort,
            fork,
            force,
        } => {
            busy(terminal, app, format!("Adopting into {project}…"))?;
            let request = Adopt {
                conversation,
                project: Some(project),
                name,
                model: Some(model),
                effort: Some(effort),
                fork,
                force,
            };
            roster.adopt(request).map(|s| {
                app.select(EntryKey::Session(s.id.clone()));
                let key = app.keymap.label(Action::Confirm).unwrap_or("?");
                let copy = if fork { " as a copy" } else { "" };
                format!("Adopted {}{copy}; {key} attaches", s.label())
            })
        }
        Op::KillOrphans => roster
            .kill_orphans()
            .map(|n| format!("Killed {n} orphaned tmux session(s)")),
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
/// is the terminal's, the inner one the attach's: whether a prompt was submitted.
fn attach(terminal: &mut DefaultTerminal, roster: &mut Roster, id: &str) -> Result<Result<bool>> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    let result = roster.attach(id);
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(result)
}
