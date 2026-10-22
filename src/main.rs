//! roster: a keyboard-first terminal manager for AI coding agent sessions.

mod adopt;
mod agent;
mod config;
mod db;
mod doctor;
mod engine;
mod keys;
mod launch;
mod model;
mod paths;
mod print;
mod report;
mod search;
mod stats;
mod title;
mod tmux;
mod tui;
mod util;

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use adopt::Adopt;
use engine::{Defaults, NewSession, Roster};
use model::{Session, State};
use print::{doctor_line, events, list_adoptable, list_projects, list_sessions, print_info};

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
    /// Add, edit, list or remove projects
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Start a new agent session in a project
    New {
        project: String,
        #[command(flatten)]
        session: SessionArgs,
    },
    /// Start a chat: a session that needs no project (it runs in the built-in chats project)
    Chat {
        #[command(flatten)]
        session: SessionArgs,
    },
    /// Take over a Claude or pi conversation started elsewhere, resuming it here
    Adopt {
        /// The conversation's id (4+ characters are enough); without it, lists them
        conversation: Option<String>,
        /// List the conversations that can be adopted
        #[arg(long)]
        list: bool,
        /// Also list those outside your projects' directories
        #[arg(long)]
        all: bool,
        /// Project to put it in, created for its directory if new (default: the project
        /// holding its directory, else a new one named after the directory)
        #[arg(long)]
        project: Option<String>,
        /// The agent's name in the project (default: claude-1, claude-2, …)
        #[arg(long)]
        name: Option<String>,
        /// Claude model (default: the project's; "default" for Claude's own)
        #[arg(long)]
        model: Option<String>,
        /// Claude effort (default: the project's; "default" for Claude's own)
        #[arg(long)]
        effort: Option<String>,
        /// Adopt a copy and leave the original alone
        #[arg(long)]
        fork: bool,
        /// Adopt it even if it's open in its agent (Claude tells; pi doesn't)
        #[arg(short, long)]
        yes: bool,
        /// Attach to it right away
        #[arg(short, long)]
        attach: bool,
    },
    /// Rename a session, set its title, or change its model and effort (applies on its next start)
    Edit {
        session: String,
        /// The agent's name in the project
        #[arg(long)]
        name: Option<String>,
        /// A title of your own ("" follows the agent's title again)
        #[arg(long)]
        title: Option<String>,
        /// Model for Claude or pi ("default" for the agent's own)
        #[arg(long)]
        model: Option<String>,
        /// Effort for Claude, thinking level for pi ("default" for the agent's own)
        #[arg(long)]
        effort: Option<String>,
    },
    /// List sessions and their state (● = unread)
    Ls {
        /// Only sessions matching all these words, by project, title or name (fuzzy, like `/` in the TUI)
        words: Vec<String>,
        /// List archived sessions instead
        #[arg(long)]
        archived: bool,
        /// Only this project's sessions
        #[arg(long)]
        project: Option<String>,
        /// `project` (default) or `attention`: needs input, idle, stopped, working, like the TUI's shelves
        #[arg(long, default_value = "project", value_parser = ["project", "attention"])]
        sort: String,
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
    /// Archive a session: its agent stops, and `roster restore` brings it back
    Archive {
        session: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Delete an archived session for good (its Claude or pi conversation stays, and can be adopted again)
    Rm {
        session: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Restore an archived session
    Restore {
        session: String,
        /// Restore under a new name
        #[arg(long)]
        name: Option<String>,
    },
    /// A session's details and statistics (created, prompts, time worked, context, tokens, cost, lines)
    Info { session: String },
    /// Give a Claude or pi session a fresh title, made by its agent from its recent prompts (title_model in config.toml picks the model)
    Retitle {
        session: String,
        /// As roster does on its own after a first turn: the agent's title,
        /// unless it has one
        #[arg(long, hide = true)]
        auto: bool,
    },
    /// Mark a session read
    Read { session: String },
    /// Mark a session unread
    Unread { session: String },
    /// Check roster's tmux server and database against each other
    Doctor {
        /// Kill orphaned tmux sessions
        #[arg(long)]
        fix: bool,
        #[arg(short, long)]
        yes: bool,
    },
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

/// What a new session or chat should be.
#[derive(clap::Args)]
struct SessionArgs {
    /// The agent's name in the project, a role say (default: <agent>-1, <agent>-2, …)
    #[arg(short, long)]
    name: Option<String>,
    /// claude, pi, or an agent from [agents] in config.toml (default: the project's, else default_agent)
    #[arg(long)]
    agent: Option<String>,
    /// Model for Claude or pi (default: the project's; "default" for the agent's own)
    #[arg(long)]
    model: Option<String>,
    /// Effort for Claude, thinking level for pi (default: the project's; "default" for the agent's own)
    #[arg(long)]
    effort: Option<String>,
    /// A first prompt, typed into the agent once it's ready
    #[arg(long)]
    prompt: Option<String>,
    /// Attach to it right away
    #[arg(short, long)]
    attach: bool,
}

impl SessionArgs {
    /// The request for a session in `p`, whose defaults fill in what's not given.
    fn request(self, p: model::Project) -> (NewSession, bool) {
        let request = NewSession {
            project: p.name,
            name: self.name,
            agent: self.agent,
            model: setting(self.model).unwrap_or(p.model),
            effort: setting(self.effort).unwrap_or(p.effort),
            prompt: self.prompt,
        };
        (request, self.attach)
    }
}

/// A project's defaults for new sessions; "default" clears one.
#[derive(clap::Args)]
struct DefaultsArgs {
    /// Default agent for new sessions
    #[arg(long)]
    agent: Option<String>,
    /// Default model: for Claude opus, sonnet, haiku, fable or a full name; for pi any it knows
    #[arg(long)]
    model: Option<String>,
    /// Default effort: for Claude low … max; for pi a thinking level, off … max
    #[arg(long)]
    effort: Option<String>,
}

/// None: not given; Some(None): "default" (no value); Some(Some(v)): v.
fn setting(value: Option<String>) -> Option<Option<String>> {
    value.map(|v| (v != "default").then_some(v))
}

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
                    "  with archived session {} (conversation {conversation})",
                    s.name
                );
            }
        }
        Cmd::New { project, session } => {
            let p = r.db.project(&project)?.with_context(|| {
                format!("no project {project:?} (add it with `roster project add`)")
            })?;
            let (request, attach) = session.request(p);
            start_session(&mut r, request, attach)?;
        }
        Cmd::Chat { session } => {
            let (request, attach) = session.request(r.chat_project()?);
            start_session(&mut r, request, attach)?;
        }
        Cmd::Ls {
            words,
            archived,
            project,
            sort,
            json,
        } => {
            r.settle()?;
            let query = words.join(" ");
            let by_attention = sort == "attention";
            list_sessions(&r, &query, archived, project.as_deref(), by_attention, json)?;
        }
        Cmd::Show { session } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            print!("{}", r.screen(&s, io::stdout().is_terminal())?);
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
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            // A running one fails to start anyway; don't ask about it first.
            if s.state == State::Stopped && !confirm_new_conversation(&r, &s, yes)? {
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
        Cmd::Archive { session, yes } => {
            let s = r.resolve(&session, false)?;
            if !confirm(
                &format!(
                    "Archive {}? Its agent stops (`roster restore` brings it back)",
                    s.label()
                ),
                yes,
            )? {
                return Ok(());
            }
            r.archive(&s.id)?;
            println!("Archived {} ({})", s.label(), s.id);
        }
        Cmd::Rm { session, yes } => {
            let s = match r.resolve(&session, true) {
                Ok(s) => s,
                Err(e) => match r.resolve(&session, false) {
                    Ok(live) => bail!(
                        "{} isn't archived; only archived sessions can be deleted (`roster archive {}`)",
                        live.label(),
                        live.label()
                    ),
                    Err(_) => return Err(e),
                },
            };
            let kept = if agent::adapter(&s.agent).adopts() {
                format!(
                    "; its {} conversation stays and can be adopted again",
                    util::capitalize(&s.agent)
                )
            } else {
                String::new()
            };
            if !confirm(
                &format!("Delete {} for good? Roster forgets it{kept}", s.label()),
                yes,
            )? {
                return Ok(());
            }
            r.delete(&s.id)?;
            println!("Deleted {} ({}) for good", s.label(), s.id);
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
            title,
            model,
            effort,
        } => {
            let s = r.resolve(&session, false)?;
            let s = r.update_session(
                &s.id,
                name.as_deref().unwrap_or(&s.name),
                title.or(s.title_override.clone()),
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
        Cmd::Adopt {
            conversation: None,
            all,
            ..
        }
        | Cmd::Adopt {
            list: true, all, ..
        } => {
            list_adoptable(&r, all)?;
        }
        Cmd::Adopt {
            conversation: Some(conversation),
            project,
            name,
            model,
            effort,
            fork,
            yes,
            attach,
            ..
        } => {
            let s = r.adopt(Adopt {
                conversation,
                project,
                name,
                model: setting(model),
                effort: setting(effort),
                fork,
                force: yes,
            })?;
            let copy = if fork { " as a copy" } else { "" };
            println!("Adopted {}{copy} ({})", s.label(), s.id);
            if attach {
                r.attach(&s.id)?;
            }
        }
        Cmd::Info { session } => {
            r.refresh()?;
            let s = r.resolve_any(&session)?;
            print_info(&r, &s);
        }
        Cmd::Retitle { session, auto } => {
            let s = r.resolve(&session, false)?;
            let title = if auto {
                r.auto_title(&s.id)?
            } else {
                r.generate_title(&s.id)?
            };
            println!("{title}");
        }
        Cmd::Read { session } => {
            let s = r.resolve(&session, false)?;
            r.set_unread(&s.id, false)?;
        }
        Cmd::Unread { session } => {
            let s = r.resolve(&session, false)?;
            r.set_unread(&s.id, true)?;
        }
        Cmd::Doctor { fix, yes } => {
            let mut diagnosis = r.diagnose()?;
            if fix && !diagnosis.orphans.is_empty() {
                let question =
                    format!("Kill {} orphaned tmux session(s)?", diagnosis.orphans.len());
                if confirm(&question, yes)? {
                    let killed = r.kill_orphans()?;
                    println!("Killed {killed} orphaned tmux session(s).\n");
                    diagnosis = r.diagnose()?;
                }
            }
            let tty = io::stdout().is_terminal();
            for line in &diagnosis.lines {
                println!("{}", doctor_line(line, tty));
            }
            if diagnosis.problems > 0 {
                return Err(anyhow::anyhow!("{} problem(s)", diagnosis.problems));
            }
        }
        Cmd::Events { follow, session } => events(&r, follow, session.as_deref())?,
        // Dispatched in main before parsing; only reached via unusual argument orders.
        Cmd::Report { .. } => {}
    }
    Ok(())
}

/// Starts a new session, waits for its first prompt to go in (unless attaching,
/// which shows it), and attaches if asked.
fn start_session(r: &mut Roster, request: NewSession, attach: bool) -> Result<()> {
    let waits = request.prompt.is_some() && !attach;
    let s = r.new_session(request)?;
    println!("Started {} ({})", s.label(), s.id);
    if waits {
        if r.wait_for_prompt(&s.id, Duration::from_secs(60))? {
            println!("Sent the first prompt");
        } else {
            println!(
                "The agent isn't ready yet (it may be waiting for you, e.g. to trust the folder);"
            );
            println!("the prompt is typed in once it is, by the next roster command or the TUI");
        }
    }
    if attach {
        r.attach(&s.id)?;
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
