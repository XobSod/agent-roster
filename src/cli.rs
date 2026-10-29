//! The command line: its commands and their arguments.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::engine::NewSession;
use crate::model;

#[derive(Parser)]
#[command(
    name = "roster",
    version,
    about = "Keyboard-first manager for AI coding agent sessions",
    after_help = "Sessions are addressed as project/name, name, or id (a prefix of 4+ characters is enough)."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
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
    Adopt(AdoptArgs),
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
        /// What each live session's agent and all it started use: CPU (measured over a second) and memory
        #[arg(long)]
        resources: bool,
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
    /// Stop a session's agent, keeping the session (asks first if it's working); --kill kills it and all it started at once
    Stop {
        session: String,
        #[arg(short, long)]
        yes: bool,
        /// Kill it at once instead, asking it nothing: its agent and all it started (an archived session's too, what it left running)
        #[arg(long)]
        kill: bool,
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
    Note(NoteArgs),
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
    /// The session list, read only, beside the agent you're attached to (fit_preview = "always"): roster runs it itself
    #[command(hide = true)]
    Sidebar { session: String },
    /// Check roster's tmux server and database against each other, and show what each session's processes use
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
pub enum ProjectCmd {
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
pub struct PromptAgain {
    /// Give the agent its first prompt again, one it exited before saying it took
    #[arg(long, conflicts_with = "no_prompt_again")]
    pub prompt_again: bool,
    /// Start without that first prompt, kept to be asked about again
    #[arg(long)]
    pub no_prompt_again: bool,
}

/// What a new session or chat should be.
#[derive(clap::Args)]
pub struct SessionArgs {
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
    pub fn request(self, p: model::Project, default_agent: &str) -> (NewSession, bool) {
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
pub enum ConfigCmd {
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
pub struct DefaultsArgs {
    /// Default agent for new sessions
    #[arg(long)]
    pub agent: Option<String>,
    /// Default model: for Claude opus, sonnet, haiku, fable or a full name; for pi or codex any it knows
    #[arg(long)]
    pub model: Option<String>,
    /// Default effort: for Claude low … max; for codex none … ultra; for pi a thinking level, off … max
    #[arg(long)]
    pub effort: Option<String>,
}

/// None: not given; Some(None): "default" (no value); Some(Some(v)): v.
pub fn setting(value: Option<String>) -> Option<Option<String>> {
    value.map(|v| (v != "default").then_some(v))
}

#[derive(clap::Args)]
pub struct AdoptArgs {
    /// The conversation's id (4+ characters are enough); without it, lists them
    pub conversation: Option<String>,
    /// List the conversations that can be adopted
    #[arg(long)]
    pub list: bool,
    /// Also list those outside your projects' directories
    #[arg(long)]
    pub all: bool,
    /// Project to put it in, created for its directory if new (default: the project
    /// holding its directory, else a new one named after the directory)
    #[arg(long)]
    pub project: Option<String>,
    /// The agent's name in the project (default: its agent's and a number, claude-1, pi-2, …)
    #[arg(long)]
    pub name: Option<String>,
    /// Go on with the defaults, not what it last ran on: the project's model and effort if it
    /// runs the same agent, else the agent's in config.toml, else its own
    #[arg(long)]
    pub defaults: bool,
    /// Model (default: what it last ran on, or with --defaults the defaults; "default" for those)
    #[arg(long)]
    pub model: Option<String>,
    /// Effort (default: what it last ran on, or with --defaults the defaults; "default" for those)
    #[arg(long)]
    pub effort: Option<String>,
    /// Adopt a copy and leave the original alone
    #[arg(long)]
    pub fork: bool,
    /// Move it without asking even if it's open in its agent (Claude tells; pi and codex don't)
    #[arg(short, long)]
    pub yes: bool,
    /// Attach to it right away
    #[arg(short, long)]
    pub attach: bool,
}

#[derive(clap::Args)]
pub struct NoteArgs {
    /// Whose note: a session's (inside an agent, left out: its own
    /// session's); with --project alone, its project's
    pub session: Option<String>,
    /// A project's note: this project's, or the session's project's (inside
    /// an agent, its own session's)
    #[arg(long, value_name = "PROJECT", num_args = 0..=1, default_missing_value = "")]
    pub project: Option<String>,
    /// Replace it with this text ("-" reads it from stdin)
    #[arg(long, allow_hyphen_values = true, conflicts_with_all = ["append", "clear", "edit"])]
    pub set: Option<String>,
    /// Add this line at its end
    #[arg(long, allow_hyphen_values = true, conflicts_with_all = ["clear", "edit"])]
    pub append: Option<String>,
    /// Remove it
    #[arg(long, conflicts_with = "edit")]
    pub clear: bool,
    /// Open it in $VISUAL or $EDITOR
    #[arg(long)]
    pub edit: bool,
    /// Pin this line of it (from 1): the preview shows it and the one below
    #[arg(long, value_name = "LINE", conflicts_with_all = ["set", "append", "clear", "edit", "unpin"])]
    pub pin: Option<usize>,
    /// Unpin its pinned line: the preview shows its first lines
    #[arg(long, conflicts_with_all = ["set", "append", "clear", "edit"])]
    pub unpin: bool,
}
