//! The TUI: sessions on shelves (or grouped by project) on the left, the selected
//! agent's screen on the right. Enter hands the terminal to tmux; the back key
//! returns here.

mod app;
mod beside;
mod form;
mod input;
mod limits;
mod list;
mod note;
mod op;
mod perform;
mod picker;
mod theme;
mod tokens;
mod transcripts;
mod ui;

pub use beside::{attached_list, run_sidebar};
pub use ui::{status_line, status_line_beside};

use std::collections::{HashMap, HashSet};
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
use crate::engine::{Defaults, NewSession, NoteSaved, Roster};
use crate::keys::Keymap;
use crate::note::Whose;
use crate::settings::Reloaded;
use app::{App, Mode};
use list::{Entry, EntryKey, Group};
use op::Op;
// What one job has that the others use.
use perform::*;
use transcripts::*;

/// How often states and the preview refresh.
const TICK: Duration = Duration::from_secs(1);

/// How often what the sessions' processes use is read for the memory
/// warning: a leak grows over minutes, and reading costs a pass over /proc.
const RESOURCES_EVERY: Duration = Duration::from_secs(5);
/// The smallest size agents are fitted to; a narrower preview cuts their lines.
const MIN_FIT: (u16, u16) = (60, 10);
/// What the TUI's list showed (`app::View`), for the read-only list beside
/// an attached agent.
pub const LIST_VIEW: &str = "ROSTER_LIST_VIEW";
/// A list narrower than this is of no use beside an attached agent.
const MIN_LIST: u16 = 20;

pub fn run(roster: &mut Roster) -> Result<()> {
    let _only = only_one(&roster.paths.state_dir)?;
    let keymap = Keymap::new(&roster.config.keys)?;
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let mut app = App::new(keymap, cwd, agent::available(&roster.config));
    app.order = roster.config.order.clone();
    app.notes = roster.config.notes.clone();
    app.account_limits = roster.config.account_limits();
    app.sidebar_usage = roster.config.sidebar_usage();
    app.card_subagents = roster.config.card_subagents();
    app.set_memory_limit(roster.config.memory_warning());
    app.set_grouped(roster.config.grouped());
    app.set_stopped_cards(roster.config.stopped_cards());
    app.config_file = roster.paths.config_file();
    app.models.chosen = form::Models::chosen_in(&roster.config);
    app.listing = Some(list_models(roster));
    for notice in roster.take_notices() {
        app.fail(notice);
    }
    let mut terminal = ratatui::init();
    restore_only_on_the_main_threads_panic();
    let result = event_loop(&mut terminal, roster, &mut app);
    ratatui::restore();
    result
}

/// ratatui's panic hook gives the terminal back whichever thread panicked: a
/// thread's (asking the accounts, reading what was spent) would leave the TUI
/// running in a terminal given back. Only the main thread's gives it back;
/// another thread's ends that thread, which whatever waits on it hears of
/// (an account's asking says roster failed), and prints nothing over the
/// screen.
fn restore_only_on_the_main_threads_panic() {
    let restoring = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            restoring(info);
        }
    }));
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
    app.account_limits = roster.config.account_limits();
    app.sidebar_usage = roster.config.sidebar_usage();
    app.card_subagents = roster.config.card_subagents();
    app.set_memory_limit(roster.config.memory_warning());
    app.set_grouped(roster.config.grouped());
    app.set_stopped_cards(roster.config.stopped_cards());
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
        // while something shows it, unless they're not to be asked.
        if app.account_limits && app.limits.due(app.sidebar_usage || app.mode == Mode::Usage) {
            app.limits.ask(roster.usage_reader());
        }
        app.limits.arrived();
        // What the agents spent, read in the background while a period of
        // it shows.
        let period = app.usage_period.filter(|_| app.mode == Mode::Usage);
        if app.tokens.due(period.is_some()) {
            let prices = crate::tokens::Prices::new(&roster.config.prices);
            app.tokens.read(tokens::scan(roster.paths.db(), prices));
        }
        app.tokens.arrived();
        if let Some(period) = period {
            app.tokens.sum(period, tokens::sum(roster.paths.db()));
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
            || app.tokens.summing()
        {
            wait = wait.min(Duration::from_millis(200));
        }
        if event::poll(wait)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let editing = |app: &App| app.note.as_ref().map(|n| n.shown().whose.clone());
            let opened = |app: &App| app.note.as_ref().map_or(0, |n| n.opened());
            let (selected, mode, was_editing) = (app.selected_key(), app.mode, editing(app));
            let was_opened = opened(app);
            let unfolded = app.unfolded.len();
            match app.on_key(key) {
                Some(Op::Quit) => return Ok(()),
                Some(op) => {
                    perform(terminal, roster, app, op)?;
                    // What it ended, or started, shows at once.
                    app.resources_read = None;
                    reload(roster, app);
                    last_reload = Instant::now();
                }
                None if app.mode != mode => reload(roster, app),
                // A card unfolded: what its subagents are, at once.
                None if app.unfolded.len() != unfolded => read_subagents(app),
                // A stopped session's panel shows its note unless it's open.
                None if app.selected_key() != selected || editing(app) != was_editing => {
                    // A note just opened is as saved now, not as last read.
                    if opened(app) > was_opened {
                        reload(roster, app);
                        last_reload = Instant::now();
                    }
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
    // What the sessions' processes use, every few seconds while it's to be
    // said of any using too much (the server view reads them itself);
    // nothing, if it can't be read.
    if app.memory_limit.is_none() {
        app.set_resources(HashMap::new());
        app.resources_read = None;
    } else if app.mode != Mode::Server
        && (app.resources_read).is_none_or(|at| at.elapsed() >= RESOURCES_EVERY)
    {
        app.set_resources(roster.resources().unwrap_or_default());
        app.resources_read = Some(Instant::now());
    }
    if let Ok(names) = roster.db.model_names() {
        app.models.names = names;
    }
    app.chat_project = roster.chat_project_id().ok().flatten();
    // What codex wrote of its limits since, which costs nothing to read.
    if app.sidebar_usage || app.mode == Mode::Usage {
        app.limits.read_transcripts(&app.sessions);
    }
    read_subagents(app);
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

/// How long after a subagent started what's read of it is read again, till
/// it's all there (its files written as it starts, its model as it first
/// replies); then what's there is what shows.
const SUBAGENT_SETTLES_MS: i64 = 2 * 60_000;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_main_threads_panic_gives_the_terminal_back() {
        // In a process of its own: the panic hook is the process's.
        const CHILD: &str = "ROSTER_TEST_PANIC_HOOK";
        if std::env::var_os(CHILD).is_none() {
            let name = "tui::tests::only_the_main_threads_panic_gives_the_terminal_back";
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && said.contains("1 passed"), "{said}");
            return;
        }
        use std::sync::atomic::{AtomicUsize, Ordering};
        static RESTORED: AtomicUsize = AtomicUsize::new(0);
        std::panic::set_hook(Box::new(|_| {
            RESTORED.fetch_add(1, Ordering::SeqCst);
        }));
        restore_only_on_the_main_threads_panic();
        // A thread's panic: the terminal stays the TUI's.
        let _ = std::thread::spawn(|| panic!("asking failed")).join();
        assert_eq!(RESTORED.load(Ordering::SeqCst), 0);
        // The main thread's gives it back.
        let main = std::thread::Builder::new().name("main".into());
        let _ = main.spawn(|| panic!("the TUI failed")).unwrap().join();
        assert_eq!(RESTORED.load(Ordering::SeqCst), 1);
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
