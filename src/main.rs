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
mod note;
mod paths;
mod print;
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

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use adopt::{Adopt, RunOn};
use engine::{Defaults, NewSession, Roster};
use model::{Session, State};
use print::{
    doctor_line, events, list_adoptable, list_projects, list_sessions, print_info, print_tokens,
    print_usage,
};

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
    /// Take over a Claude, pi or codex conversation started elsewhere, resuming it here
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
        /// The agent's name in the project (default: its agent's and a number, claude-1, pi-2, …)
        #[arg(long)]
        name: Option<String>,
        /// Go on with the defaults, not what it last ran on: the project's model and effort if it
        /// runs the same agent, else the agent's in config.toml, else its own
        #[arg(long)]
        defaults: bool,
        /// Model (default: what it last ran on, or with --defaults the defaults; "default" for those)
        #[arg(long)]
        model: Option<String>,
        /// Effort (default: what it last ran on, or with --defaults the defaults; "default" for those)
        #[arg(long)]
        effort: Option<String>,
        /// Adopt a copy and leave the original alone
        #[arg(long)]
        fork: bool,
        /// Move it without asking even if it's open in its agent (Claude tells; pi and codex don't)
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
        /// Model for Claude, pi or codex ("default" for the agent's in config.toml, else its own)
        #[arg(long)]
        model: Option<String>,
        /// Effort for Claude or codex, thinking level for pi ("default" for the agent's in config.toml, else its own)
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
        #[command(flatten)]
        again: PromptAgain,
    },
    /// Start a stopped session, resuming its conversation
    Start {
        session: String,
        /// Don't ask before starting a new conversation for agents that can't resume
        #[arg(short, long)]
        yes: bool,
        #[command(flatten)]
        again: PromptAgain,
    },
    /// Stop a session's agent, keeping the session (asks first if it's working)
    Stop {
        session: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Stop and start a session, resuming its conversation
    Restart {
        session: String,
        #[arg(short, long)]
        yes: bool,
        #[command(flatten)]
        again: PromptAgain,
    },
    /// Archive a session: its agent stops, and `roster restore` brings it back
    Archive {
        session: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// Delete a session for good, archived or not: its agent stops (its Claude, pi or codex conversation stays, and can be adopted again)
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
    /// Print a session's or a project's note, or change it (in the TUI: s o, p o)
    Note {
        /// Whose note: a session's (inside an agent, left out: its own
        /// session's); with --project alone, its project's
        session: Option<String>,
        /// A project's note: this project's, or the session's project's (inside
        /// an agent, its own session's)
        #[arg(long, value_name = "PROJECT", num_args = 0..=1, default_missing_value = "")]
        project: Option<String>,
        /// Replace it with this text ("-" reads it from stdin)
        #[arg(long, allow_hyphen_values = true, conflicts_with_all = ["append", "clear", "edit"])]
        set: Option<String>,
        /// Add this line at its end
        #[arg(long, allow_hyphen_values = true, conflicts_with_all = ["clear", "edit"])]
        append: Option<String>,
        /// Remove it
        #[arg(long, conflicts_with = "edit")]
        clear: bool,
        /// Open it in $VISUAL or $EDITOR
        #[arg(long)]
        edit: bool,
        /// Pin this line of it (from 1): the preview shows it and the one below
        #[arg(long, value_name = "LINE", conflicts_with_all = ["set", "append", "clear", "edit", "unpin"])]
        pin: Option<usize>,
        /// Unpin its pinned line: the preview shows its first lines
        #[arg(long, conflicts_with_all = ["set", "append", "clear", "edit"])]
        unpin: bool,
    },
    /// Give a Claude, pi or codex session a fresh title, made by its agent from its recent prompts (title_model in config.toml picks the model)
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
    /// Every setting of config.toml: its value, whether you set it, what it does
    #[command(args_conflicts_with_subcommands = true)]
    Config {
        #[command(subcommand)]
        change: Option<ConfigCmd>,
        /// Print a config.toml with every setting commented out at its default
        #[arg(long)]
        defaults: bool,
        /// Open config.toml in $VISUAL or $EDITOR, written first from those
        /// defaults if there's none
        #[arg(long, conflicts_with = "defaults")]
        edit: bool,
    },
    /// What your Claude and codex accounts have used of their limits: each window, and when it starts over
    Usage {
        #[arg(long)]
        json: bool,
    },
    /// What the agents spent: tokens of each kind and their cost at API prices, by agent and model, from every conversation they keep on this machine
    Tokens {
        /// How many days back, up to now
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(i64).range(1..))]
        days: i64,
        #[arg(long)]
        json: bool,
    },
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
    /// Run by a session's launch in its agent's place: trusts the folder for
    /// it, in its environment, then runs it (`trust_project_folders`)
    #[command(hide = true)]
    TrustFolder {
        agent: String,
        #[arg(last = true)]
        argv: Vec<String>,
    },
    /// Types a session's first prompt once its agent is ready: what `new
    /// --attach` leaves running while you're attached, when nothing else
    /// refreshes
    #[command(hide = true)]
    Deliver { session: String },
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
    /// Remove a project (it must have no live sessions); its archived sessions go for good
    Rm {
        name: String,
        #[arg(short, long)]
        yes: bool,
    },
}

/// Whether a start gives again the first prompt its agent exited before
/// saying it took (Codex, at a screen it showed first, say); asked when
/// neither is given.
#[derive(clap::Args)]
struct PromptAgain {
    /// Give the agent its first prompt again, one it exited before saying it took
    #[arg(long, conflicts_with = "no_prompt_again")]
    prompt_again: bool,
    /// Start without that first prompt, kept to be asked about again
    #[arg(long)]
    no_prompt_again: bool,
}

/// What a new session or chat should be.
#[derive(clap::Args)]
struct SessionArgs {
    /// The agent's name in the project, a role say (default: <agent>-1, <agent>-2, …)
    #[arg(short, long)]
    name: Option<String>,
    /// claude, pi, codex, or an agent from [agents] in config.toml (default: the project's, else default_agent)
    #[arg(long)]
    agent: Option<String>,
    /// Model for Claude, pi or codex (default: the project's, else the agent's in config.toml, else its own)
    #[arg(long)]
    model: Option<String>,
    /// Effort for Claude or codex, thinking level for pi (default: the project's, else the agent's in config.toml, else its own)
    #[arg(long)]
    effort: Option<String>,
    /// A first prompt, typed into the agent once it's ready (codex gets it on its command line)
    #[arg(long)]
    prompt: Option<String>,
    /// Attach to it right away
    #[arg(short, long)]
    attach: bool,
}

impl SessionArgs {
    /// The request for a session in `p`, whose defaults fill in what's not
    /// given (its model and effort for its own agent: `default_agent` if it
    /// picks none).
    fn request(self, p: model::Project, default_agent: &str) -> (NewSession, bool) {
        let project_agent = p.agent.as_deref().unwrap_or(default_agent);
        let same = self.agent.as_deref().is_none_or(|a| a == project_agent);
        let (model, effort) = if same {
            (p.model, p.effort)
        } else {
            (None, None)
        };
        let request = NewSession {
            project: p.name,
            name: self.name,
            agent: self.agent,
            model: setting(self.model).unwrap_or(model),
            effort: setting(self.effort).unwrap_or(effort),
            prompt: self.prompt,
        };
        (request, self.attach)
    }
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Change a setting in config.toml (keeping the rest of it): `set agents.pi.title_model
    /// claude-haiku-4-5`, `set keys.up "k Up"`
    Set { setting: String, value: String },
    /// Take a setting out of config.toml, so its default holds again; a whole
    /// table of them (`unset agents.aider`) asks first
    Unset {
        setting: String,
        /// Take a whole table out without asking
        #[arg(short, long)]
        yes: bool,
    },
}

/// A project's defaults for new sessions; "default" clears one.
#[derive(clap::Args)]
struct DefaultsArgs {
    /// Default agent for new sessions
    #[arg(long)]
    agent: Option<String>,
    /// Default model: for Claude opus, sonnet, haiku, fable or a full name; for pi or codex any it knows
    #[arg(long)]
    model: Option<String>,
    /// Default effort: for Claude low … max; for codex none … ultra; for pi a thinking level, off … max
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
    // Roster's own, in the background: what they'd say goes nowhere.
    let helper = matches!(
        cli.command,
        Some(Cmd::Deliver { .. } | Cmd::Retitle { auto: true, .. })
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
            return edit_unread_config(e);
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
        Cmd::Project(ProjectCmd::Ls) => list_projects(r)?,
        Cmd::Project(ProjectCmd::Rm { name, yes }) => {
            if !confirm(&r.removal_question(&name)?, yes)? {
                return Ok(());
            }
            let purged = r.remove_project(&name)?;
            println!("Removed project {name}");
            for s in purged {
                let conversation = s.conversation_id.as_deref().unwrap_or("none");
                let note = if s.note.is_some() {
                    ", and its note"
                } else {
                    ""
                };
                println!(
                    "  with archived session {} (conversation {conversation}){note}",
                    s.name
                );
            }
        }
        Cmd::New { project, session } => {
            let p = r.db.project(&project)?.with_context(|| {
                format!("no project {project:?} (add it with `roster project add`)")
            })?;
            let (request, attach) = session.request(p, r.config.default_agent());
            start_session(r, request, attach)?;
        }
        Cmd::Chat { session } => {
            let chats = r.chat_project()?;
            let (request, attach) = session.request(chats, r.config.default_agent());
            start_session(r, request, attach)?;
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
            list_sessions(r, &query, archived, project.as_deref(), by_attention, json)?;
        }
        Cmd::Show { session } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            print!("{}", r.screen(&s, io::stdout().is_terminal())?);
        }
        Cmd::Attach {
            session,
            yes,
            again,
        } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            if s.state == State::Stopped {
                // Its first prompt not seen taken asks that instead: a new
                // conversation either way.
                let give = match s.given_prompt {
                    Some(_) => match give_prompt_again(&s, &again)? {
                        Some(give) => give,
                        None => return Ok(()),
                    },
                    None if !confirm_new_conversation(r, &s, yes)? => return Ok(()),
                    None => false,
                };
                // Here, so what starting says comes before tmux takes the
                // terminal; unless another roster started it while asking.
                r.refresh()?;
                if r.db.session(&s.id)?.state == State::Stopped {
                    if give {
                        r.give_prompt_again(&s.id)?;
                    } else {
                        r.start(&s.id)?;
                    }
                    say_notices(r);
                }
            }
            r.attach(&s.id)?;
        }
        Cmd::Start {
            session,
            yes,
            again,
        } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            // What can't be done isn't asked about first.
            if again.prompt_again
                && let Some(refused) = s.prompt_again_refused()
            {
                bail!(refused);
            }
            // Its first prompt not seen taken asks that instead: a new
            // conversation either way. A running one fails to start anyway.
            let give = match (&s.given_prompt, s.state) {
                (Some(_), State::Stopped) => match give_prompt_again(&s, &again)? {
                    Some(give) => give,
                    None => return Ok(()),
                },
                (_, State::Stopped) if !confirm_new_conversation(r, &s, yes)? => return Ok(()),
                _ => false,
            };
            if give {
                r.give_prompt_again(&s.id)?;
                println!("Started {}, its first prompt given again", s.label());
                return Ok(());
            }
            r.start(&s.id)?;
            println!("Started {}", s.label());
            if s.given_prompt.is_some() {
                println!("Its first prompt, not seen taken, is kept");
            }
        }
        Cmd::Stop { session, yes } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            if s.state == State::Working
                && !confirm(&format!("{} is working. Stop it?", s.label()), yes)?
            {
                return Ok(());
            }
            if r.stop(&s.id)? {
                println!("Stopped {}", s.label());
            } else {
                println!("{} wasn't running", s.label());
            }
        }
        Cmd::Restart {
            session,
            yes,
            again,
        } => {
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            // Still at a screen it showed before taking its first prompt,
            // say: whether to give it again is asked instead. That question
            // says a working one is cut short; answered on the command line,
            // it isn't asked, so that's asked as any restart asks it.
            if s.given_prompt.is_some() {
                let answered = again.prompt_again || again.no_prompt_again;
                if answered
                    && s.state == State::Working
                    && !confirm(&format!("{} is working. Restart it?", s.label()), yes)?
                {
                    return Ok(());
                }
                let Some(give) = give_prompt_again(&s, &again)? else {
                    return Ok(());
                };
                if give {
                    r.restart_giving_prompt(&s.id)?;
                    println!("Restarted {}, its first prompt given again", s.label());
                } else {
                    r.restart(&s.id)?;
                    println!("Restarted {}", s.label());
                }
                return Ok(());
            }
            let asked = if !r.can_resume(&s) {
                confirm_new_conversation(r, &s, yes)?
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
            // Whether its agent still runs, for the question.
            r.refresh()?;
            let s = r.resolve(&session, false)?;
            if !confirm(&agent::archive_question(&s), yes)? {
                return Ok(());
            }
            r.archive(&s.id)?;
            println!("Archived {} ({})", s.label(), s.id);
        }
        Cmd::Rm { session, yes } => {
            // What its hooks said since, in the question: a turn or a subagent
            // it would cut short.
            r.refresh()?;
            let s = r.resolve_either(&session)?;
            if !confirm(&agent::delete_question(&s), yes)? {
                return Ok(());
            }
            r.delete(&s.id)?;
            println!("Deleted {} ({}) for good", s.label(), s.id);
        }
        Cmd::Restore { session, name } => {
            let s = r.resolve(&session, true)?;
            let s = r.restore(&s.id, name.as_deref())?;
            println!("Restored {} (stopped)", s.label());
        }
        Cmd::Edit {
            session,
            name,
            title,
            model,
            effort,
        } => {
            let before = r.resolve(&session, false)?;
            // Only what's given changes: "" for the title follows the agent's.
            let s = r.update_session(
                &before.id,
                name.as_deref(),
                title.map(Some),
                setting(model),
                setting(effort),
            )?;
            println!("Updated {}", s.label());
            // A running agent goes on with what it started with.
            let restart = (&s.model, &s.effort) != (&before.model, &before.effort);
            if restart && s.state != State::Stopped {
                println!("The agent picks up a new model or effort on its next start");
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
            list_adoptable(r, all)?;
        }
        Cmd::Adopt {
            conversation: Some(conversation),
            project,
            name,
            defaults,
            model,
            effort,
            fork,
            yes,
            attach,
            ..
        } => {
            let mut req = Adopt {
                conversation,
                project,
                name,
                run_on: if defaults {
                    RunOn::Defaults
                } else {
                    RunOn::Conversation
                },
                model: setting(model),
                effort: setting(effort),
                fork,
                force: yes,
            };
            if let Some((agent, pid)) = r.open_elsewhere(&req)? {
                let question = format!(
                    "It's open in {} (pid {pid}): two would write one conversation; --fork adopts a copy. Adopt it anyway?",
                    util::capitalize(&agent)
                );
                if !confirm(&question, yes)? {
                    return Ok(());
                }
                req.force = true;
            }
            let s = r.adopt(req)?;
            let copy = if fork { " as a copy" } else { "" };
            println!("Adopted {}{copy} ({})", s.label(), s.id);
            if attach {
                say_notices(r);
                r.attach(&s.id)?;
            }
        }
        Cmd::Info { session } => {
            r.refresh()?;
            let s = r.resolve_any(&session)?;
            print_info(r, &s);
        }
        Cmd::Note {
            session,
            project,
            set,
            append,
            clear,
            edit,
            pin,
            unpin,
        } => {
            let n = noted(r, session.as_deref(), project.as_deref())?;
            if pin.is_some() || unpin {
                println!("{}", r.pin_note(&n, pin)?);
                return Ok(());
            }
            let old = n.note.as_ref().map_or(String::new(), |n| n.text.clone());
            let new = match (set, append) {
                (Some(text), _) => {
                    let text = match text.as_str() {
                        "-" => io::read_to_string(io::stdin())?,
                        _ => text,
                    };
                    // Not an empty pipe taken for a note to remove.
                    if note::tidy(&text).is_empty() {
                        bail!("nothing to set the note to; --clear removes it");
                    }
                    text
                }
                (None, Some(line)) if old.is_empty() => line,
                (None, Some(line)) => format!("{old}\n{line}"),
                (None, None) if clear => String::new(),
                (None, None) if edit => {
                    let file = r.note_file(&n);
                    match note::edit_externally(&file, &old, |text| r.change_note(&n, &text))? {
                        Some(said) => println!("{said}"),
                        None => bail!("$EDITOR exited with an error; the note is as it was"),
                    }
                    return Ok(());
                }
                (None, None) => {
                    match &n.note {
                        Some(note) => println!("{}", note.text),
                        None => eprintln!("{} has no note", n.label),
                    }
                    return Ok(());
                }
            };
            println!("{}", r.change_note(&n, &new)?);
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
        Cmd::Config {
            change,
            defaults,
            edit,
        } => match change {
            Some(ConfigCmd::Set { setting, value }) => {
                r.change_setting(&setting, Some(&value))?;
                println!("Set {setting}");
            }
            Some(ConfigCmd::Unset { setting, yes }) => {
                let question = r.unset_question(&setting);
                if let Some(question) = &question
                    && !confirm(question, yes)?
                {
                    return Ok(());
                }
                r.change_setting(&setting, None)?;
                match question {
                    Some(_) => println!("Took [{setting}] out of config.toml"),
                    None => println!("{setting} is back to its default"),
                }
            }
            None if defaults => print!("{}", settings::template(&r.settings())),
            None if edit => {
                let (file, made) = r.config_file_to_edit()?;
                if !util::run_editor(&file)? {
                    bail!("the editor exited with an error; config.toml is as it left it");
                }
                match r.reload_config_now()? {
                    settings::Reloaded::Same if made => {
                        println!("config.toml made, every setting commented out at its default")
                    }
                    settings::Reloaded::Same => println!("config.toml unchanged"),
                    settings::Reloaded::InUse(None) => println!("config.toml saved"),
                    settings::Reloaded::InUse(Some(e)) => {
                        return Err(
                            e.context("config.toml is saved, but roster's tmux didn't take it")
                        );
                    }
                }
            }
            None => print::settings(r),
        },
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
        Cmd::Usage { json } => print_usage(r, json)?,
        Cmd::Tokens { days, json } => print_tokens(r, days, json)?,
        Cmd::Events { follow, session } => events(r, follow, session.as_deref())?,
        // Dispatched in main before parsing; only reached via unusual argument orders.
        Cmd::Report { .. } | Cmd::TrustFolder { .. } => {}
        Cmd::Deliver { session } => {
            let s = r.resolve(&session, false)?;
            // Its output goes nowhere: you're attached, so it says so there.
            if let Err(e) = r.wait_for_prompt(&s.id, DELIVER_TIMEOUT) {
                r.tmux.say(&s.tmux_name(), &format!("roster: {e:#}"));
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Starts a new session, waits for its first prompt to go in, and attaches if
/// asked: then the prompt goes in from a roster left in the background, as
/// nothing refreshes while you're attached.
fn start_session(r: &mut Roster, request: NewSession, attach: bool) -> Result<()> {
    let prompted = request.prompt.is_some();
    let waits = prompted && !attach;
    let s = r.new_session(request)?;
    println!("Started {} ({})", s.label(), s.id);
    say_notices(r);
    // Given on its command line (codex): nothing to wait for.
    if prompted && agent::adapter(&s.agent).takes_prompt() {
        println!("It starts with the first prompt, given on its command line");
    } else if waits {
        if r.wait_for_prompt(&s.id, Duration::from_secs(60))? {
            println!("Sent the first prompt");
        } else if r.db.session(&s.id)?.state == State::Stopped {
            println!(
                "The agent stopped before it was ready; the prompt is typed in once it's started and ready"
            );
        } else {
            println!(
                "The agent isn't ready yet (it may be waiting for you, e.g. to trust the folder);"
            );
            println!("the prompt is typed in once it is, by the next roster command or the TUI");
        }
    }
    if attach {
        // Typed in meanwhile by a roster of its own (`Roster::attach_terminal`).
        r.attach(&s.id)?;
    }
    Ok(())
}

/// How long `roster deliver` waits for an agent to be ready for its first
/// prompt: time to answer what it asks first, a folder to trust say.
const DELIVER_TIMEOUT: Duration = Duration::from_secs(600);

/// Whether a start of `s`, whose first prompt its agent hasn't said it took,
/// gives it again: as `again` says, else asked (with a terminal to
/// ask in); None for no start at all.
fn give_prompt_again(s: &Session, again: &PromptAgain) -> Result<Option<bool>> {
    if again.prompt_again || again.no_prompt_again {
        return Ok(Some(again.prompt_again));
    }
    let question = crate::agent::prompt_again_question(s);
    if !io::stdin().is_terminal() {
        bail!("{question} Pass --prompt-again to give it, or --no-prompt-again to start without.");
    }
    eprint!("{question} [y/n, anything else cancels] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(match answer.trim().to_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    })
}

/// Asks before (re)starting an agent that can't resume its conversation.
fn confirm_new_conversation(r: &Roster, s: &Session, yes: bool) -> Result<bool> {
    if r.can_resume(s) {
        return Ok(true);
    }
    confirm(&crate::agent::new_conversation_question(s), yes)
}

/// Whose note `roster note` acts on: `session`'s, or with `project` a
/// project's, the one named or else the session's. Inside an agent, without
/// a session, its own session is meant.
fn noted(r: &Roster, session: Option<&str>, project: Option<&str>) -> Result<note::Noted> {
    let of = |id: i64| -> Result<note::Noted> {
        let p = r.projects()?.into_iter().find(|p| p.id == id);
        Ok(p.context("its project is gone")?.noted())
    };
    // Inside an agent, its session, by the id roster gave it.
    let own = || -> Result<Option<model::Session>> {
        match std::env::var("ROSTER_SESSION") {
            Ok(id) if !id.is_empty() => {
                let s = r.sessions(false)?.into_iter().find(|s| s.id == id);
                let gone =
                    || format!("ROSTER_SESSION is {id}, which names no live session: give one");
                s.map(Some).with_context(gone)
            }
            _ => Ok(None),
        }
    };
    match (session, project) {
        (Some(_), Some(name)) if !name.is_empty() => {
            bail!("a session or --project with a name, not both")
        }
        (Some(session), Some(_)) => of(r.resolve_any(session)?.project_id),
        (Some(session), None) => Ok(r.resolve_any(session)?.noted()),
        (None, Some("")) => match own()? {
            Some(s) => of(s.project_id),
            None => bail!("which project's note? --project <PROJECT>"),
        },
        (None, Some(name)) => match r.projects()?.into_iter().find(|p| p.name == name) {
            Some(p) => Ok(p.noted()),
            None => bail!("no project named {name:?}"),
        },
        (None, None) => match own()? {
            Some(s) => Ok(s.noted()),
            None => bail!("which note? a session's, or --project <PROJECT>"),
        },
    }
}

/// `roster config --edit` when roster couldn't open (`why`): if config.toml
/// is why, the file in the editor, then roster opened with it, which puts it
/// in use in roster's tmux too; anything else is `why` as it is.
fn edit_unread_config(why: anyhow::Error) -> Result<()> {
    let file = paths::Paths::from_env()?.config_file();
    if config::Config::load_text(&file).is_ok() {
        return Err(why);
    }
    eprintln!("roster: {why:#}");
    if !util::run_editor(&file)? {
        bail!("the editor exited with an error; config.toml is as it left it");
    }
    config::Config::load_text(&file).context("config.toml still has an error")?;
    let mut r = Roster::open()?;
    // What it sets that roster no longer has is only said; roster's tmux
    // refusing what it sets is a failure.
    let gone = r.config.gone();
    let (said, refused): (Vec<String>, Vec<String>) =
        r.take_notices().into_iter().partition(|n| gone.contains(n));
    for notice in said {
        eprintln!("roster: {notice}");
    }
    if !refused.is_empty() {
        bail!("config.toml is saved, but {}", refused.join("; "));
    }
    println!("config.toml saved");
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
