//! Agents: what roster needs to know about each beyond running its command.
//!
//! An agent roster knows nothing about (any CLI under `[agents.<name>]` in the
//! config) is generic: its state comes from its screen alone, and each start is
//! a new conversation. A built-in agent has an `Adapter` that tells more: how
//! to start and resume it, what its reports (hooks calling `roster report`)
//! say, which models and efforts it takes, and where its transcript is.
//! Adding one is a module here, implementing what it supports, a line in
//! `adapter` and its name in `BUILT_IN`.
//!
//! An agent that reports does so with `roster report <kind>`, a JSON payload
//! on its stdin (`ROSTER_SESSION`, `ROSTER_DB` and `ROSTER_EXE` are in its
//! environment). The kinds:
//!
//! - `session`: it (re)started or cleared its conversation, and waits for a
//!   prompt. Its payload names the transcript. Until it's reported in the run
//!   roster started, a first prompt isn't typed in (the agent may still be
//!   asking something else, such as whether to trust the directory). An agent
//!   handed its first prompt on its command line instead
//!   (`Adapter::takes_prompt`) has nothing typed in.
//! - `prompt`: you gave it a task; it's working.
//! - `working`: it's still at it (after a tool, or a question answered).
//! - `needs-input`: it's waiting for you in the middle of a turn.
//! - `idle`: the turn is over, unless subagents it started run on.
//! - `subagent-start`, `subagent-stop`: one of its subagents started or
//!   stopped (Claude's, codex's); a turn of its own ends working while any
//!   runs.
//! - anything else (pi's extension says `info`): only its payload counts.
//!
//! An agent that reports needs to report the start of a turn as well as its
//! end: its screen only corrects an end that went unreported.

pub mod claude;
pub mod codex;
pub(crate) mod locked;
pub mod pi;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer};

use crate::config::Config;
use crate::model::{QUIET_MS, Session, State};
use crate::stats::Transcript;
use crate::tokens::Spending;
use crate::usage::Usage;

pub use claude::CLAUDE;
pub use codex::CODEX;
pub use pi::PI;

/// Agents with an adapter. Claude is always offered; the others once they're
/// installed (their command is found) or configured.
pub const BUILT_IN: &[&str] = &[CLAUDE, PI, CODEX];

/// What a built-in agent can do beyond running. Every method has a default for
/// an agent that doesn't, so an adapter implements only what its agent supports.
pub trait Adapter: Sync {
    /// Whether it reports its state through hooks calling `roster report`;
    /// if not, its screen is all roster has to go on.
    fn reports(&self) -> bool {
        false
    }

    /// Whether a session's first prompt goes on its command line (`launch`
    /// reads `Session::pending_prompt`), rather than being typed in once it has
    /// reported its start: for an agent that reports nothing before its first
    /// turn (codex), typing would land in whatever it shows first.
    fn takes_prompt(&self) -> bool {
        false
    }

    /// Whether it reports each of its subagents starting and stopping
    /// (`subagent-start`, `subagent-stop`): then one heard of that wasn't
    /// heard starting started all the same, its start's report lost.
    fn says_subagents_start(&self) -> bool {
        false
    }

    /// Whether its subagents run on when its turn is interrupted (codex's),
    /// rather than ending with it.
    fn subagents_outlive_interrupts(&self) -> bool {
        false
    }

    /// Whether starting it again resumes its conversation.
    fn resumes(&self) -> bool {
        false
    }

    /// Whether its conversations outlive roster's sessions, to be adopted again.
    fn adopts(&self) -> bool {
        false
    }

    /// Whether it names a conversation itself, reporting its id once it has
    /// one (codex); if not, an agent that resumes is given one at its start
    /// (`Launch::conversation_id`).
    fn names_its_conversation(&self) -> bool {
        false
    }

    /// Whether it deletes old conversations itself (Claude, those older than
    /// its retention period): then that's why one is gone.
    fn deletes_old_conversations(&self) -> bool {
        false
    }

    /// Whether it keeps a transcript roster reads (`transcript`).
    fn keeps_transcript(&self) -> bool {
        false
    }

    /// Whether it has a one-off mode (`oneoff`), for generated titles.
    fn runs_oneoff(&self) -> bool {
        false
    }

    /// The conversations it has kept, started in roster or not, whose id
    /// `wanted` says yes to (only their files are read), for adopting.
    fn conversations(&self, _wanted: &dyn Fn(&str) -> bool) -> Vec<Conversation> {
        Vec::new()
    }

    /// How lists mark its sessions; None for the generic mark, which says only
    /// that it's an agent, so lists name it as well.
    fn mark(&self) -> Option<Mark> {
        None
    }

    /// The efforts a session can ask for; None if it takes neither a model
    /// nor an effort.
    fn choices(&self) -> Option<Choices> {
        None
    }

    /// The models to offer for a session, for forms (any other name can be
    /// typed). An agent that can list its own does, through `ask`, which runs
    /// a command of its in the environment its sessions get and returns what
    /// it printed (None if it failed); others know theirs.
    fn models(&self, _config: &Config, _ask: &Ask<'_>) -> Vec<Model> {
        Vec::new()
    }

    /// What the account it runs on has used of its limits, asked of the
    /// agent now (through `ask`, as for `models`); None if it has none roster
    /// can read (an API key, say).
    fn usage(&self, _config: &Config, _ask: &Ask<'_>) -> Option<Result<Usage>> {
        None
    }

    /// What its transcript at `path` last said of those limits, as of when
    /// the file last changed, for an agent that writes them as it goes
    /// (codex, with every response).
    fn usage_in(&self, _path: &Path) -> Option<Usage> {
        None
    }

    /// The command line that starts `s`, given its command and the config's
    /// arguments, and the conversation it starts on if roster gives it one.
    fn launch(&self, _s: &Session, argv: Vec<String>, _cx: &Context) -> Result<Launch> {
        Ok(Launch {
            argv,
            conversation_id: None,
            prompt: None,
        })
    }

    /// What a report's payload says, in roster's terms.
    fn report(&self, _payload: &serde_json::Value) -> Report {
        Report::default()
    }

    /// Whether the agent names the task it's on itself (`title`); if not,
    /// and it has a one-off mode and a transcript, roster makes it a title
    /// after its first turn (`[agents.<name>] auto_title`).
    fn names_its_task(&self) -> bool {
        false
    }

    /// The task the agent names, if it names one: in its terminal title
    /// (Claude), or kept with the conversation it's on (codex).
    fn title(&self, _pane_title: &str, _conversation: Option<&str>) -> Option<String> {
        None
    }

    /// A one-off run of the agent for a small job such as a title: the prompt
    /// goes last, the answer comes on stdout, and nothing of it is kept
    /// (no tools, no session). `[agents.<name>] title_model` picks its model,
    /// else `model` (`AgentConfig::titles_by`), else the agent's default.
    /// None if the agent has no such mode (`runs_oneoff`).
    fn oneoff(&self, _config: &Config) -> Option<Vec<String>> {
        None
    }

    /// A reader for the transcript at `path` (as its reports named it), which
    /// reads nothing yet; None if it keeps none (`keeps_transcript`).
    fn transcript(&self, _path: &Path) -> Option<Box<dyn Transcript>> {
        None
    }

    /// What it keeps of its subagent `id` in the conversation whose
    /// transcript is at `transcript`: the task it was given and the model
    /// it runs, as far as it says yet.
    fn subagent_info(&self, _transcript: &Path, _id: &str) -> SubagentInfo {
        SubagentInfo::default()
    }

    /// Whether its subagent `id` (in the conversation whose transcript is at
    /// `transcript`) is finished, as its agent keeps it: since it was last
    /// heard of (`heard`), not a run it was taken up again after.
    fn subagent_finished(&self, _transcript: &Path, _id: &str, _heard: i64) -> bool {
        false
    }

    /// What one of its subagents is doing now, from its transcript at
    /// `file` (`SubagentInfo::file`), in a few words: its latest step.
    fn subagent_doing(&self, _file: &Path) -> Option<String> {
        None
    }

    /// The commands it left running in the background whose end its
    /// transcript at `transcript` records from byte `from` on, and how far
    /// that was read: to the end of its last whole line. None if it can't
    /// tell.
    fn shells_ended(&self, _transcript: &Path, _from: u64) -> Option<(ShellEnds, u64)> {
        None
    }

    /// How far before a shell's mark (`Backgrounded::from`) its end may be
    /// written in its transcript (bytes), to be read from there.
    fn shell_slack(&self) -> u64 {
        0
    }

    /// The commands its last turn left running in the background, as its
    /// transcript at `transcript` records them, for an agent whose hooks
    /// don't say (codex's background terminals).
    fn shells_left(&self, _transcript: &Path) -> Vec<Backgrounded> {
        Vec::new()
    }

    /// When its last turn ended, as its transcript at `transcript` records
    /// it: None if that turn hasn't, was interrupted, or it can't tell. For
    /// a turn whose end no hook reports (a codex review's).
    fn turn_ended(&self, _transcript: &Path) -> Option<i64> {
        None
    }

    /// Trusts the folder `dir` in the agent's own config, as answering its
    /// question there would (`trust_project_folders`), unless it trusts it
    /// already: whether it wrote that. Its config is found from the
    /// environment this runs in, which must be the agent's own
    /// (`trust_and_run`).
    fn trust_folder(&self, _dir: &Path) -> Result<bool> {
        Ok(false)
    }

    /// Whether it trusts folders roster can trust for it (`trust_folder`).
    fn trusts_folders(&self) -> bool {
        false
    }

    /// Whether it trusts the folder `dir`, as its own question would find:
    /// for a worktree's folder to be trusted as its checkout's is. Its config
    /// is found as `trust_folder` finds it.
    fn folder_trusted(&self, _dir: &Path) -> bool {
        false
    }

    /// The config `trust_folder` writes in, found as it finds it.
    fn trust_config(&self) -> Option<PathBuf> {
        None
    }

    /// Takes back, in its config `file`, the trust of the folders at or
    /// under `root` (a worktree removed): whether any was.
    fn untrust_folders_under(&self, _file: &Path, _root: &Path) -> Result<bool> {
        Ok(false)
    }

    /// Its command line `argv` with roster's own hooks in it (roster run as
    /// `exe`) trusted for that run, as answering its review of them would
    /// (`trust_roster_hooks`). Which it trusts already is found from the
    /// environment this runs in, which must be the agent's own
    /// (`trust_and_run`).
    fn hooks_trusted(&self, argv: &[String], _exe: &str) -> Result<Vec<String>> {
        Ok(argv.to_vec())
    }

    /// Whether it asks to trust hooks roster can trust for it (`hooks_trusted`).
    fn trusts_hooks(&self) -> bool {
        false
    }

    /// Every file it keeps what it spent in (its conversations', started in
    /// roster or not), each with the id of the conversation it's of.
    fn token_files(&self) -> Vec<(std::path::PathBuf, String)> {
        Vec::new()
    }

    /// A reader of what the file at `path` (one of `token_files`) says it
    /// spent, call by call, which reads nothing yet.
    fn tokens(&self, _path: &Path) -> Option<Box<dyn Spending>> {
        None
    }
}

/// Any agent without an adapter of its own.
struct Generic;

impl Adapter for Generic {}

/// The adapter for agent `name`: a built-in agent's, else the generic one (also
/// for agents since removed from the config, which are still watched).
pub fn adapter(name: &str) -> &'static dyn Adapter {
    match name {
        CLAUDE => &claude::Claude,
        PI => &pi::Pi,
        CODEX => &codex::Codex,
        _ => &Generic,
    }
}

/// A conversation an agent has kept, as `Adapter::conversations` finds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub agent: String,
    pub id: String,
    pub path: std::path::PathBuf,
    /// Where it ran; it's resumed there.
    pub cwd: std::path::PathBuf,
    pub branch: Option<String>,
    /// The name it was given (`claude --name`, `/rename`, pi's `/name`, or a
    /// session manager).
    pub custom_title: Option<String>,
    /// The agent's own title for it.
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    /// The model it last used, and the effort, if the agent records them.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// When the file last changed, in ms.
    pub modified: i64,
    /// A process of the agent's that has it open right now, if the agent
    /// tells (Claude does).
    pub open_in: Option<i32>,
}

impl Conversation {
    /// What to call it, on one line: its name, the agent's title, else a prompt.
    pub fn label(&self) -> String {
        [
            &self.custom_title,
            &self.title,
            &self.first_prompt,
            &self.last_prompt,
        ]
        .into_iter()
        .flatten()
        .next()
        .map_or(String::new(), |s| crate::util::one_line(s))
    }
}

/// A message's text from a conversation file already parsed: a string, or the
/// text blocks of a list joined (`stats::Text` reads it the same way from a
/// transcript), trimmed; None if there's none.
pub(crate) fn message_text(content: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| matches!(b["type"].as_str(), None | Some("text" | "input_text")))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A conversation file's lines worth reading for what it says about itself:
/// a small file whole, a larger one's start and end (the line cut at the
/// start of the end left out), as JSON; and when it last changed, in ms.
pub(crate) fn file_ends(path: &Path) -> Option<(Vec<serde_json::Value>, i64)> {
    use std::io::{Read, Seek, SeekFrom};
    const HEAD: u64 = 64 * 1024;
    const TAIL: u64 = 512 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    let mut head = Vec::new();
    let mut tail = Vec::new();
    if meta.len() <= HEAD + TAIL {
        file.read_to_end(&mut head).ok()?;
    } else {
        (&mut file).take(HEAD).read_to_end(&mut head).ok()?;
        file.seek(SeekFrom::Start(meta.len() - TAIL)).ok()?;
        file.read_to_end(&mut tail).ok()?;
        let cut = tail
            .iter()
            .position(|b| *b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        tail.drain(..cut);
    }
    let entries = head
        .split(|b| *b == b'\n')
        .chain(tail.split(|b| *b == b'\n'))
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    Some((entries, modified))
}

/// An agent's mark in lists: a glyph, in its colour if it has one.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub glyph: &'static str,
    pub rgb: Option<(u8, u8, u8)>,
}

/// The efforts an agent takes.
#[derive(Clone, Copy, Debug)]
pub struct Choices {
    pub efforts: &'static [&'static str],
}

/// A model to offer for a session.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// What the agent's model option takes.
    pub id: String,
    /// Shown next to it. None for an alias, which follows the latest version:
    /// what it last turned out to be shows instead ("latest" until known).
    pub note: Option<String>,
}

/// Runs an agent's command and returns what it printed; failing, why (or
/// what it said as it failed).
pub type Ask<'a> = dyn Fn(Question<'_>) -> Result<String> + 'a;

/// A command for `Ask` to run.
pub struct Question<'a> {
    pub argv: Vec<String>,
    /// What it reads on its stdin; none, nothing.
    pub input: Option<crate::launch::Input<'a>>,
}

impl Question<'_> {
    pub fn of(argv: Vec<String>) -> Question<'static> {
        Question { argv, input: None }
    }
}

/// What an agent reported besides its state: `Adapter::report` reads it from
/// the payload, whatever its agent calls things. An agent whose reports roster
/// writes itself (Pi's, through roster's extension) sends it as JSON with
/// these names.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Report {
    /// The conversation it's on now (it changes when the agent clears it).
    pub conversation_id: Option<String>,
    pub transcript_path: Option<String>,
    /// The conversation was cleared: a new task.
    pub cleared: bool,
    /// The agent itself just started (not resumed, cleared or compacted in
    /// it), so the model it runs is the one its start asked for.
    pub started: bool,
    /// It compacted its conversation, mid-turn as likely as not: neither a
    /// start nor the turn's end.
    pub compacted: bool,
    /// From one of its subagents (its agent's id for it), inside a turn: of
    /// the session's own conversation, model and turn it says nothing.
    pub subagent: Option<String>,
    /// What kind that subagent is (Claude's `agent_type`: Explore, …).
    pub subagent_kind: Option<String>,
    /// From a subagent of an agent that reports each starting
    /// (`Adapter::says_subagents_start`), set as the report's read.
    pub subagents_start_said: bool,
    /// The model and effort it's running.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// What it says it's working on; Some(None): it no longer says.
    #[serde(deserialize_with = "present")]
    pub title: Option<Option<String>>,
    /// The context window of the model it's running, in tokens.
    pub context_window: Option<u64>,
    /// Commands it just left running in the background (its shells:
    /// Claude's, codex's background terminals).
    pub shells: Vec<Backgrounded>,
    /// Commands it left running that it says have ended, by its id for each.
    pub shells_ended: Vec<String>,
    /// A prompt it was given by itself: told that something it left running
    /// ended, not by you.
    pub woken: bool,
    /// Its transcript was there as it reported (`report::TRANSCRIPT_SIZE`):
    /// its conversation has had a message, whatever roster saw of it.
    #[serde(skip)]
    pub transcript_written: bool,
}

/// Ends of commands an agent left running, as its transcript records them
/// (`Adapter::shells_ended`).
pub type ShellEnds = Vec<ShellEnd>;

/// The end of a command an agent left running, as its transcript records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEnd {
    /// The agent's id for the command.
    pub id: String,
    /// When it was written (ms), if it says.
    pub at: Option<i64>,
    /// Told to the agent mid-turn, not only queued for it nor given as the
    /// prompt of a turn of its own (which reports it): no hook will report
    /// it.
    pub told: bool,
    /// Where its line starts in the transcript (bytes): one before a shell's
    /// mark, but for its agent's `Adapter::shell_slack`, isn't its.
    pub line: u64,
}

/// A command an agent left running in the background (`Report::shells`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Backgrounded {
    /// The agent's id for it.
    pub id: String,
    /// What it does, as the agent put it.
    pub what: Option<String>,
    /// When it started (ms), if the agent says; else `ran` before it was
    /// reported.
    pub started: Option<i64>,
    /// How long it had run before it was left running (ms).
    pub ran: i64,
    /// Where in the agent's transcript nothing of its end is written before
    /// (bytes), but for its agent's `Adapter::shell_slack`: how long it was
    /// as the command was reported, or where it was seen running.
    pub from: u64,
}

/// A field that's there, even as null (missing ones get the default, None).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// What a hook's payload says in the shape Claude's have, which codex's
/// copy: the conversation it's on and the file it's kept in, both of which a
/// /clear changes; how it started (`source`: `startup` the agent's own
/// start, `clear` a new task, `compact` the conversation compacted; `resume`
/// and codex's `fork` none of these); and the model. Both mark a subagent's
/// with its `agent_id`, and codex's name the subagent's own file and model
/// (Claude's the session's file, and no model): it gives none of them.
pub(crate) fn hook_report(payload: &serde_json::Value) -> Report {
    use serde_json::Value;
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
    match payload.get("agent_id") {
        None | Some(Value::Null) => {}
        Some(id) => {
            return Report {
                subagent: Some(id.as_str().map_or_else(|| id.to_string(), String::from)),
                subagent_kind: text(payload.get("agent_type")).filter(|k| !k.is_empty()),
                ..Report::default()
            };
        }
    }
    let source = payload.get("source").and_then(Value::as_str);
    Report {
        conversation_id: text(payload.get("session_id")),
        transcript_path: text(payload.get("transcript_path")),
        cleared: source == Some("clear"),
        started: source == Some("startup"),
        compacted: source == Some("compact"),
        model: text(payload.get("model")),
        transcript_written: transcript_written(payload),
        ..Report::default()
    }
}

/// Whether `payload`'s transcript was there, not empty, as roster stored it
/// (`report::TRANSCRIPT_SIZE`).
pub(crate) fn transcript_written(payload: &serde_json::Value) -> bool {
    (payload.get(crate::report::TRANSCRIPT_SIZE)).is_some_and(|size| size.as_u64() > Some(0))
}

/// What an agent keeps of one of its subagents (`Adapter::subagent_info`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubagentInfo {
    /// What it was given to do, in a few words.
    pub task: Option<String>,
    /// The model it runs, as its agent names it (an alias, `sonnet`, or an
    /// id); `inherit`: its parent's.
    pub model: Option<String>,
    /// All there: its agent's record of it read, and the model the one its
    /// replies are from, not one asked for. Neither changes after.
    pub settled: bool,
    /// Its transcript, once found, where what it's doing is read.
    pub file: Option<std::path::PathBuf>,
}

/// What a report from agent `name` says; nothing if its payload isn't JSON.
pub fn read_report(name: &str, payload: Option<&str>) -> Report {
    let adapter = adapter(name);
    let mut report: Report = payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .map(|v| adapter.report(&v))
        .unwrap_or_default();
    report.subagents_start_said = report.subagent.is_some() && adapter.says_subagents_start();
    report
}

/// What `roster trust [--folder] [--hooks] <agent> -- <command…>` does, run
/// by a session's launch script in its agent's place once the env files are
/// sourced, so in the environment the agent gets (a `CODEX_HOME` of the
/// project's, say): trusts the folder it's in for that agent
/// (`Adapter::trust_folder`), roster's hooks in its command
/// (`Adapter::hooks_trusted`), or both, then becomes the agent. What fails of
/// the trust is said, and the agent asks as it would.
pub fn trust_and_run(args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let flags = args
        .iter()
        .take_while(|a| a.starts_with("--") && *a != "--");
    let (mut folder, mut hooks, mut like) = (false, false, None);
    for flag in flags.clone() {
        match flag.as_str() {
            "--folder" => folder = true,
            "--hooks" => hooks = true,
            _ if flag.starts_with("--like=") => like = Some(PathBuf::from(&flag[7..])),
            _ => {
                eprintln!("roster: trust takes --folder, --hooks or --like=<dir>, not {flag}");
                std::process::exit(2);
            }
        }
    }
    let (agent, argv) = match &args[flags.count()..] {
        [agent, dash, argv @ ..] if dash == "--" && !argv.is_empty() => (agent, argv),
        _ => {
            eprintln!("roster: trust takes an agent, --, and its command");
            std::process::exit(2);
        }
    };
    let name = crate::util::capitalize(agent);
    // The working directory as the agent will see it: links resolved.
    let a = adapter(agent);
    // A worktree's (`--like` its checkout): what's written is said, to be
    // taken back as it's removed.
    let like_trusted = like
        .as_deref()
        .is_some_and(|checkout| a.folder_trusted(checkout));
    match std::env::current_dir() {
        Ok(dir) if folder || like_trusted => match a.trust_folder(&dir) {
            Ok(true) if like.is_some() => {
                let said = crate::model::WorktreeReport {
                    set_up: false,
                    trusted_in: a.trust_config(),
                };
                let said = serde_json::to_string(&said).unwrap_or_default();
                if let Err(e) = crate::report::record(crate::model::WORKTREE, &said) {
                    eprintln!("roster: couldn't record {}'s trust: {e:#}", dir.display());
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("roster: couldn't trust {} in {name}: {e:#}", dir.display()),
        },
        Ok(_) => {}
        Err(e) => eprintln!("roster: can't tell which folder to trust: {e}"),
    }
    let mut argv = argv.to_vec();
    if hooks {
        let trusted = crate::util::own_exe()
            .and_then(|exe| adapter(agent).hooks_trusted(&argv, &exe.to_string_lossy()));
        match trusted {
            Ok(trusted) => argv = trusted,
            Err(e) => eprintln!("roster: couldn't trust roster's hooks in {name}: {e:#}"),
        }
    }
    let e = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
    eprintln!("roster: can't run {}: {e}", argv[0]);
    std::process::exit(127);
}

/// What an adapter may use to start its agent.
pub struct Context<'a> {
    /// roster's own executable, which the agent's hooks call (`roster report`).
    pub roster: &'a Path,
    /// roster's state directory, for files an adapter gives its agent.
    pub state_dir: &'a Path,
}

pub struct Launch {
    pub argv: Vec<String>,
    /// The conversation the agent will use, to store with the session: None
    /// for one that names its own (`Adapter::names_its_conversation`), or
    /// keeps none.
    pub conversation_id: Option<String>,
    /// A first prompt to go last on its command line, which the launch
    /// script reads from a file (`launch::wrap`): tmux takes a command line
    /// of about 16 KB at most.
    pub prompt: Option<String>,
}

/// `name`'s adapter, if it's an agent that can be started (see `available`).
pub fn known(name: &str, config: &Config) -> Result<&'static dyn Adapter> {
    if offered(name, config) {
        Ok(adapter(name))
    } else {
        bail!(
            "unknown agent {name:?} (available: {})",
            available(config).join(", ")
        )
    }
}

/// Agents that can be started: the default one first, then the built-in ones
/// that are installed or configured (Claude always), then the others from the
/// config.
pub fn available(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = BUILT_IN
        .iter()
        .filter(|n| offered(n, config))
        .map(|n| n.to_string())
        .collect();
    names.extend(
        config
            .agents
            .keys()
            .filter(|n| !BUILT_IN.contains(&n.as_str()))
            .cloned(),
    );
    if let Some(i) = names.iter().position(|n| n == config.default_agent()) {
        let default = names.remove(i);
        names.insert(0, default);
    }
    names
}

/// Whether `name` can be started: Claude, an agent config.toml has, or a
/// built-in one that's installed.
pub(crate) fn offered(name: &str, config: &Config) -> bool {
    name == CLAUDE
        || config.agents.contains_key(name)
        || (BUILT_IN.contains(&name) && crate::util::on_path(name))
}

/// Session `s`'s transcript at `path`, its agent's reader of it, told what
/// an adopted copy was copied from (`Transcript::forked`): its file begins
/// with that conversation.
pub fn transcript_of(s: &Session, path: &Path) -> Option<Box<dyn Transcript>> {
    let mut transcript = adapter(&s.agent).transcript(path)?;
    if let Some(from) = &s.fork_from {
        transcript.forked(from, s.created_at);
    }
    Some(transcript)
}

/// Whether starting `s` goes on with its conversation: its agent resumes, and
/// roster knows the conversation, or the one an adopted copy forks. An agent
/// that names its conversation only from its first turn on (codex) may not
/// have yet, or never will if its hooks don't run: then it starts a new one,
/// as does one whose agent no longer has it (`conversation_lost`).
pub fn resumes(s: &Session) -> bool {
    adapter(&s.agent).resumes()
        && (s.conversation_id.is_some() || s.fork_from.is_some())
        && !conversation_lost(s)
}

/// Whether `s`'s agent no longer has its conversation: it has had a turn
/// (`Session::conversation_begun`), and its transcript is gone (Claude
/// deletes old ones). An agent given the conversation's id at every start
/// (Claude, pi) would begin a new one on it, unasked; codex, told to resume
/// one it can't find, says so.
pub fn conversation_lost(s: &Session) -> bool {
    let a = adapter(&s.agent);
    a.keeps_transcript()
        && !a.names_its_conversation()
        && s.conversation_begun
        && (s.transcript_path.as_deref()).is_some_and(|p| !Path::new(p).exists())
}

/// What starting `s` asks when its agent hasn't said it took its first
/// prompt (`Session::given_prompt`): whether to give it again. Either way
/// it's a new conversation, its agent never having said which it's on.
/// Stopped, its agent exited first; running, it's at a screen it showed
/// first, or at work on it unheard (its hooks not running), and a restart
/// cuts that short: said, as it's what a working one's restart asks.
pub fn prompt_again_question(s: &Session) -> String {
    let prompt = crate::util::one_line(s.given_prompt.as_deref().unwrap_or(""));
    let prompt = match prompt.char_indices().nth(60) {
        Some((at, _)) => format!("{}…", &prompt[..at]),
        None => prompt,
    };
    let (label, agent) = (s.label(), &s.agent);
    if s.state == State::Stopped {
        format!(
            "{label} ({agent}) exited before it said it took its first prompt, \"{prompt}\", so it starts a new conversation. Give it the prompt again?"
        )
    } else {
        format!(
            "{label} ({agent}) hasn't said it took its first prompt, \"{prompt}\": it may be at a screen it showed first, or at work on it unheard. Restarting cuts that short and starts a new conversation. Give it the prompt again?"
        )
    }
}

/// What archiving `s` asks: that its agent stops, if it runs.
pub fn archive_question(s: &Session) -> String {
    let stops = if s.state == State::Stopped {
        ""
    } else {
        " Its agent stops."
    };
    format!("Archive {}?{stops}", s.label())
}

/// What deleting `s` for good asks: what stops, if it runs (a working one's
/// turn, its subagents and the commands it left running are cut short), and
/// what goes and what stays. An archived one is stopped.
pub fn delete_question(s: &Session) -> String {
    let stops = if s.state == State::Stopped {
        String::new()
    } else {
        let count = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(one.to_string()),
            n => Some(format!("{n} {many}")),
        };
        let at_work: Vec<String> = [
            count(s.subagents.len(), "a subagent", "subagents"),
            count(
                s.shells.len(),
                "a command it left running",
                "commands it left running",
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        let at_work = at_work.join(" and ");
        let busy = match (s.state == State::Working, at_work.is_empty()) {
            (true, true) => " (it's working)".to_string(),
            (true, false) => format!(" (it's working, with {at_work})"),
            (false, true) => String::new(),
            (false, false) if s.shells.is_empty() => match s.subagents.len() {
                1 => " (a subagent of it is working)".to_string(),
                n => format!(" ({n} subagents of it are working)"),
            },
            (false, false) => format!(" (with {at_work})"),
        };
        format!("Its agent stops{busy}, and ")
    };
    let roster = if stops.is_empty() { "Roster" } else { "roster" };
    let note = if s.note.is_some() {
        " and its note"
    } else {
        ""
    };
    let kept = if adapter(&s.agent).adopts() {
        format!(
            "; its {} conversation stays",
            crate::util::capitalize(&s.agent)
        )
    } else {
        String::new()
    };
    format!(
        "Delete {} for good? {stops}{roster} forgets it{note}{kept}.",
        s.label()
    )
}

/// What the CLI and TUI ask before starting `s`, which begins a new
/// conversation (`resumes`).
pub fn new_conversation_question(s: &Session) -> String {
    let a = adapter(&s.agent);
    if conversation_lost(s) {
        let why = if a.deletes_old_conversations() {
            ", as it deletes old ones"
        } else {
            ", its file gone"
        };
        format!(
            "{} ({}) can't resume its conversation: {} no longer has it{why}. Start a new one?",
            s.label(),
            s.agent,
            crate::util::capitalize(&s.agent)
        )
    } else if a.names_its_conversation() {
        format!(
            "{} ({}) hasn't said which conversation it's on, so it can't resume it. Start a new one?",
            s.label(),
            s.agent
        )
    } else {
        format!(
            "{} ({}) can't resume its conversation. Start a new one?",
            s.label(),
            s.agent
        )
    }
}

/// The key under which what a model asked of `agent` (or "default") turned
/// out to be is learned: Claude's as it is, as keys already learned are;
/// another's after its name ("pi:default").
pub fn learned_key(agent: &str, asked: &str) -> String {
    if agent == CLAUDE {
        asked.to_string()
    } else {
        format!("{agent}:{asked}")
    }
}

/// Checks an effort against the ones `agent` takes. An agent that takes none
/// ignores it.
pub fn check_effort(agent: &str, effort: &str) -> Result<()> {
    let Some(choices) = adapter(agent).choices() else {
        return Ok(());
    };
    if !choices.efforts.contains(&effort) {
        bail!(
            "unknown effort {effort:?} (one of: {})",
            choices.efforts.join(", ")
        );
    }
    Ok(())
}

/// How long an agent without hooks must show a still screen to count as idle.
pub fn idle_after_ms(name: &str, config: &Config) -> i64 {
    config
        .agent(name)
        .idle_after
        .map_or(QUIET_MS, |secs| secs as i64 * 1000)
}

/// How `name`'s sessions are sized to the TUI's preview: its own setting,
/// else the global one, else as agents drawing full screen are.
pub fn fit_mode(name: &str, config: &Config) -> crate::config::FitPreview {
    (config.agent(name).fit_preview)
        .or(config.fit_preview)
        .unwrap_or_default()
}

/// How to start `s` now: its command and the config's arguments, completed by
/// its adapter (for Claude, a new conversation or its existing one resumed).
pub fn launch(s: &Session, config: &Config, cx: &Context) -> Result<Launch> {
    let adapter = known(&s.agent, config)?;
    let agent = config.agent(&s.agent);
    let mut argv = vec![agent.command.clone().unwrap_or_else(|| s.agent.clone())];
    argv.extend(agent.args.iter().cloned());
    adapter.launch(s, argv, cx)
}

/// How long a subagent's step is let be: a line on a card, and a bit to cut.
pub(crate) const STEP_CHARS: usize = 200;

/// A fetched URL's host, nothing else of it: not a login before an `@`, nor
/// a query (a token, say).
pub(crate) fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// An MCP server's tool (`mcp__<server>__<tool>`) as `<tool> (<server>)`.
pub(crate) fn mcp(name: &str) -> Option<String> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    Some(format!("{tool} ({server})"))
}

/// A subagent's step, `said`, on one line, without what a terminal would
/// take for more than text (control characters, which a card would show the
/// rest of, and direction overrides), at most `STEP_CHARS` long.
pub(crate) fn clean(said: &str) -> String {
    let bidi = |c: char| matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let kept: String = (said.chars())
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !bidi(*c))
        .collect();
    crate::util::one_line(&kept)
        .chars()
        .take(STEP_CHARS)
        .collect()
}

/// The whole lines in the last `bytes` of the file at `path`, first to last:
/// a line cut where they start isn't one.
pub(crate) fn tail_lines(path: &Path, bytes: u64) -> Option<Vec<Vec<u8>>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let mut lines: Vec<Vec<u8>> = tail.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::model::tests::session;

    pub(super) fn config_with(name: &str, agent: AgentConfig) -> Config {
        let mut config = Config::default();
        config.agents.insert(name.into(), agent);
        config
    }

    #[test]
    fn an_adopted_copys_transcript_is_told_what_it_was_copied_from() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = |uuid: &str, text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type": "user", "uuid": uuid,
                    "timestamp": "2026-10-25T10:00:00.000Z", "message": {"content": text}})
            )
        };
        std::fs::write(dir.path().join("p.jsonl"), prompt("u1", "parent's")).unwrap();
        let path = dir.path().join("c.jsonl");
        std::fs::write(&path, prompt("u1", "parent's") + &prompt("u2", "own")).unwrap();
        let mut s = session("a");
        s.agent = CLAUDE.into();
        let prompts = |s: &Session| {
            let mut t = transcript_of(s, &path).unwrap();
            t.update().unwrap();
            t.stats(None).prompts
        };
        assert_eq!(prompts(&s), Some(2));
        s.fork_from = Some("p".into());
        assert_eq!(prompts(&s), Some(1));
    }

    /// A launch context with roster at `roster` and nowhere to keep files.
    pub(super) fn cx(roster: &str) -> Context<'_> {
        Context {
            roster: Path::new(roster),
            state_dir: Path::new("/nonexistent"),
        }
    }

    #[test]
    fn configured_agents_are_generic() {
        let mut s = session("a");
        s.agent = "aider".into();
        assert!(launch(&s, &Config::default(), &cx("/bin/roster")).is_err());

        let agent = AgentConfig {
            args: vec!["--quiet".into()],
            idle_after: Some(2),
            ..AgentConfig::default()
        };
        let config = config_with("aider", agent);
        let l = launch(&s, &config, &cx("/bin/roster")).unwrap();
        // The command defaults to the agent's name; no hooks, no conversation.
        assert_eq!(l.argv, ["aider", "--quiet"]);
        assert_eq!(l.conversation_id, None);
        let aider = known("aider", &config).unwrap();
        assert!(!aider.reports() && !aider.resumes() && aider.choices().is_none());
        assert!(adapter(CLAUDE).resumes());
        // An effort means nothing to it.
        assert!(check_effort("aider", "extreme").is_ok());
        // Fitting the preview: the agent's setting, else the global one, else
        // full-screen agents.
        use crate::config::FitPreview;
        let mut config = config;
        assert_eq!(fit_mode("aider", &config), FitPreview::Auto);
        config.fit_preview = Some(FitPreview::Always);
        assert_eq!(
            (fit_mode("aider", &config), fit_mode("claude", &config)),
            (FitPreview::Always, FitPreview::Always)
        );
        config.agents.get_mut("aider").unwrap().fit_preview = Some(FitPreview::Never);
        assert_eq!(
            (fit_mode("aider", &config), fit_mode("claude", &config)),
            (FitPreview::Never, FitPreview::Always)
        );
        // Built-in agents first: Claude, then the others installed here, then
        // the one only the config knows.
        let mut expected: Vec<String> = vec![CLAUDE.into()];
        for other in [PI, CODEX] {
            if crate::util::on_path(other) {
                expected.push(other.into());
            }
        }
        expected.push("aider".into());
        assert_eq!(available(&config), expected);
        assert_eq!(idle_after_ms("aider", &config), 2000);
        assert_eq!(idle_after_ms("claude", &config), QUIET_MS);
    }

    #[test]
    fn built_in_agents_are_offered_once_installed_or_configured() {
        let mut config = Config::default();
        config.agents.insert("pi".into(), AgentConfig::default());
        assert!(known("pi", &config).unwrap().reports());
        // Configured, so offered whether or not it's installed here; codex
        // only if it is.
        let mut expected = vec![CLAUDE, PI];
        if crate::util::on_path(CODEX) {
            expected.push(CODEX);
        }
        assert_eq!(available(&config), expected);
        assert!(known("claude", &Config::default()).is_ok());
    }

    #[test]
    fn the_default_agent_comes_first() {
        let mut config = config_with("aider", AgentConfig::default());
        config.default_agent = Some("aider".into());
        assert_eq!(
            available(&config).first().map(String::as_str),
            Some("aider")
        );
        assert!(available(&config).contains(&"claude".to_string()));
    }

    #[test]
    fn a_parsed_messages_text_is_read_as_a_transcripts_is() {
        let text = |v: serde_json::Value| message_text(&v);
        assert_eq!(
            text(serde_json::json!("  fix it \n")).as_deref(),
            Some("fix it")
        );
        assert_eq!(
            text(
                serde_json::json!([{"type": "text", "text": "one"}, {"type": "image"},
                {"type": "input_text", "text": "two"}, {"type": "output_text", "text": "no"}])
            )
            .as_deref(),
            Some("one\ntwo")
        );
        assert_eq!(text(serde_json::json!("   ")), None);
        assert_eq!(text(serde_json::json!({"text": "a map"})), None);
    }

    #[test]
    fn each_agent_says_what_it_can_do_as_it_does() {
        let state = tempfile::tempdir().unwrap();
        let cx = Context {
            roster: Path::new("/roster"),
            state_dir: state.path(),
        };
        for name in BUILT_IN.iter().copied().chain(["aider"]) {
            let a = adapter(name);
            assert_eq!(
                a.keeps_transcript(),
                a.transcript(Path::new("/t.jsonl")).is_some(),
                "{name}"
            );
            assert_eq!(
                a.runs_oneoff(),
                a.oneoff(&Config::default()).is_some(),
                "{name}"
            );
            // A new session's start: given a conversation, unless the agent
            // names its own (or keeps none to resume).
            let mut s = session("a");
            s.agent = name.into();
            let given = a
                .launch(&s, vec![name.into()], &cx)
                .unwrap()
                .conversation_id;
            assert_eq!(
                given.is_some(),
                a.resumes() && !a.names_its_conversation(),
                "{name}"
            );
        }
    }

    #[test]
    fn resumes_only_a_conversation_roster_knows() {
        let mut s = session("a");
        s.agent = "codex".into();
        // Codex hasn't said which it's on (its hooks not run, say).
        assert!(!resumes(&s));
        assert!(new_conversation_question(&s).contains("hasn't said which conversation"));
        // A copy forks the one it was adopted from until it has its own.
        s.fork_from = Some("01a1".into());
        assert!(resumes(&s));
        s.fork_from = None;
        s.conversation_id = Some("01a2".into());
        assert!(resumes(&s));
        // An agent that can't resume doesn't, whatever roster knows.
        s.agent = "aider".into();
        assert!(!resumes(&s));
        assert!(new_conversation_question(&s).contains("can't resume its conversation"));
    }

    #[test]
    fn a_conversation_gone_after_its_first_turn_isnt_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("c-1.jsonl");
        let mut s = session("a");
        s.conversation_id = Some("c-1".into());
        s.transcript_path = Some(transcript.to_string_lossy().into_owned());
        // Not written yet: before its first message, or just cleared.
        assert!(resumes(&s) && !conversation_lost(&s));
        // A copy, forking its original till it has its own.
        s.fork_from = Some("c-0".into());
        assert!(resumes(&s));
        // Written, then deleted: lost, and said why.
        s.conversation_begun = true;
        std::fs::write(&transcript, "{}\n").unwrap();
        assert!(resumes(&s));
        std::fs::remove_file(&transcript).unwrap();
        assert!(conversation_lost(&s) && !resumes(&s));
        assert_eq!(
            new_conversation_question(&s),
            "demo/s-a (claude) can't resume its conversation: Claude no longer has it, as it deletes old ones. Start a new one?"
        );
        s.agent = "pi".into();
        assert!(!resumes(&s));
        assert!(
            new_conversation_question(&s).contains("Pi no longer has it, its file gone"),
            "{}",
            new_conversation_question(&s)
        );
        // Codex says so itself, if it can't find it.
        s.agent = "codex".into();
        assert!(!conversation_lost(&s) && resumes(&s));
    }

    #[test]
    fn models_are_learned_per_agent() {
        assert_eq!(learned_key("claude", "opus"), "opus");
        assert_eq!(learned_key("pi", "default"), "pi:default");
    }

    #[test]
    fn reports_in_rosters_own_terms() {
        let report: Report = serde_json::from_str(
            r#"{"conversation_id":"c-1","cleared":true,"title":null,"context_window":200000,"extra":1}"#,
        )
        .unwrap();
        assert_eq!(report.conversation_id.as_deref(), Some("c-1"));
        assert!(report.cleared);
        // A title that's gone is told apart from one not reported.
        assert_eq!(report.title, Some(None));
        assert_eq!(report.context_window, Some(200_000));
        let report: Report = serde_json::from_str(r#"{"title":"Fix it"}"#).unwrap();
        assert_eq!(report.title, Some(Some("Fix it".into())));
        assert_eq!(
            serde_json::from_str::<Report>("{}").unwrap(),
            Report::default()
        );
    }
}
