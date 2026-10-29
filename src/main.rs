//! roster: a keyboard-first terminal manager for AI coding agent sessions.

mod adopt;
mod agent;
mod cli;
mod cmd;
mod config;
mod db;
mod doctor;
mod engine;
mod keys;
mod launch;
mod model;
mod note;
mod paths;
mod print;
mod procs;
mod report;
mod search;
mod settings;
mod stats;
mod title;
mod tmux;
mod tokens;
mod tui;
mod usage;
mod util;

// What's in scope here is also cmd/'s (`use crate::*` there): an import
// this file doesn't use may be one of theirs.
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;

use adopt::{Adopt, RunOn};
use cli::*;
use engine::{Defaults, NewSession, Roster};
use model::{Session, State};
use print::{
    doctor_line, events, list_adoptable, list_projects, list_sessions, print_info, print_tokens,
    print_usage,
};

fn main() -> ExitCode {
    // Like other command-line tools, end quietly when what reads the output goes
    // away (`roster ls | head`) instead of panicking. Nothing roster writes to
    // is a pipe it could break otherwise: it only reads from its children.
    // SAFETY: nothing else runs yet.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("report") {
        report::run(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some("trust-folder") {
        crate::agent::trust_and_run(&args[2..]);
    }
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("roster: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    // Roster's own, in the background or under the list's screen: what
    // they'd say goes nowhere.
    let helper = matches!(
        cli.command,
        Some(Cmd::Deliver { .. } | Cmd::Retitle { auto: true, .. } | Cmd::Sidebar { .. })
    );
    let opened = if helper {
        Roster::open_quietly()
    } else {
        Roster::open()
    };
    let mut r = match opened {
        Ok(r) => r,
        // A config.toml roster can't read is what `config --edit` is for.
        Err(e) if matches!(cli.command, Some(Cmd::Config { edit: true, .. })) => {
            return cmd::config::edit_unread_config(e);
        }
        Err(e) => return Err(e),
    };
    let Some(command) = cli.command else {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            say_notices(&mut r);
            bail!("the TUI needs a terminal; see `roster --help` for the CLI");
        }
        let done = tui::run(&mut r);
        // What a TUI that didn't get going (another one open, say) didn't say.
        say_notices(&mut r);
        return done;
    };
    say_notices(&mut r);
    let done = run_command(&mut r, command);
    // What came up in it: a config the server it started didn't take, what a
    // refresh couldn't type in, whichever session's.
    say_notices(&mut r);
    for u in r.take_undelivered() {
        eprintln!("roster: {}", u.message);
    }
    done
}

fn run_command(r: &mut Roster, command: Cmd) -> Result<()> {
    match command {
        Cmd::Project(ProjectCmd::Add {
            name,
            path,
            defaults,
        }) => cmd::projects::add(r, name, path, defaults)?,
        Cmd::Project(ProjectCmd::Edit {
            project,
            name,
            path,
            defaults,
        }) => cmd::projects::edit(r, project, name, path, defaults)?,
        Cmd::Project(ProjectCmd::Ls) => list_projects(r)?,
        Cmd::Project(ProjectCmd::Rm { name, yes }) => cmd::projects::remove(r, name, yes)?,
        Cmd::New { project, session } => cmd::sessions::new(r, project, session)?,
        Cmd::Chat { session } => cmd::sessions::chat(r, session)?,
        Cmd::Ls {
            words,
            archived,
            project,
            sort,
            resources,
            json,
        } => cmd::sessions::list(r, words, archived, project, sort, resources, json)?,
        Cmd::Show { session } => cmd::sessions::show(r, session)?,
        Cmd::Attach {
            session,
            yes,
            again,
        } => cmd::lifecycle::attach(r, session, yes, again)?,
        Cmd::Start {
            session,
            yes,
            again,
        } => cmd::lifecycle::start(r, session, yes, again)?,
        Cmd::Stop {
            session,
            yes,
            kill: true,
        } => cmd::lifecycle::kill(r, session, yes)?,
        Cmd::Stop {
            session,
            yes,
            kill: false,
        } => cmd::lifecycle::stop(r, session, yes)?,
        Cmd::Restart {
            session,
            yes,
            again,
        } => cmd::lifecycle::restart(r, session, yes, again)?,
        Cmd::Archive { session, yes } => cmd::lifecycle::archive(r, session, yes)?,
        Cmd::Rm { session, yes } => cmd::lifecycle::delete(r, session, yes)?,
        Cmd::Restore { session, name } => cmd::lifecycle::restore(r, session, name)?,
        Cmd::Edit {
            session,
            name,
            title,
            model,
            effort,
        } => cmd::sessions::edit(r, session, name, title, model, effort)?,
        // Without a conversation, or asked to: those that can be.
        Cmd::Adopt(mut args) => match args.conversation.take() {
            Some(conversation) if !args.list => cmd::sessions::adopt(r, conversation, args)?,
            _ => list_adoptable(r, args.all)?,
        },
        Cmd::Info { session } => cmd::sessions::info(r, session)?,
        Cmd::Note(args) => cmd::notes::note(r, args)?,
        Cmd::Retitle { session, auto } => cmd::sessions::retitle(r, session, auto)?,
        Cmd::Read { session } => cmd::sessions::read(r, session)?,
        Cmd::Unread { session } => cmd::sessions::unread(r, session)?,
        Cmd::Config {
            change,
            defaults,
            edit,
        } => cmd::config::config(r, change, defaults, edit)?,
        Cmd::Sidebar { session } => tui::run_sidebar(r, &session)?,
        Cmd::Doctor { fix, yes } => cmd::doctor::doctor(r, fix, yes)?,
        Cmd::Usage { json } => print_usage(r, json)?,
        Cmd::Tokens { days, json } => print_tokens(r, days, json)?,
        Cmd::Events { follow, session } => events(r, follow, session.as_deref())?,
        // Dispatched in main before parsing; only reached via unusual argument orders.
        Cmd::Report { .. } | Cmd::TrustFolder { .. } => {}
        Cmd::Deliver { session } => cmd::sessions::deliver(r, session)?,
    }
    Ok(())
}

/// Says on stderr what came up that isn't worth failing for (`take_notices`).
fn say_notices(r: &mut Roster) {
    for notice in r.take_notices() {
        eprintln!("roster: {notice}");
    }
}

fn confirm(question: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        bail!("{question} Pass --yes to confirm.");
    }
    eprint!("{question} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}
