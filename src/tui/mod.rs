//! The TUI: sessions on shelves (or grouped by project) on the left, the selected
//! agent's screen on the right. Enter hands the terminal to tmux; the back key
//! returns here.

mod app;
mod form;
mod input;
mod limits;
mod list;
mod note;
mod op;
mod picker;
mod theme;
mod tokens;
mod ui;

pub use ui::status_line;

use std::collections::HashMap;
use std::io::stdout;
use std::path::Path;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::text::Text;

use crate::adopt::Adopt;
use crate::agent;
use crate::engine::{Defaults, NewSession, Roster};
use crate::keys::{Action, Keymap, Menu};
use crate::note::Whose;
use crate::settings::Reloaded;
use app::{App, Mode};
use list::{Entry, EntryKey, Group};
use op::Op;

/// How often states and the preview refresh.
const TICK: Duration = Duration::from_secs(1);
/// The smallest size agents are fitted to; a narrower preview cuts their lines.
const MIN_FIT: (u16, u16) = (60, 10);

pub fn run(roster: &mut Roster) -> Result<()> {
    let _only = only_one(&roster.paths.state_dir)?;
    let keymap = Keymap::new(&roster.config.keys)?;
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let mut app = App::new(keymap, cwd, agent::available(&roster.config));
    app.order = roster.config.order.clone();
    app.notes = roster.config.notes.clone();
    app.sidebar_usage = roster.config.sidebar_usage();
    app.set_grouped(roster.config.grouped());
    app.config_file = roster.paths.config_file();
    app.models.chosen = form::Models::chosen_in(&roster.config);
    app.listing = Some(list_models(roster));
    for notice in roster.take_notices() {
        app.fail(notice);
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, roster, &mut app);
    ratatui::restore();
    result
}

/// One TUI at a time: two would each act on what they last read, and size
/// agents' windows each to its own preview. A lock in the state directory,
/// held while the returned file is open; it goes with the process, so a crash
/// can't leave it behind. Commands such as `roster ls` don't take it.
fn only_one(state_dir: &Path) -> Result<std::fs::File> {
    use std::io::{Read, Seek, Write};
    use std::os::fd::AsRawFd;
    let path = state_dir.join("tui.lock");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("can't open {}", path.display()))?;
    // SAFETY: flock(2) on a descriptor we own has no memory effects.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let mut holder = String::new();
        file.read_to_string(&mut holder).ok();
        let holder = holder.trim();
        let holder = if holder.is_empty() {
            String::new()
        } else {
            format!(" ({holder})")
        };
        bail!(
            "roster is already open{holder}; use that one (commands such as `roster ls` work alongside it)"
        );
    }
    // Who has it, for the message above.
    let tty = std::fs::read_link("/proc/self/fd/0")
        .map(|t| format!(" on {}", t.display()))
        .unwrap_or_default();
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "pid {}{tty}", std::process::id())?;
    Ok(file)
}

/// The agents' models, asked in the background: pi takes a moment to list its
/// own, and codex asks its server. Each agent's arrive as they're known, so a
/// slow one holds up neither the forms nor the others.
fn list_models(roster: &Roster) -> std::sync::mpsc::Receiver<(String, Vec<agent::Model>)> {
    let (send, models) = std::sync::mpsc::channel();
    let list = roster.model_lister();
    let agents = agent::available(&roster.config);
    std::thread::spawn(move || {
        for agent in agents {
            send.send((agent.clone(), list(&agent))).ok();
        }
    });
    models
}

/// Settings just changed (in the view, an editor, a `git pull`): what the
/// TUI keeps of them follows.
fn use_config(roster: &Roster, app: &mut App) {
    match Keymap::new(&roster.config.keys) {
        Ok(keymap) => app.keymap = keymap,
        Err(e) => app.fail(format!("{e:#}")),
    }
    app.set_agents(agent::available(&roster.config));
    app.order = roster.config.order.clone();
    app.notes = roster.config.notes.clone();
    app.sidebar_usage = roster.config.sidebar_usage();
    app.set_grouped(roster.config.grouped());
    app.preview.fitted.clear();
    app.models.chosen = form::Models::chosen_in(&roster.config);
    app.listing = Some(list_models(roster));
}

fn event_loop(terminal: &mut DefaultTerminal, roster: &mut Roster, app: &mut App) -> Result<()> {
    reload(roster, app);
    let mut last_reload = Instant::now();
    loop {
        // What each agent has answered of its models, as it answers, into an
        // open form too; the receiver goes once its thread is done.
        if let Some(listing) = &app.listing {
            let mut arrived = false;
            loop {
                match listing.try_recv() {
                    Ok((agent, models)) => {
                        app.models.offered.insert(agent, models);
                        arrived = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        app.listing = None;
                        break;
                    }
                }
            }
            if arrived && let Some(app::Dialog::Form(form)) = &mut app.dialog {
                form.set_models(&app.models);
            }
        }
        // Titles asked for (`s t`) that have come, kept and said.
        if titles_arrived(roster, app) {
            reload(roster, app);
            last_reload = Instant::now();
        }
        // A heading's sessions' prompts, read on a thread, show as they come.
        if transcripts_read(app) && heading_shown(app) {
            update_preview(roster, app);
        }
        // What the agents' accounts have used, asked in the background
        // while something shows it.
        if app.limits.due(app.sidebar_usage || app.mode == Mode::Usage) {
            app.limits.ask(roster.usage_reader());
        }
        app.limits.arrived();
        // What the agents spent, read in the background while a period of
        // it shows.
        let period = app.usage_period.filter(|_| app.mode == Mode::Usage);
        if app.tokens.due(period.is_some()) {
            app.tokens.read(tokens::scan(roster.paths.db()));
        }
        app.tokens.arrived();
        if let Some(period) = period {
            app.tokens.sum(period, &roster.db);
        }
        // Escape keys held back in the note editor go in once it's too late
        // for the rest (`notes.escape_ms`).
        if let Some(open) = app.note.as_mut() {
            open.shown_mut().tick(Instant::now());
        }
        let size = app.preview.size.get();
        terminal.draw(|f| ui::draw(f, app))?;
        // A heading's preview is made to the preview's size: resized, it's
        // made again.
        if app.preview.size.get() != size && heading_shown(app) {
            update_preview(roster, app);
            terminal.draw(|f| ui::draw(f, app))?;
        }
        // Redraw often enough to move the spinners, refresh the data once a tick.
        let until_reload = TICK.saturating_sub(last_reload.elapsed());
        let mut wait = if ui::animating(app) {
            until_reload.min(Duration::from_millis(theme::SPINNER_MS as u64))
        } else {
            until_reload
        };
        if let Some(deadline) = app.note.as_ref().and_then(|n| n.shown().deadline()) {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        // A title or a transcript coming is looked for more often than a tick.
        if !app.titling.is_empty()
            || !app.preview.reading.ids.is_empty()
            || app.limits.asking()
            || app.tokens.reading()
        {
            wait = wait.min(Duration::from_millis(200));
        }
        if event::poll(wait)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let editing = |app: &App| app.note.as_ref().map(|n| n.shown().whose.clone());
            let (selected, mode, was_editing) = (app.selected_key(), app.mode, editing(app));
            match app.on_key(key) {
                Some(Op::Quit) => return Ok(()),
                Some(op) => {
                    perform(terminal, roster, app, op)?;
                    reload(roster, app);
                    last_reload = Instant::now();
                }
                None if app.mode != mode => reload(roster, app),
                // A stopped session's panel shows its note unless it's open.
                None if app.selected_key() != selected || editing(app) != was_editing => {
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

/// Keeps the titles that came from their threads (`Op::RetitleSession`) and
/// says so, or why not (all that came together, errors first); whether any
/// came.
fn titles_arrived(roster: &mut Roster, app: &mut App) -> bool {
    let mut arrived = Vec::new();
    app.titling.retain(|t| match t.title.try_recv() {
        Ok(made) => {
            arrived.push((t.id.clone(), t.asked_of.clone(), made));
            false
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => true,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            let gone = anyhow::anyhow!("no title came: its run ended without an answer");
            arrived.push((t.id.clone(), t.asked_of.clone(), Err(gone)));
            false
        }
    });
    let any = !arrived.is_empty();
    let (mut errors, mut news) = (Vec::new(), Vec::new());
    for (id, asked_of, made) in arrived {
        let label = app.session_label(&id);
        let kept = made.and_then(|title| {
            roster
                .keep_title(&id, &title, &asked_of)
                .map(|kept| (title, kept))
        });
        use crate::title::Kept;
        match kept {
            Ok((title, Kept::Yes)) => news.push(format!("New title for {label}: {title}")),
            Ok((_, Kept::Retitled)) => news.push(format!(
                "{label}'s title changed meanwhile, so the new one isn't kept"
            )),
            Ok((_, Kept::Moved)) => news.push(format!(
                "{label} is on another conversation now, so the new title isn't kept"
            )),
            // Deleted meanwhile: there's no one to tell.
            Ok((_, Kept::Gone)) => {}
            Err(e) => errors.push((label, format!("{e:#}"))),
        }
    }
    match (title_failures(&errors), news.is_empty()) {
        (Some(failed), _) => app.fail_too([vec![failed], news].concat().join(" · ")),
        (None, false) => app.notice(news.join(" · ")),
        (None, true) => {}
    }
    any
}

/// What to say of titles that failed (each a session and why), in the
/// footer's one line: the first in full, then which others failed too.
fn title_failures(errors: &[(String, String)]) -> Option<String> {
    let ((label, why), others) = errors.split_first()?;
    let first = format!("{label}: {why}");
    if others.is_empty() {
        return Some(first);
    }
    let others: Vec<&str> = others.iter().map(|(l, _)| l.as_str()).collect();
    Some(format!("{first} · not {} either", others.join(", ")))
}

/// Folds hook events, checks the agents, and reloads the data from the database
/// (and config.toml, if it changed).
fn reload(roster: &mut Roster, app: &mut App) {
    match roster.reload_config() {
        Ok(Reloaded::InUse(None)) => {
            use_config(roster, app);
            app.flash("config.toml changed: its settings are in use");
        }
        Ok(Reloaded::InUse(Some(e))) => {
            use_config(roster, app);
            app.fail(format!(
                "config.toml's settings are in use, but roster's tmux didn't take them: {e:#}"
            ));
        }
        Ok(Reloaded::Same) => {}
        // What matters first: a long message is cut at its end.
        Err(e) => app.fail(format!(
            "config.toml has an error, the settings in use stay: {e:#}"
        )),
    }
    if app.mode == Mode::Settings {
        app.set_settings(roster.settings());
    }
    // The server report refreshes too.
    let refreshed = if app.mode == Mode::Server {
        roster.diagnose().map(|d| app.set_report(d))
    } else {
        roster.refresh()
    };
    // Every prompt not typed in, unless the refresh itself failed (that first).
    let undelivered: Vec<String> = roster
        .take_undelivered()
        .into_iter()
        .map(|u| u.message)
        .collect();
    match refreshed {
        Err(e) => app.fail(format!("{e:#}")),
        Ok(()) if !undelivered.is_empty() => app.fail(undelivered.join("; ")),
        Ok(()) => {}
    }
    match (roster.projects(), roster.sessions(true)) {
        (Ok(projects), Ok(sessions)) => app.set_data(projects, sessions),
        (Err(e), _) | (_, Err(e)) => app.fail(format!("{e:#}")),
    }
    if let Ok(names) = roster.db.model_names() {
        app.models.names = names;
    }
    app.chat_project = roster.chat_project_id().ok().flatten();
    // What codex wrote of its limits since, which costs nothing to read.
    if app.sidebar_usage || app.mode == Mode::Usage {
        app.limits.read_transcripts(&app.sessions);
    }
    // Only the session list shows the preview.
    if app.mode == Mode::Sessions {
        fit(roster, app);
        update_preview(roster, app);
    }
    // Notices (roster's tmux not taking its config as a start started it),
    // last: after any error showing (the start's own, say), not under one.
    let notices = roster.take_notices();
    if !notices.is_empty() {
        app.fail_too(notices.join("; "));
    }
}

/// Agents drawing full screen sized to the preview, so their screens fit it.
fn fit(roster: &mut Roster, app: &mut App) {
    let Some((w, h)) = app.preview.size.get() else {
        return;
    };
    let size = (w.max(MIN_FIT.0), h.max(MIN_FIT.1));
    if app.preview.fitted_size != Some(size) {
        app.preview.fitted.clear();
        app.preview.fitted_size = Some(size);
    }
    if let Err(e) = roster.fit_screens(&app.sessions, size, &mut app.preview.fitted) {
        app.fail(format!("{e:#}"));
    }
}

fn update_preview(roster: &Roster, app: &mut App) {
    // What its transcript tells, first: a session with no screen shows its
    // last prompts.
    let (stats, prompts) = app
        .selected_session()
        .cloned()
        .and_then(|s| transcript_info(roster, app, &s))
        .unzip();
    let prompts = prompts.unwrap_or_default();
    let now = crate::util::now_ms();
    app.preview.stats = stats;
    app.preview.screen = false;
    app.preview.text = match app.selected_entry().cloned() {
        None => app.empty_info(),
        // A shelf or a project: its sessions at a glance.
        Some(Entry::Heading { group, .. }) => {
            let size = app.preview.size.get().unwrap_or((80, 24));
            let prompts = group_prompts(app, group, size.1);
            app.group_info(group, &prompts, size, now)
        }
        Some(Entry::Card(s) | Entry::Row(s)) if s.deleted_at.is_some() => {
            app.session_info(&s, &prompts, &[], now)
        }
        // Stopped: what it was, as when archived; after an error, also how
        // its screen ended (kept in tmux), where the error is.
        Some(Entry::Card(s) | Entry::Row(s)) if s.state == crate::model::State::Stopped => {
            let width = app.preview.size.get().map_or(80, |(w, _)| w as usize);
            let tail = match s.exit_status {
                Some(code) if code != 0 => roster
                    .screen(&s, false)
                    .map(|raw| app::screen_tail(&raw, LAST_SCREEN_LINES, width.saturating_sub(2)))
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            app.session_info(&s, &prompts, &tail, now)
        }
        Some(Entry::Card(s) | Entry::Row(s)) => match roster.screen(&s, true) {
            Ok(screen) => {
                app.preview.screen = true;
                screen_text(&screen)
            }
            Err(_) => app.session_info(&s, &prompts, &[], now),
        },
    };
}

/// The last prompt of each of `group`'s sessions that may show in `rows`,
/// from their transcripts: those read before (which costs next to nothing);
/// the others are read on a thread, and show when they come
/// (`transcripts_read`).
fn group_prompts(app: &mut App, group: Group, rows: u16) -> HashMap<String, String> {
    // As many as can show: each takes a line, and one for its prompt and
    // one for its note.
    let mut lines = 0;
    let members: Vec<crate::model::Session> = app
        .group_members(group)
        .into_iter()
        .take_while(|s| {
            let fits = lines < rows as usize;
            lines += 1 + usize::from(s.transcript_path.is_some()) + usize::from(s.note.is_some());
            fits
        })
        .cloned()
        .collect();
    let mut prompts = HashMap::new();
    let mut unread = Vec::new();
    for s in members {
        let Some(path) = s.transcript_path.as_deref().map(Path::new) else {
            continue;
        };
        match app.preview.transcripts.get_mut(&s.id) {
            Some(reader) if reader.path() == path => {
                if reader.update().is_ok()
                    && let Some(last) = reader.recent_prompts().pop()
                {
                    prompts.insert(s.id.clone(), last);
                }
            }
            // Not read again: one being read, or one that's gone (deleted
            // with its conversation).
            _ if app.preview.reading.ids.contains(&s.id) || !path.exists() => {}
            _ => {
                if let Some(reader) = agent::adapter(&s.agent).transcript(path) {
                    unread.push((s.id.clone(), reader));
                }
            }
        }
    }
    if !unread.is_empty() {
        let reading = &mut app.preview.reading;
        reading.ids.extend(unread.iter().map(|(id, _)| id.clone()));
        let send = reading.send.clone();
        std::thread::spawn(move || {
            for (id, mut reader) in unread {
                let read = reader.update().is_ok().then_some(reader);
                if send.send((id, read)).is_err() {
                    return;
                }
            }
        });
    }
    prompts
}

/// Keeps the transcripts read on a thread that have come (`group_prompts`);
/// whether any did. One that couldn't be read is tried again at the next
/// refresh.
fn transcripts_read(app: &mut App) -> bool {
    let reading = &mut app.preview.reading;
    let mut any = false;
    for (id, read) in reading.came.try_iter() {
        reading.ids.remove(&id);
        if let Some(reader) = read {
            app.preview.transcripts.insert(id, reader);
            any = true;
        }
    }
    any
}

/// Whether the session list shows a heading's preview, which is made to its
/// size and of what's been read on a thread.
fn heading_shown(app: &App) -> bool {
    app.mode == Mode::Sessions && matches!(app.selected_entry(), Some(Entry::Heading { .. }))
}

/// What the agent can tell about a session, its transcript read incrementally:
/// its statistics and its last prompts.
fn transcript_info(
    roster: &Roster,
    app: &mut App,
    s: &crate::model::Session,
) -> Option<(crate::stats::AgentStats, Vec<String>)> {
    let path = std::path::Path::new(s.transcript_path.as_deref()?);
    // After /clear the conversation, and its transcript, change.
    if app
        .preview
        .transcripts
        .get(&s.id)
        .is_none_or(|t| t.path() != path)
    {
        let reader = agent::adapter(&s.agent).transcript(path)?;
        app.preview.transcripts.insert(s.id.clone(), reader);
    }
    let reader = app.preview.transcripts.get_mut(&s.id)?;
    reader.update().ok()?;
    let stats = roster.with_window(reader.stats(s.running_model.as_deref()), s);
    Some((stats, reader.recent_prompts()))
}

/// How many of a stopped agent's last lines its preview shows after an error.
const LAST_SCREEN_LINES: usize = 8;

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
        Op::ChangeSetting { setting, input } => {
            let back = input.is_none();
            let before = roster.config_text.clone();
            let changed = roster.change_setting(&setting, input.as_deref());
            if roster.config_text != before {
                use_config(roster, app);
            }
            changed.map(|()| {
                if back {
                    format!("{setting} is back to its default")
                } else {
                    format!("Set {setting}")
                }
            })
        }
        Op::EditConfig => edit_config(terminal, roster, app)?,
        Op::AddAgent(line) => {
            let before = roster.config_text.clone();
            let added = roster.add_agent(&line);
            if roster.config_text != before {
                use_config(roster, app);
            }
            added.map(|name| format!("Added the agent {name}: new sessions can run it"))
        }
        Op::Attach(id) => {
            app.attaching(&id);
            let result = attach(terminal, roster, app, &id)?.map(|prompted| {
                app.detached(prompted);
                String::new()
            });
            // Attaching sized it to the terminal: fit it to the preview again.
            app.preview.fitted.clear();
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
            .update_session(&id, name.as_deref(), title, model, effort)
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
        // Asked on a thread: the model takes a while, and the TUI goes on.
        Op::RetitleSession(id) if app.titling.iter().any(|t| t.id == id) => Err(anyhow::anyhow!(
            "Already asking for a title for {}",
            app.session_label(&id)
        )),
        Op::RetitleSession(id) => roster.title_job(&id).map(|job| {
            let agent = crate::util::capitalize(&job.agent);
            let asked_of = job.asked_of.clone();
            let (send, title) = std::sync::mpsc::channel();
            std::thread::spawn(move || send.send(job.run()));
            app.titling.push(app::Titling {
                id: id.clone(),
                title,
                asked_of,
            });
            format!("Asking {agent} for a title for {}…", app.session_label(&id))
        }),
        Op::SaveNotes { saves, close } => save_notes(roster, app, saves, close),
        Op::EditNote(whose) => edit_note(terminal, roster, app, &whose)?,
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
            run_on,
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
                run_on,
                model,
                effort,
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

/// Hands the terminal to `run` (an editor) and takes it back. The outer result
/// is the terminal's.
fn in_terminal<T>(terminal: &mut DefaultTerminal, run: impl FnOnce() -> T) -> Result<T> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    let ran = run();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(ran)
}

/// Saves notes from the editor in turn, each marked saved there, until one
/// can't be; then closes it if all were and `close`. What was saved, to say.
fn save_notes(
    roster: &Roster,
    app: &mut App,
    saves: Vec<op::NoteSave>,
    close: bool,
) -> Result<String> {
    let mut said = Vec::new();
    for s in saves {
        let saved = roster.save_note(&s.whose, &s.text, s.pin, s.base.as_ref(), s.force);
        let note = match saved {
            Ok(note) => note,
            // What was saved before is, and says so.
            Err(e) if said.is_empty() => return Err(e),
            Err(e) => return Err(e.context(said.join("; "))),
        };
        if let Some(editor) = app.note.as_mut().and_then(|n| n.of(&s.whose)) {
            editor.saved(note.as_ref());
        }
        said.push(match note {
            Some(_) => format!("Saved the note of {}", s.label),
            None => format!("{} has no note now", s.label),
        });
    }
    if close {
        app.note = None;
    }
    Ok(said.join("; "))
}

/// Hands a note to `$EDITOR`: the open editor's text, which comes back into
/// it, or else the note as saved, saved again when the editor exits. The
/// outer result is the terminal's.
fn edit_note(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    whose: &Whose,
) -> Result<Result<String>> {
    let noted = match whose {
        Whose::Session(id) => roster.resolve_any(id).map(|s| s.noted()),
        Whose::Project(id) => roster.projects().and_then(|ps| {
            let p = ps.into_iter().find(|p| p.id == *id);
            Ok(p.context("the project is gone")?.noted())
        }),
    };
    let n = match noted {
        Ok(n) => n,
        Err(e) => return Ok(Err(e)),
    };
    let file = roster.note_file(&n);
    let failed = || anyhow::anyhow!("$EDITOR exited with an error; the note is as it was");
    if let Some(editor) = app.note.as_mut().and_then(|open| open.of(whose)) {
        let text = editor.text();
        let back = in_terminal(terminal, || {
            crate::note::edit_externally(&file, &text, |back| {
                editor.replace(&back);
                Ok(())
            })
        })?;
        return Ok(back.and_then(|done| {
            done.map(|()| "Back from $EDITOR: :w saves the note".to_string())
                .ok_or_else(failed)
        }));
    }
    let text = n.note.as_ref().map_or(String::new(), |n| n.text.clone());
    let saved = in_terminal(terminal, || {
        crate::note::edit_externally(&file, &text, |back| roster.change_note(&n, &back))
    })?;
    Ok(saved.and_then(|said| said.ok_or_else(failed)))
}

/// Hands the terminal to `$VISUAL` or `$EDITOR` (else vi) on config.toml, made
/// first from the template of every setting if there's none, then puts what
/// was saved in use. The outer result is the terminal's.
fn edit_config(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
) -> Result<Result<String>> {
    let file = match roster.config_file_to_edit() {
        Ok((file, _)) => file,
        Err(e) => return Ok(Err(e)),
    };
    Ok(
        match in_terminal(terminal, || crate::util::run_editor(&file))? {
            Err(e) => Err(e),
            Ok(_) => match roster.reload_config_now() {
                Ok(Reloaded::InUse(problem)) => {
                    use_config(roster, app);
                    match problem {
                        None => Ok("config.toml saved: its settings are in use".into()),
                        Some(e) => Err(e.context(
                            "config.toml's settings are in use, but roster's tmux didn't take them",
                        )),
                    }
                }
                Ok(Reloaded::Same) => Ok("config.toml unchanged".into()),
                Err(e) => Err(e.context("config.toml has an error, the settings in use stay")),
            },
        },
    )
}

/// Hands the terminal to `tmux attach` until the user detaches. The outer result
/// is the terminal's, the inner one the attach's: whether a prompt was submitted.
///
/// tmux switches to the terminal's other screen and back by itself, so roster
/// stays on its own until then (leaving it first flashed the normal screen),
/// and draws itself again as it was the moment tmux is done, before catching
/// up on what happened meanwhile (which takes a moment: catching up first
/// showed the normal screen, then a blank one, found in real use).
fn attach(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &App,
    id: &str,
) -> Result<Result<bool>> {
    disable_raw_mode()?;
    let attached = roster.attach_terminal(id);
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(attached.and_then(|since| roster.detached(id, since)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headings_transcripts_are_read_on_a_thread_as_many_as_show() {
        use crate::tui::list::Shelf;
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| Some(dir.path().join(name).to_string_lossy().into_owned());
        // Not empty, so it isn't taken for an empty file on any filesystem.
        std::fs::create_dir(dir.path().join("a-directory")).unwrap();
        std::fs::write(dir.path().join("a-directory/x"), "x").unwrap();
        let mut app = App::new(
            crate::keys::Keymap::new(&crate::config::Keys::default()).unwrap(),
            "/".into(),
            vec!["claude".into()],
        );
        let sessions: Vec<_> = (0..10)
            .map(|i| {
                let mut s = crate::model::tests::session(&format!("s{i}"));
                s.state = crate::model::State::NeedsInput;
                // First come, first served: s0 on top.
                s.state_since = Some(i);
                s.transcript_path = match i {
                    // Gone, and one that can't be read.
                    0 => path("gone.jsonl"),
                    1 => path("a-directory"),
                    _ => {
                        let line =
                            format!(r#"{{"type":"user","message":{{"content":"task {i}"}}}}"#);
                        std::fs::write(dir.path().join(format!("{i}.jsonl")), line + "\n").unwrap();
                        path(&format!("{i}.jsonl"))
                    }
                };
                s
            })
            .map(|mut s| {
                // A note's line counts too.
                if s.id == "s2" {
                    s.note = Some(crate::note::Note {
                        text: "todo".into(),
                        pin: None,
                        at: 0,
                    });
                }
                s
            })
            .collect();
        app.set_data(Vec::new(), sessions);
        let shelf = Group::Shelf(Shelf::NeedsInput);
        let reading = |app: &App| {
            let mut ids: Vec<String> = app.preview.reading.ids.iter().cloned().collect();
            ids.sort();
            ids
        };
        // What's being read comes back, each with its id cleared; whether
        // any could be read.
        let come = |app: &mut App| {
            let until = Instant::now() + Duration::from_secs(5);
            let mut any = false;
            while !app.preview.reading.ids.is_empty() && Instant::now() < until {
                any |= transcripts_read(app);
                std::thread::sleep(Duration::from_millis(10));
            }
            any
        };
        // In 7 rows three show (a line each, one for its prompt, s2's one for
        // its note); the one that's gone isn't read.
        assert!(group_prompts(&mut app, shelf, 7).is_empty());
        assert_eq!(reading(&app), ["s1", "s2"]);
        assert!(come(&mut app));
        assert!(reading(&app).is_empty());
        // The one read is kept and its prompt shows; the other is tried
        // again, and isn't news when it fails again.
        assert_eq!(app.preview.transcripts.keys().collect::<Vec<_>>(), ["s2"]);
        let prompts = group_prompts(&mut app, shelf, 7);
        assert_eq!(prompts, HashMap::from([("s2".into(), "task 2".into())]));
        assert_eq!(reading(&app), ["s1"]);
        assert!(!come(&mut app));
        assert!(reading(&app).is_empty());
        assert_eq!(app.preview.transcripts.len(), 1);
    }

    #[test]
    fn titles_that_failed_say_which_and_why() {
        let failed = |e: &[(&str, &str)]| {
            let e: Vec<(String, String)> = e
                .iter()
                .map(|(l, w)| (l.to_string(), w.to_string()))
                .collect();
            title_failures(&e)
        };
        assert_eq!(failed(&[]), None);
        assert_eq!(
            failed(&[("demo/one", "no prompts to title yet")]),
            Some("demo/one: no prompts to title yet".into())
        );
        // The first in full, the others by name: their causes may differ.
        assert_eq!(
            failed(&[
                ("demo/one", "timed out"),
                ("demo/two", "x"),
                ("demo/three", "y")
            ]),
            Some("demo/one: timed out · not demo/two, demo/three either".into())
        );
    }
}
