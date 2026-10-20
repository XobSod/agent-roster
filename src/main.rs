//! roster: a keyboard-first terminal manager for AI coding agent sessions.

mod agent;
mod config;
mod db;
mod engine;
mod keys;
mod launch;
mod model;
mod paths;
mod report;
mod tmux;
mod tui;
mod util;

use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use engine::{Defaults, NewSession, Roster};
use model::{Session, State};

#[derive(Parser)]
#[command(
    name = "roster",
    version,
    about = "Keyboard-first manager for AI coding agent sessions",
    after_help = "Sessions are addressed as project/name, name, or id (a prefix of 4+ characters is enough)."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add, list or remove projects
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Start a new agent session in a project
    New {
        project: String,
        #[arg(short, long)]
        name: Option<String>,
        /// claude, or an agent from [agents] in config.toml (default: the project's)
        #[arg(long)]
        agent: Option<String>,
        /// Claude model (default: the project's; "default" for Claude's own)
        #[arg(long)]
        model: Option<String>,
        /// Claude effort (default: the project's; "default" for Claude's own)
        #[arg(long)]
        effort: Option<String>,
        /// A first prompt, typed into the agent once it's ready
        #[arg(long)]
        prompt: Option<String>,
        /// Attach to it right away
        #[arg(short, long)]
        attach: bool,
    },
    /// Rename a session or change its model and effort (applies on its next start)
    Edit {
        session: String,
        #[arg(long)]
        name: Option<String>,
        /// Claude model ("default" for Claude's own)
        #[arg(long)]
        model: Option<String>,
        /// Claude effort ("default" for Claude's own)
        #[arg(long)]
        effort: Option<String>,
    },
    /// List sessions and their state (● = unread)
    Ls {
        /// List deleted sessions instead
        #[arg(long)]
        deleted: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print a session's current screen
    Show { session: String },
    /// Attach to a session (starting it if stopped); the back key (Ctrl+Q) returns and marks it read
    Attach {
        session: String,
        /// Start a stopped agent that can't resume without asking
        #[arg(short, long)]
        yes: bool,
    },
    /// Start a stopped session, resuming its conversation
    Start {
        session: String,
        /// Don't ask before starting a new conversation for agents that can't resume
        #[arg(short, long)]
        yes: bool,
    },
    /// Stop a session's agent, keeping the session
    Stop { session: String },
    /// Stop and start a session, resuming its conversation
    Restart {
        session: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Delete a session (`roster restore` brings it back)
    Rm {
        session: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Restore a deleted session
    Restore {
        session: String,
        /// Restore under a new name
        #[arg(long)]
        name: Option<String>,
    },
    /// Mark a session read
    Read { session: String },
    /// Mark a session unread
    Unread { session: String },
    /// Show status events reported by agent hooks
    Events {
        #[arg(short, long)]
        follow: bool,
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Called by agent hooks; always silent, always exits 0
    #[command(hide = true)]
    Report { kind: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Add a project: a name and its origin directory
    Add {
        name: String,
        path: PathBuf,
        #[command(flatten)]
        defaults: DefaultsArgs,
    },
    /// Rename, move, or change the defaults of a project
    Edit {
        project: String,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New origin directory (existing sessions keep theirs)
        #[arg(long)]
        path: Option<PathBuf>,
        #[command(flatten)]
        defaults: DefaultsArgs,
    },
    /// List projects
    Ls,
    /// Remove a project (it must have no live sessions)
    Rm { name: String },
}

/// A project's defaults for new sessions; "default" clears one.
#[derive(clap::Args)]
struct DefaultsArgs {
    /// Default agent for new sessions
    #[arg(long)]
    agent: Option<String>,
    /// Default Claude model (opus, sonnet, haiku, fable, or a full name)
    #[arg(long)]
    model: Option<String>,
    /// Default Claude effort (low, medium, high, xhigh, max)
    #[arg(long)]
    effort: Option<String>,
}

/// None: not given; Some(None): "default" (no value); Some(Some(v)): v.
fn setting(value: Option<String>) -> Option<Option<String>> {
    value.map(|v| (v != "default").then_some(v))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("report") {
        report::run(&args[2..]);
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
    let mut r = Roster::open()?;
    let Some(command) = cli.command else {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!("the TUI needs a terminal; see `roster --help` for the CLI");
        }
        return tui::run(&mut r);
    };
    match command {
        Cmd::Project(ProjectCmd::Add {
            name,
            path,
            defaults,
        }) => {
            let defaults = Defaults {
                agent: setting(defaults.agent).flatten(),
                model: setting(defaults.model).flatten(),
                effort: setting(defaults.effort).flatten(),
            };
            let p = r.add_project(&name, &path, defaults)?;
            println!("Added project {} ({})", p.name, p.origin.display());
        }
        Cmd::Project(ProjectCmd::Edit {
            project,
            name,
            path,
            defaults,
        }) => {
            let p = r.projects()?.into_iter().find(|p| p.name == project);
            let p = p.with_context(|| format!("no project {project:?}"))?;
            let updated = Defaults {
                agent: setting(defaults.agent).unwrap_or(p.agent.clone()),
                model: setting(defaults.model).unwrap_or(p.model.clone()),
                effort: setting(defaults.effort).unwrap_or(p.effort.clone()),
            };
            let name = name.unwrap_or(p.name.clone());
            let p = r.update_project(p.id, &name, &path.unwrap_or(p.origin.clone()), updated)?;
            println!("Updated project {} ({})", p.name, p.origin.display());
        }
        Cmd::Project(ProjectCmd::Ls) => list_projects(&r)?,
        Cmd::Project(ProjectCmd::Rm { name }) => {
            let purged = r.remove_project(&name)?;
            println!("Removed project {name}");
            for s in purged {
                let conversation = s.conversation_id.as_deref().unwrap_or("none");
                println!(
                    "  with deleted session {} (conversation {conversation})",
                    s.name
                );
            }
        }
        Cmd::New {
            project,
            name,
            agent,
            model,
            effort,
            prompt,
            attach,
        } => {
            let p = r.projects()?.into_iter().find(|p| p.name == project);
            let p = p.with_context(|| {
                format!("no project {project:?} (add it with `roster project add`)")
            })?;
            let waits = prompt.is_some() && !attach;
            let s = r.new_session(NewSession {
                project,
                name,
                agent,
                model: setting(model).unwrap_or(p.model),
                effort: setting(effort).unwrap_or(p.effort),
                prompt,
            })?;
            println!("Started {} ({})", s.label(), s.id);
            if waits {
                if r.wait_for_prompt(&s.id, Duration::from_secs(60))? {
                    println!("Sent the first prompt");
                } else {
                    println!(
                        "The agent isn't ready yet (it may be waiting for you, e.g. to trust the folder);"
                    );
                    println!(
                        "the prompt is typed in once it is, by the next roster command or the TUI"
                    );
                }
            }
            if attach {
                r.attach(&s.id)?;
            }
        }
        Cmd::Ls { deleted, json } => {
            r.settle()?;
            list_sessions(&r, deleted, json)?;
        }
        Cmd::Show { session } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            print!("{}", r.screen(&s.id, io::stdout().is_terminal())?);
        }
        Cmd::Attach { session, yes } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            if s.state == State::Stopped && !confirm_new_conversation(&r, &s, yes)? {
                return Ok(());
            }
            r.attach(&s.id)?;
        }
        Cmd::Start { session, yes } => {
            let s = r.resolve(&session, false)?;
            if !confirm_new_conversation(&r, &s, yes)? {
                return Ok(());
            }
            r.start(&s.id)?;
            println!("Started {}", s.label());
        }
        Cmd::Stop { session } => {
            let s = r.resolve(&session, false)?;
            if r.stop(&s.id)? {
                println!("Stopped {}", s.label());
            } else {
                println!("{} wasn't running", s.label());
            }
        }
        Cmd::Restart { session, yes } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            let asked = if !r.can_resume(&s) {
                confirm_new_conversation(&r, &s, yes)?
            } else if s.state == State::Working {
                confirm(&format!("{} is working. Restart it?", s.label()), yes)?
            } else {
                true
            };
            if !asked {
                return Ok(());
            }
            r.restart(&s.id)?;
            println!("Restarted {}", s.label());
        }
        Cmd::Rm { session, yes } => {
            let s = r.resolve(&session, false)?;
            if !confirm(
                &format!("Delete {}? (`roster restore` brings it back)", s.label()),
                yes,
            )? {
                return Ok(());
            }
            r.delete(&s.id)?;
            println!("Deleted {} ({})", s.label(), s.id);
        }
        Cmd::Restore { session, name } => {
            let s = r.resolve(&session, true)?;
            let s = r.restore(&s.id, name.as_deref())?;
            println!(
                "Restored {} (stopped; `roster start {}` resumes it)",
                s.label(),
                s.label()
            );
        }
        Cmd::Edit {
            session,
            name,
            model,
            effort,
        } => {
            let s = r.resolve(&session, false)?;
            let s = r.update_session(
                &s.id,
                name.as_deref().unwrap_or(&s.name),
                setting(model).unwrap_or(s.model.clone()),
                setting(effort).unwrap_or(s.effort.clone()),
            )?;
            println!("Updated {}", s.label());
            if s.state != State::Stopped {
                println!(
                    "The agent picks up a new model or effort on its next start (`roster restart {}`)",
                    s.label()
                );
            }
        }
        Cmd::Read { session } => {
            let s = r.resolve(&session, false)?;
            r.set_unread(&s.id, false)?;
        }
        Cmd::Unread { session } => {
            let s = r.resolve(&session, false)?;
            r.set_unread(&s.id, true)?;
        }
        Cmd::Events { follow, session } => events(&r, follow, session.as_deref())?,
        // Dispatched in main before parsing; only reached via unusual argument orders.
        Cmd::Report { .. } => {}
    }
    Ok(())
}

/// Asks before (re)starting an agent that can't resume its conversation.
fn confirm_new_conversation(r: &Roster, s: &Session, yes: bool) -> Result<bool> {
    if r.can_resume(s) {
        return Ok(true);
    }
    let question = format!(
        "{} ({}) can't resume its conversation. Start a new one?",
        s.label(),
        s.agent
    );
    confirm(&question, yes)
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

fn list_projects(r: &Roster) -> Result<()> {
    let projects = r.projects()?;
    if projects.is_empty() {
        println!("No projects. Add one with `roster project add <name> <path>`.");
        return Ok(());
    }
    let sessions = r.sessions(false)?;
    let rows: Vec<Vec<String>> = projects
        .iter()
        .map(|p| {
            let count = sessions.iter().filter(|s| s.project_id == p.id).count();
            vec![
                p.name.clone(),
                p.origin.display().to_string(),
                count.to_string(),
            ]
        })
        .collect();
    print_table(&["PROJECT", "ORIGIN", "SESSIONS"], &rows);
    Ok(())
}

fn list_sessions(r: &Roster, deleted: bool, json: bool) -> Result<()> {
    let sessions: Vec<Session> = r
        .sessions(deleted)?
        .into_iter()
        .filter(|s| s.deleted_at.is_some() == deleted)
        .collect();
    if json {
        let items: Vec<_> = sessions
            .iter()
            .map(|s| {
                json!({
                    "id": s.id, "project": s.project, "name": s.name, "agent": s.agent,
                    "state": s.state.as_str(), "unread": s.unread, "exit_status": s.exit_status,
                    "conversation_id": s.conversation_id, "workdir": s.workdir, "deleted_at": s.deleted_at,
                    "title": s.title, "model": s.model, "effort": s.effort,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if sessions.is_empty() {
        if deleted {
            println!("No deleted sessions.");
        } else {
            println!("No sessions. Start one with `roster new <project>`.");
        }
        return Ok(());
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let state = match (s.state, s.exit_status) {
                (State::Stopped, Some(code)) if code != 0 => format!("stopped (exit {code})"),
                (state, _) => state.as_str().to_string(),
            };
            let marker = if s.unread { "●" } else { " " };
            vec![
                s.project.clone(),
                s.name.clone(),
                format!("{marker} {state}"),
                s.agent.clone(),
                s.id.clone(),
            ]
        })
        .collect();
    print_table(&["PROJECT", "SESSION", "  STATE", "AGENT", "ID"], &rows);
    Ok(())
}

fn print_table(header: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w - c.chars().count())))
            .collect();
        println!("{}", padded.join("  ").trim_end());
    };
    line(header.to_vec());
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
}

fn events(r: &Roster, follow: bool, session: Option<&str>) -> Result<()> {
    let filter = match session {
        Some(addr) => Some(
            r.resolve(addr, false)
                .or_else(|_| r.resolve(addr, true))?
                .id,
        ),
        None => None,
    };
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut last = r.db.event_id_before_last(20, filter.as_deref())?;
    loop {
        for e in r.db.events_after(last, filter.as_deref(), 1000)? {
            if !labels.contains_key(&e.session_id) {
                labels = r
                    .sessions(true)?
                    .into_iter()
                    .map(|s| (s.id.clone(), s.label()))
                    .collect();
            }
            let label = labels.get(&e.session_id).unwrap_or(&e.session_id);
            println!(
                "{}  {label}  {}  {}",
                util::local_time(e.at),
                e.kind,
                summarize(e.payload.as_deref())
            );
            last = e.id;
        }
        if !follow {
            return Ok(());
        }
        io::stdout().flush()?;
        sleep(Duration::from_millis(500));
    }
}

/// The interesting parts of a hook payload.
fn summarize(payload: Option<&str>) -> String {
    let Some(payload) = payload else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return payload.chars().take(80).collect();
    };
    let mut parts = Vec::new();
    if let Some(hook) = v["hook_event_name"].as_str() {
        parts.push(hook.to_string());
    }
    for key in ["source", "tool_name", "reason"] {
        if let Some(value) = v[key].as_str() {
            parts.push(format!("{key}={value}"));
        }
    }
    if let Some(id) = v["session_id"].as_str() {
        parts.push(format!("conversation={}", id.get(..8).unwrap_or(id)));
    }
    parts.join(" ")
}
