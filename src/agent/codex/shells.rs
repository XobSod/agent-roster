//! The commands codex leaves running past its turn, its background
//! terminals: `exec_command` given back a session id rather than an exit
//! code, and not polled to its end, or cut short by Esc as codex waited on
//! it. No hook says either way, so both are read from the rollout: those
//! running as the turn ends, from that turn's tool outputs; an end, from the
//! record codex writes of each command once it's over (`item_completed`, a
//! `CommandExecution` with that id as its `process_id`), ended by itself or
//! closed with `/stop`. Codex takes none of their results up, so an end is
//! reported by nothing else. It keeps that record only in a thread whose
//! history is paginated (its header's `history_mode`), which a thread begun
//! by an older codex isn't, resumed or forked: there none is taken for left
//! running, as none could be seen to end.
//!
//! Codex gives a command's id to a later one once it's over, so a command is
//! told by when it started too: by the call that started it, which its end
//! comes after. One cut short has no id said at all: it's told by what it
//! runs, and by when it started.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::agent::{Backgrounded, ShellEnd, ShellEnds};

/// How much of a rollout's end is read for its last turn's terminals: a turn
/// longer than that has those started before it left out.
const TAIL: u64 = 4 * 1024 * 1024;

/// What a tool's output says of a command still running, as the plain
/// `exec_command` says it.
const RUNNING: &str = "Process running with session ID ";

/// What a tool's output says once Esc cut it short: a command it was
/// waiting on runs on.
const CUT_SHORT: &str = "aborted by user after ";

/// How far apart codex may time a tool call's line and the command it ran
/// (ms): a line is timed as it's written, a command as it ran.
const SLACK_MS: i64 = 1_000;

/// How many processes run a command, run in a directory (paths in it
/// relative to that), started between two times (ms), as far as given
/// (`procs::running_in`).
pub(super) type Running<'a> =
    dyn Fn(&str, Option<&str>, Option<i64>, Option<i64>) -> Vec<crate::procs::SessionCommand> + 'a;

/// A tool call in the turn.
struct Call {
    /// When its line was written (ms).
    at: Option<i64>,
    /// Where its line is in the rollout (bytes).
    line: u64,
    /// It starts commands and polls none: so a command it says runs
    /// started after it.
    starts: bool,
    /// The command it runs, if it's one alone, and where, if it says.
    cmd: Option<String>,
    dir: Option<String>,
}

/// A command said to run in the turn, by a call starting it or polling it.
struct Started {
    /// Codex's id for it.
    id: String,
    /// When its call gave it back running (ms).
    at: Option<i64>,
    /// The call's id.
    call: Option<String>,
    /// Where the line saying it runs is in the rollout (bytes).
    line: u64,
}

/// A call codex was waiting on, cut short by Esc.
struct CutShort {
    call: String,
    /// When (ms), and where its line is (bytes).
    at: Option<i64>,
    line: u64,
}

/// A command's end, as codex records it.
struct Over {
    /// Its id, the id codex gave the command (its process's
    /// `CODEX_TOOL_CALL_ID`), and what it ran, as far as said.
    id: Option<String>,
    item: Option<String>,
    cmd: Option<String>,
    /// When it started and ended (ms).
    started: Option<i64>,
    ended: Option<i64>,
    /// Where its line is in the rollout (bytes).
    line: u64,
}

/// A command said to run in the turn: by the call that started it, or a
/// call polling one started before it.
struct Command {
    id: String,
    /// When it was given back running (ms), and where that's written.
    at: Option<i64>,
    line: u64,
    /// What it runs, if its call says.
    what: Option<String>,
    /// The call that started it, if one in the turn did, and when that, or
    /// the one polling it, was made (ms).
    call: Option<String>,
    called: Option<i64>,
}

impl Over {
    /// Whether it's the end of the command `c`: of its id, started after
    /// the call starting it, or over after the call polling it. What can't
    /// be dated is taken for it.
    fn ends(&self, c: &Command) -> bool {
        if self.id.as_deref() != Some(&c.id) {
            return false;
        }
        let after = if c.call.is_some() {
            self.started
        } else {
            self.ended
        };
        after.zip(c.called).is_none_or(|(t, at)| t >= at - SLACK_MS)
    }

    /// Whether it's the end of the command `call` ran, cut short by Esc at
    /// `cut`: one that runs the same, started between the two.
    fn ends_cut_short(&self, call: &Call, cut: Option<i64>) -> bool {
        self.cmd.is_some()
            && self.cmd == call.cmd
            && self.line > call.line
            && (self.started.zip(call.at)).is_none_or(|(t, at)| t >= at - SLACK_MS)
            && (self.started.zip(cut)).is_none_or(|(t, cut)| t <= cut + SLACK_MS)
    }
}

/// The terminals the rollout at `transcript` shows its last turn left
/// running: started in it, with no end recorded anywhere in it. Codex writes
/// a call's output once the response it's in is over, and a command's end
/// as it ends, which can come first, so an output saying one runs (its own
/// call's, a poll's) may come after its end. The rollout as far as `upto`
/// (bytes), if given: as long as it was as that turn ended, rather than a
/// later turn's, read late. Each with when it started, and where it was
/// seen running: its end is looked for after, as before it codex may have
/// given its id to a command over since.
/// `seen_running` says which processes run a command (`Running`).
pub(super) fn left(
    transcript: &Path,
    upto: Option<u64>,
    seen_running: &Running<'_>,
) -> Vec<Backgrounded> {
    let Some((tail, offset)) = tail(transcript, upto) else {
        return Vec::new();
    };
    // From the turn's start, or as much of it as was read.
    let turn = memchr::memmem::rfind(&tail, br#""type":"task_started""#).map_or(0, |at| {
        memchr::memrchr(b'\n', &tail[..at]).map_or(0, |i| i + 1)
    });
    let (mut started, mut calls) = (Vec::<Started>::new(), HashMap::<String, Call>::new());
    let (mut cut_short, mut over) = (Vec::<CutShort>::new(), Vec::<Over>::new());
    // Where the turn's commands run unless one says (`turn_context`):
    // Some(None) once looked for further back and not found.
    let mut turn_dir: Option<Option<String>> = None;
    // Only lines of tool calls and commands' ends, which few are, parsed.
    let finders = [
        r#""function_call"#,
        r#""custom_tool_call"#,
        r#""item_completed""#,
        r#""turn_context""#,
    ]
    .map(memchr::memmem::Finder::new);
    let mut next = offset + turn as u64;
    for line in tail[turn..].split(|b| *b == b'\n') {
        let at_line = next;
        next += line.len() as u64 + 1;
        if !finders.iter().any(|f| f.find(line).is_some()) {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let at = entry["timestamp"].as_str().and_then(crate::util::iso_ms);
        let payload = &entry["payload"];
        match payload["type"].as_str() {
            Some("function_call" | "custom_tool_call") => {
                if let Some(id) = payload["call_id"].as_str() {
                    let call = Call {
                        at,
                        line: at_line,
                        starts: starts(payload),
                        cmd: command(payload),
                        dir: exec_argument(payload, "workdir"),
                    };
                    calls.insert(id.to_string(), call);
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let call = payload["call_id"].as_str().map(String::from);
                let ids = running(&payload["output"]);
                if let Some(call) = call.clone().filter(|_| ids.is_empty())
                    && cut(&payload["output"])
                {
                    cut_short.push(CutShort {
                        call,
                        at,
                        line: at_line,
                    });
                }
                started.extend(ids.into_iter().map(|id| Started {
                    id,
                    at,
                    call: call.clone(),
                    line: at_line,
                }));
            }
            Some("item_completed") => over.extend(finished(payload, at_line)),
            // Where its commands run unless one says otherwise.
            _ if entry["type"] == "turn_context" => {
                if let Some(cwd) = payload["cwd"].as_str() {
                    turn_dir = Some(Some(cwd.to_string()));
                }
            }
            _ => {}
        }
    }
    if (!started.is_empty() || !cut_short.is_empty()) && !ends_recorded(transcript) {
        return Vec::new();
    }
    // Each command: one a call started, or one polled of a command started
    // before, the last with its id, of the turn or before it.
    let mut commands: Vec<Command> = Vec::new();
    for s in started {
        let call = s.call.as_ref().and_then(|id| calls.get(id));
        let starts = call.is_some_and(|c| c.starts);
        if !starts && commands.iter().any(|c| c.id == s.id) {
            continue;
        }
        commands.push(Command {
            what: call.and_then(|c| c.cmd.clone()),
            called: call.and_then(|c| c.at),
            call: s.call.filter(|_| starts),
            id: s.id,
            at: s.at,
            line: s.line,
        });
    }
    // Each over by an end of its own, the first that fits: an end isn't two
    // commands', as codex gives one's id to the next.
    let mut used = vec![false; over.len()];
    let mut left: Vec<Backgrounded> = Vec::new();
    for c in commands {
        if let Some(i) = (0..over.len()).find(|&i| !used[i] && over[i].ends(&c)) {
            used[i] = true;
            continue;
        }
        // One before it of its id is over, its id given to this one.
        left.retain(|l| l.id != c.id);
        left.push(Backgrounded {
            what: c.what,
            started: c.at,
            ran: 0,
            from: c.line,
            called: c.called.filter(|_| c.call.is_some()),
            call: c.call,
            ends_as: Vec::new(),
            id: c.id,
        });
    }
    // Cut short as codex waited on it, its id unsaid: by what it runs, if a
    // process of it is seen running, started since its call, each its own,
    // the first started for the first called. One waiting on an approval,
    // cancelled, never ran, its output the same.
    let mut seen = std::collections::HashSet::new();
    for c in cut_short {
        let Some(call) = calls.get(&c.call).filter(|call| call.starts) else {
            continue;
        };
        let Some(cmd) = &call.cmd else { continue };
        if let Some(i) = (0..over.len()).find(|&i| !used[i] && over[i].ends_cut_short(call, c.at)) {
            used[i] = true;
            continue;
        }
        let (from, to) = (
            call.at.map(|at| at - SLACK_MS),
            c.at.map(|at| at + SLACK_MS),
        );
        // A command's processes (codex's sandbox, the shell, the command)
        // are one, by the id codex gave it.
        let command =
            |p: &crate::procs::SessionCommand| p.call.clone().unwrap_or(p.pid.to_string());
        // Where its turn runs, from its own start if it's further back
        // than what was read (a long turn), else from the thread's.
        if turn_dir.is_none() {
            turn_dir = Some(launch_dir(transcript, offset + turn as u64));
        }
        let dir = call
            .dir
            .as_deref()
            .or(turn_dir.as_ref().and_then(|d| d.as_deref()));
        let running = seen_running(cmd, dir, from, to);
        let Some(process) = running.into_iter().find(|p| !seen.contains(&command(p))) else {
            continue;
        };
        seen.insert(command(&process));
        // Its end by the id codex gave it, where its process said, or by what
        // it runs.
        let ends_as = process.call.into_iter().chain([by_command(cmd)]).collect();
        left.push(Backgrounded {
            id: format!("cut-{}", c.call),
            what: Some(cmd.clone()),
            started: c.at,
            ran: 0,
            from: c.line,
            called: call.at,
            ends_as,
            call: Some(c.call),
        });
    }
    left
}

/// The ends of commands the rollout at `transcript` records from byte `from`
/// on, and how far that was read: to the end of its last whole line. From
/// its start if it's shorter than `from` now. None if it can't be read.
/// Each by its id, and by the id codex gave the command and what it ran,
/// for one cut short whose id went unsaid.
pub(super) fn ended(transcript: &Path, from: u64) -> Option<(ShellEnds, u64)> {
    let mut file = File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let from = if from > len { 0 } else { from };
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut reader = BufReader::new(file.take(len - from));
    let finder = memchr::memmem::Finder::new(r#""CommandExecution""#);
    let (mut ends, mut read, mut line) = (Vec::new(), from, Vec::new());
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).ok()?;
        // A line still being written is read whole next time.
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let start = read;
        read += n as u64;
        if finder.find(&line).is_some()
            && let Ok(entry) = serde_json::from_slice::<Value>(&line)
            && let Some(over) = finished(&entry["payload"], start)
        {
            ends.push(ShellEnd {
                id: over.id.unwrap_or_default(),
                at: over.ended,
                // Told to no one, as codex takes nothing up.
                told: true,
                line: over.line,
                started: over.started,
                also: [over.item, over.cmd.as_deref().map(by_command)]
                    .into_iter()
                    .flatten()
                    .collect(),
            });
        }
    }
    Some((ends, read))
}

/// A command's end, from an `item_completed` event's `payload` on the line
/// at `line`.
fn finished(payload: &Value, line: u64) -> Option<Over> {
    let item = &payload["item"];
    if payload["type"] != "item_completed" || item["type"] != "CommandExecution" {
        return None;
    }
    // What it ran: the shell's command line, its script last.
    let cmd = match &item["command"] {
        Value::Array(argv) => argv.last().and_then(Value::as_str),
        other => other.as_str(),
    };
    Some(Over {
        id: item["process_id"].as_str().map(String::from),
        item: (item["id"].as_str())
            .filter(|id| !id.is_empty())
            .map(String::from),
        cmd: cmd.map(crate::util::one_line).filter(|c| !c.is_empty()),
        started: payload["started_at_ms"].as_i64(),
        ended: payload["completed_at_ms"].as_i64(),
        line,
    })
}

/// The id a command cut short is known by, codex having said none: what it
/// runs.
fn by_command(cmd: &str) -> String {
    format!("cmd-{}", crate::util::fnv1a(cmd.as_bytes()))
}

/// Whether a tool's `output` says Esc cut it short.
fn cut(output: &Value) -> bool {
    match output {
        Value::String(text) => text.starts_with(CUT_SHORT),
        Value::Array(items) => {
            (items.iter()).any(|i| i["text"].as_str().is_some_and(|t| t.starts_with(CUT_SHORT)))
        }
        _ => false,
    }
}

/// The ids of the commands a tool's `output` gives back still running: as
/// the plain tool says it, on a line of its own in what it says before the
/// command's output (`Output:`), which may say anything; or as code mode's
/// `exec_command` returns it, an object with a `session_id` beside its
/// `wall_time_seconds` (and no exit code), which the script printed. Not one
/// quoted in a command's output, which is escaped in it.
fn running(output: &Value) -> Vec<String> {
    let texts: Vec<&str> = match output {
        Value::String(text) => vec![text],
        Value::Array(items) => items.iter().filter_map(|i| i["text"].as_str()).collect(),
        _ => Vec::new(),
    };
    let mut ids = Vec::new();
    for text in texts {
        if let Some((head, _)) = text.split_once("\nOutput:")
            && head.lines().any(|line| line.starts_with("Wall time: "))
        {
            let found = head
                .lines()
                .filter_map(|l| l.strip_prefix(RUNNING).and_then(digits));
            ids.extend(found);
            continue;
        }
        if let Ok(printed) = serde_json::from_str::<Value>(text.trim()) {
            results(&printed, &mut ids);
            continue;
        }
        // Printed among other things: the object around each id.
        const KEY: &str = r#""session_id":"#;
        for (at, _) in text.match_indices(KEY) {
            let start = text[..at].rfind('{').unwrap_or(at);
            let end = text[at..].find('}').map_or(text.len(), |i| at + i);
            let object = &text[start..end];
            if object.contains(r#""wall_time_seconds":"#)
                && !object.contains(r#""exit_code":"#)
                && let Some(id) = digits(&text[at + KEY.len()..])
            {
                ids.push(id);
            }
        }
    }
    ids
}

/// The ids of `exec_command`'s results still running in what a script
/// printed as JSON: the result, or a list of them.
fn results(printed: &Value, ids: &mut Vec<String>) {
    match printed {
        Value::Array(items) => items.iter().for_each(|item| results(item, ids)),
        Value::Object(result)
            if result.contains_key("wall_time_seconds") && !result.contains_key("exit_code") =>
        {
            ids.extend(
                result
                    .get("session_id")
                    .and_then(Value::as_u64)
                    .map(|id| id.to_string()),
            );
        }
        _ => {}
    }
}

/// Where the turn whose start (or as much of it as was read) is at byte
/// `before` of the rollout at `path` runs its commands: as its last
/// `turn_context` before that says, else as the thread's header does.
fn launch_dir(path: &Path, before: u64) -> Option<String> {
    let cwd = |e: &Value| e["payload"]["cwd"].as_str().map(String::from);
    let context = super::transcript::last_entry_back(
        path,
        Some(before),
        super::transcript::REVIEWER_REACH,
        br#""type":"turn_context""#,
        None,
        |e| cwd(e).is_some(),
    );
    if let Some(dir) = context.as_ref().and_then(cwd) {
        return Some(dir);
    }
    let mut header = Vec::new();
    BufReader::new(File::open(path).ok()?)
        .read_until(b'\n', &mut header)
        .ok()?;
    serde_json::from_slice::<Value>(&header)
        .ok()
        .as_ref()
        .and_then(cwd)
}

/// Whether codex records the ends of commands in the rollout at `path`, as
/// its header, the first line, says.
fn ends_recorded(path: &Path) -> bool {
    let mut header = Vec::new();
    File::open(path).is_ok_and(|file| BufReader::new(file).read_until(b'\n', &mut header).is_ok())
        && serde_json::from_slice::<Value>(&header)
            .is_ok_and(|e| e["payload"]["history_mode"] == "paginated")
}

/// The number `text` starts with.
fn digits(text: &str) -> Option<String> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    (end > 0).then(|| text[..end].to_string())
}

/// Whether a tool call starts commands and polls none: a plain
/// `exec_command`, or code mode's script calling it and not `write_stdin`.
fn starts(payload: &Value) -> bool {
    match payload["type"].as_str() {
        Some("function_call") => payload["name"] == "exec_command",
        Some("custom_tool_call") if payload["name"] == "exec" => {
            payload["input"].as_str().is_some_and(|script| {
                let script = Script::read(script);
                !script.calls("exec_command").is_empty() && script.calls("write_stdin").is_empty()
            })
        }
        _ => false,
    }
}

/// The command a tool call runs: a plain `exec_command`'s `cmd`; or, in
/// code mode's script, the one `exec_command` call's (of several, which is
/// which can't be told).
fn command(payload: &Value) -> Option<String> {
    let cmd = crate::util::one_line(&exec_argument(payload, "cmd")?);
    (!cmd.is_empty()).then_some(cmd)
}

/// The argument `key` (`cmd`, `workdir`) a tool call gives `exec_command`:
/// a plain one's; or, in code mode's script, the one call's (of several,
/// which is which can't be told).
fn exec_argument(payload: &Value, key: &str) -> Option<String> {
    match payload["type"].as_str()? {
        "function_call" if payload["name"] == "exec_command" => {
            let arguments: Value = serde_json::from_str(payload["arguments"].as_str()?).ok()?;
            Some(arguments[key].as_str()?.to_string())
        }
        "custom_tool_call" if payload["name"] == "exec" => {
            let script = Script::read(payload["input"].as_str()?);
            match script.calls("exec_command")[..] {
                [at] => script.property(at, key),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Code mode's script as JavaScript reads it: `code` is its text with its
/// comments and what its literals hold (strings', a template's outside its
/// `${…}`, regular expressions') blanked, their quotes kept; `ends` says
/// where each string or template literal ends, by where it starts.
struct Script<'a> {
    text: &'a str,
    code: Vec<u8>,
    ends: HashMap<usize, usize>,
}

/// The words after which a `/` starts a regular expression, not a division.
const BEFORE_A_VALUE: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

/// The words whose `(…)` is a condition, after which a statement starts.
const WITH_A_CONDITION: &[&str] = &["if", "while", "for", "with"];

impl<'a> Script<'a> {
    fn read(text: &'a str) -> Self {
        let bytes = text.as_bytes();
        let mut script = Script {
            text,
            code: bytes.to_vec(),
            ends: HashMap::new(),
        };
        // The templates whose `${…}` is being read, the outermost first:
        // each one's start, and the braces open in its expression.
        let mut templates: Vec<(usize, usize)> = Vec::new();
        // For each `(` open, the word whose condition it is, if one is.
        let mut parens: Vec<Option<&str>> = Vec::new();
        // Whether a `/` here starts a regular expression: not after a value.
        let mut regex = true;
        // The word just before, if it was one (not a property's name), and
        // whether a `.` was, a property's name following it.
        let (mut word, mut dot): (Option<&str>, bool) = (None, false);
        let mut i = 0;
        while i < bytes.len() {
            let (c, start) = (bytes[i], i);
            let (mut after, mut after_dot) = (None, false);
            match c {
                b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c' => {
                    i += 1;
                    continue;
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    i = line_end(bytes, i);
                    script.code[start..i].fill(b' ');
                    continue;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    i = memchr::memmem::find(&bytes[i + 2..], b"*/")
                        .map_or(bytes.len(), |n| i + n + 4);
                    script.code[start..i].fill(b' ');
                    continue;
                }
                b'/' if regex => {
                    i = regex_end(bytes, i);
                    script.code[start + 1..i].fill(b' ');
                    regex = false;
                }
                b'\'' | b'"' => {
                    let end = string_end(bytes, i, c);
                    i = end.unwrap_or(bytes.len());
                    script.code[start + 1..end.map_or(i, |end| end - 1)].fill(b' ');
                    script.ends.insert(start, i);
                    regex = false;
                }
                b'`' => {
                    let closed;
                    (i, closed) = script.template(start, i + 1, &mut templates);
                    regex = !closed;
                }
                b'}' if templates.last().is_some_and(|t| t.1 == 0) => {
                    let (of, _) = templates.pop().expect("a template");
                    let closed;
                    (i, closed) = script.template(of, i + 1, &mut templates);
                    regex = !closed;
                }
                b'{' | b'}' => {
                    if let Some(t) = templates.last_mut() {
                        t.1 = if c == b'{' { t.1 + 1 } else { t.1 - 1 };
                    }
                    i += 1;
                    regex = true;
                }
                b'(' => {
                    parens.push(word.filter(|w| WITH_A_CONDITION.contains(w)));
                    i += 1;
                    regex = true;
                }
                b')' => {
                    i += 1;
                    regex = parens.pop().flatten().is_some();
                }
                // A value's end, as `]` is.
                b'+' | b'-' if bytes.get(i + 1) == Some(&c) => {
                    i += 2;
                    regex = false;
                }
                b']' => {
                    i += 1;
                    regex = false;
                }
                b'.' => {
                    i += 1;
                    after_dot = true;
                    regex = false;
                }
                c if js_word(c) || c >= 0x80 => {
                    let ch = text[i..].chars().next().expect("a character");
                    // A blank (or a line's end) of Unicode's, as one of ASCII's.
                    if !in_a_name(ch) {
                        script.code[i..i + ch.len_utf8()].fill(b' ');
                        i += ch.len_utf8();
                        continue;
                    }
                    i = word_end(text, i);
                    let name = &text[start..i];
                    // A property's name is no word of the language.
                    if dot {
                        regex = false;
                    } else {
                        regex = match name {
                            // One only in a `for (… of …)`, else a name.
                            "of" => parens.last() == Some(&Some("for")),
                            name => BEFORE_A_VALUE.contains(&name),
                        };
                        // `for await (…)` is a `for`'s.
                        let for_await = name == "await" && word == Some("for");
                        after = Some(if for_await { "for" } else { name });
                    }
                }
                _ => {
                    i += 1;
                    regex = true;
                }
            }
            (word, dot) = (after, after_dot);
        }
        script
    }

    /// Reads the text of the template started at `start` from `from` on,
    /// blanked: to its end (true), or into an expression in it, `${`.
    /// Where it got to.
    fn template(
        &mut self,
        start: usize,
        from: usize,
        templates: &mut Vec<(usize, usize)>,
    ) -> (usize, bool) {
        let bytes = self.text.as_bytes();
        let mut i = from;
        let (end, next, closed) = loop {
            match bytes.get(i) {
                None => break (bytes.len(), bytes.len(), true),
                Some(b'\\') => i += 2,
                Some(b'`') => break (i, i + 1, true),
                Some(b'$') if bytes.get(i + 1) == Some(&b'{') => break (i, i + 2, false),
                Some(_) => i += 1,
            }
        };
        let end = end.min(bytes.len());
        self.code[from..end].fill(b' ');
        if closed {
            self.ends.insert(start, next);
        } else {
            templates.push((start, 0));
        }
        (next, closed)
    }

    /// The first byte at or after `i` that isn't blank in the code.
    fn blanks(&self, mut i: usize) -> usize {
        while self.code.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
            i += 1;
        }
        i
    }

    /// Where the script's code calls `name`: past its `(`.
    fn calls(&self, name: &str) -> Vec<usize> {
        memchr::memmem::find_iter(&self.code, name.as_bytes())
            // Not the end of a longer name: what's before it is blank,
            // punctuation, or none.
            .filter(|&at| {
                at == 0
                    || self.code[at - 1] == b' '
                    || !self.text[..at].chars().next_back().is_some_and(in_a_name)
            })
            .filter(|&at| word_end(self.text, at) == at + name.len())
            .map(|at| self.blanks(at + name.len()))
            .filter(|&at| self.code.get(at) == Some(&b'('))
            .map(|at| at + 1)
            .collect()
    }

    /// The string value of the property `key` of the object literal the
    /// call whose arguments start at `from` is given first: as JavaScript
    /// takes it, from its last `key`. None if it isn't a string literal,
    /// or may not be: what's after it and isn't understood (a spread, a
    /// computed key, an accessor) could give it another.
    fn property(&self, from: usize, key: &str) -> Option<String> {
        let code = &self.code;
        let mut i = self.blanks(from);
        if code.get(i) != Some(&b'{') {
            return None;
        }
        i += 1;
        // How deep inside the object's own values, whether a key comes
        // next, and what `key` is so far.
        let (mut depth, mut at_key, mut found) = (0usize, true, None);
        while let Some(&c) = code.get(i) {
            if c.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            // The object's end, after a comma or not.
            if depth == 0 && c == b'}' {
                return found;
            }
            if depth == 0 && at_key {
                // A property's name, plain or quoted, and where it ends.
                let name = match c {
                    b'\'' | b'"' => {
                        let end = *self.ends.get(&i)?;
                        js_string(&self.text[i..end]).map(|name| (name, end))
                    }
                    c if js_word(c) => {
                        let end = word_end(self.text, i);
                        let name = &self.text[i..end];
                        name.bytes().all(js_word).then(|| (name.to_string(), end))
                    }
                    _ => None,
                };
                let next = name.as_ref().map(|(_, end)| self.blanks(*end));
                match (name, next.and_then(|n| code.get(n))) {
                    (Some((name, end)), Some(b':')) => {
                        if name == key {
                            found = self.value(end);
                        }
                        i = next.expect("a name") + 1;
                    }
                    // Its value a variable's, or a method.
                    (Some((name, end)), Some(b',' | b'}' | b'(')) => {
                        if name == key {
                            found = None;
                        }
                        i = end;
                    }
                    // Anything else (a spread, a computed key, an accessor)
                    // may give it a value.
                    _ => found = None,
                }
                at_key = false;
                continue;
            }
            match c {
                b'\'' | b'"' | b'`' => i = *self.ends.get(&i)?,
                b'{' | b'[' | b'(' => (i, depth) = (i + 1, depth + 1),
                b']' | b')' if depth == 0 => return None,
                b'}' | b']' | b')' => (i, depth) = (i + 1, depth - 1),
                b',' => (i, at_key) = (i + 1, depth == 0),
                _ => i += 1,
            }
        }
        None
    }

    /// The string a key whose name ends at `from` is given, if it's given
    /// one: a literal alone, up to the next property or the object's end.
    fn value(&self, from: usize) -> Option<String> {
        let at = self.blanks(from);
        if self.code.get(at) != Some(&b':') {
            return None;
        }
        let at = self.blanks(at + 1);
        let end = *self.ends.get(&at)?;
        if !matches!(self.code.get(self.blanks(end)), Some(b',' | b'}')) {
            return None;
        }
        let literal = &self.text[at..end];
        // What a template puts in it isn't known.
        if literal.starts_with('`') && literal.contains("${") {
            return None;
        }
        js_string(literal)
    }
}

/// Whether `c` can be part of a JavaScript name.
fn js_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// Whether `c` can be part of a JavaScript name (or number): any
/// character but ASCII punctuation and blanks.
fn in_a_name(c: char) -> bool {
    if c.is_ascii() {
        js_word(c as u8)
    } else {
        !c.is_whitespace() && c != '\u{feff}'
    }
}

/// Where the name (or number) at byte `at` of `text` ends.
fn word_end(text: &str, at: usize) -> usize {
    text[at..]
        .char_indices()
        .find(|&(_, c)| !in_a_name(c))
        .map_or(text.len(), |(n, _)| at + n)
}

/// Whether byte `i` of `bytes` ends a line, as JavaScript ends one: an LF,
/// a CR, or a Unicode line or paragraph separator.
fn ends_a_line(bytes: &[u8], i: usize) -> bool {
    match bytes.get(i) {
        Some(b'\n' | b'\r') => true,
        Some(0xE2) => {
            bytes[i..].starts_with("\u{2028}".as_bytes())
                || bytes[i..].starts_with("\u{2029}".as_bytes())
        }
        _ => false,
    }
}

/// Where the line that byte `from` is in ends.
fn line_end(bytes: &[u8], from: usize) -> usize {
    (from..bytes.len())
        .find(|&i| ends_a_line(bytes, i))
        .unwrap_or(bytes.len())
}

/// Where the regular expression literal at `start` ends: past its closing
/// slash and its flags, a backslash escaping what follows it, a class
/// (`[…]`) holding slashes; at its line's end if it isn't closed by then.
fn regex_end(bytes: &[u8], start: usize) -> usize {
    let (mut i, mut class) = (start + 1, false);
    while i < bytes.len() && !ends_a_line(bytes, i) {
        match bytes[i] {
            b'\\' if !ends_a_line(bytes, i + 1) => i += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                i += 1;
                while bytes.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
                    i += 1;
                }
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    i.min(bytes.len())
}

/// Where the string literal opened by `quote` at `start` ends: just past
/// its closing quote, a backslash escaping what follows it.
fn string_end(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            c if c == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// What the string literal `literal` is, in any of JavaScript's quotes,
/// its escapes taken as JavaScript takes them (a template's line ends as
/// `\n`); None for one that isn't closed, or with an escape strict code
/// doesn't take (an octal one).
fn js_string(literal: &str) -> Option<String> {
    let mut chars = literal.chars();
    let quote = chars.next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let body = chars.as_str().strip_suffix(quote)?;
    let mut chars = body.chars().peekable();
    // As UTF-16, which `\u` escapes give in halves.
    let mut units: Vec<u16> = Vec::with_capacity(body.len());
    let push = |units: &mut Vec<u16>, c: char| {
        units.extend_from_slice(c.encode_utf16(&mut [0; 2]));
    };
    let hex = |chars: &mut std::iter::Peekable<std::str::Chars>, n: usize| {
        (0..n).try_fold(0u32, |v, _| Some(v * 16 + chars.next()?.to_digit(16)?))
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => push(&mut units, '\n'),
                't' => push(&mut units, '\t'),
                'r' => push(&mut units, '\r'),
                'b' => push(&mut units, '\u{8}'),
                'f' => push(&mut units, '\u{c}'),
                'v' => push(&mut units, '\u{b}'),
                '0' if !chars.peek().is_some_and(char::is_ascii_digit) => push(&mut units, '\0'),
                '0'..='9' => return None,
                'x' => units.push(hex(&mut chars, 2)? as u16),
                'u' if chars.peek() == Some(&'{') => {
                    chars.next();
                    let mut v = 0u32;
                    let mut digits = 0;
                    loop {
                        match chars.next()? {
                            '}' if digits > 0 => break,
                            c => v = v.checked_mul(16)? + c.to_digit(16)?,
                        }
                        digits += 1;
                    }
                    match char::from_u32(v) {
                        Some(c) => push(&mut units, c),
                        None if v <= 0xFFFF => units.push(v as u16),
                        None => return None,
                    }
                }
                'u' => units.push(hex(&mut chars, 4)? as u16),
                // A line continued.
                '\r' => {
                    chars.next_if_eq(&'\n');
                }
                '\n' | '\u{2028}' | '\u{2029}' => {}
                c => push(&mut units, c),
            },
            '\r' if quote == '`' => {
                chars.next_if_eq(&'\n');
                push(&mut units, '\n');
            }
            c => push(&mut units, c),
        }
    }
    String::from_utf16(&units).ok()
}

/// The rollout's last `TAIL` bytes before `upto` (its end, if not given or
/// past it) from a line's start, and where that is in it.
fn tail(path: &Path, upto: Option<u64>) -> Option<(Vec<u8>, u64)> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let end = upto.map_or(len, |upto| upto.min(len));
    let start = end.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(end - start).read_to_end(&mut tail).ok()?;
    if start == 0 {
        return Some((tail, 0));
    }
    let cut = memchr::memchr(b'\n', &tail).map_or(tail.len(), |i| i + 1);
    tail.drain(..cut);
    Some((tail, start + cut as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Nothing seen running.
    fn nothing(
        _: &str,
        _: Option<&str>,
        _: Option<i64>,
        _: Option<i64>,
    ) -> Vec<crate::procs::SessionCommand> {
        Vec::new()
    }

    /// A process of the session's, as `procs::session_commands` reads it.
    fn process(pid: i32, started: i64, call: Option<&str>) -> crate::procs::SessionCommand {
        crate::procs::SessionCommand {
            pid,
            started,
            argv: ["/usr/bin/zsh", "-lc", "sleep 30"]
                .map(String::from)
                .to_vec(),
            call: call.map(String::from),
            cwd: None,
        }
    }

    fn line(at: &str, payload: Value) -> String {
        let kind = if payload["type"].as_str().is_some_and(|t| t.contains("call")) {
            "response_item"
        } else {
            "event_msg"
        };
        json!({"timestamp": at, "type": kind, "payload": payload}).to_string()
    }

    /// Code mode's `exec`, as 0.165 writes it: the script, and what it
    /// printed of `exec_command`'s result.
    fn code_mode(call: &str, at: &str, script: &str, result: Value) -> [String; 2] {
        [
            line(
                at,
                json!({"type": "custom_tool_call", "call_id": call, "name": "exec", "input": script}),
            ),
            line(
                at,
                json!({"type": "custom_tool_call_output", "call_id": call, "output": [
                {"type": "input_text", "text": "Script completed\nWall time 0.4 seconds\nOutput:\n"},
                {"type": "input_text", "text": result.to_string()}]}),
            ),
        ]
    }

    fn completed(id: &str, at_ms: i64, status: &str) -> String {
        line(
            "2026-01-01T10:00:15.000Z",
            json!({"type": "item_completed", "item": {
            "type": "CommandExecution", "process_id": id, "command": ["/bin/sh", "-c", "x"],
            "source": "unified_exec_startup", "status": status}, "completed_at_ms": at_ms}),
        )
    }

    #[test]
    fn the_terminals_a_turn_left_running_are_read_from_its_rollout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let started = |at| line(at, json!({"type": "task_started"}));
        let header = |mode: &str| {
            json!({"timestamp": "2026-01-01T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": "01a1", "history_mode": mode}})
            .to_string()
        };
        let mut lines = vec![
            header("paginated"),
            // An earlier turn's, not this one's.
            started("2026-01-01T10:00:01.000Z"),
            line(
                "2026-01-01T10:00:02.000Z",
                json!({"type": "function_call_output", "call_id": "c0",
                "output": "Chunk ID: 1\nWall time: 1.0 seconds\nProcess running with session ID 1001\nOutput:\n"}),
            ),
            started("2026-01-01T10:00:03.000Z"),
        ];
        lines.extend(code_mode(
            "c1",
            "2026-01-01T10:00:04.000Z",
            "const r = await tools.exec_command({cmd:\"sleep 40; echo \\\"done\\\"\",yield_time_ms:250});\ntext(JSON.stringify(r));\n",
            json!({"chunk_id": "c1", "wall_time_seconds": 0.25, "session_id": 2001, "output": ""}),
        ));
        // Run to its end within the turn: an exit code, and its end recorded.
        lines.extend(code_mode(
            "c2",
            "2026-01-01T10:00:05.000Z",
            "text(JSON.stringify(await tools.exec_command({cmd: 'ls'})))",
            json!({"wall_time_seconds": 0.1, "exit_code": 0, "output": "a.txt"}),
        ));
        // Polled till it ended, mid-turn.
        lines.push(line("2026-01-01T10:00:06.000Z", json!({"type": "function_call", "call_id": "c3",
            "name": "exec_command", "arguments": "{\"cmd\": \"cargo test\", \"yield_time_ms\": 1000}"})));
        lines.push(line("2026-01-01T10:00:07.000Z", json!({"type": "function_call_output", "call_id": "c3",
            "output": "Chunk ID: 2\nWall time: 1.0 seconds\nProcess running with session ID 2003\nOutput:\n"})));
        lines.push(completed("2003", 1_767_261_610_000, "completed"));
        // Two in one script: which runs what can't be told. And a file's
        // text with a session id in it, quoted in what it printed.
        lines.extend(code_mode(
            "c4",
            "2026-01-01T10:00:08.000Z",
            "const a = await tools.exec_command({cmd:'sleep 600'}); const b = await tools.exec_command({cmd:'cat log.json'}); text(JSON.stringify(a))",
            json!({"wall_time_seconds": 0.25, "session_id": 2004,
                "output": "{\"wall_time_seconds\":1,\"session_id\":9999}"}),
        ));
        // What a command printed, a rollout's say, is no terminal of its own.
        lines.push(line("2026-01-01T10:00:09.000Z", json!({"type": "function_call_output", "call_id": "c5",
            "output": "Wall time: 0.1 seconds\nProcess exited with code 0\nOutput:\nProcess running with session ID 4242\n"})));
        // Over, then polled (write_stdin), the poll's output written after
        // its end with the rest of the response: over, under any call.
        let reused = |at, call| {
            line(
                at,
                json!({"type": "function_call_output", "call_id": call,
            "output": "Wall time: 1.0 seconds\nProcess running with session ID 2005\nOutput:\n"}),
            )
        };
        lines.push(reused("2026-01-01T10:00:10.000Z", "c6"));
        lines.push(completed("2005", 1_767_261_610_000, "completed"));
        lines.push(reused("2026-01-01T10:00:11.000Z", "c7"));
        // Over before its own call's output was written: over.
        lines.push(line(
            "2026-01-01T10:00:12.000Z",
            json!({"type": "item_completed",
            "item": {"type": "CommandExecution", "id": "c8", "process_id": "2008"},
            "completed_at_ms": 1_767_261_620_000_i64}),
        ));
        lines.push(line(
            "2026-01-01T10:00:13.000Z",
            json!({"type": "function_call_output", "call_id": "c8",
            "output": "Wall time: 10.0 seconds\nProcess running with session ID 2008\nOutput:\n"}),
        ));
        lines.push(line(
            "2026-01-01T10:00:14.000Z",
            json!({"type": "task_complete"}),
        ));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let turn = lines[..3].iter().map(|l| l.len() as u64 + 1).sum::<u64>();
        // Where each was seen running: its call's output.
        let text = lines.join("\n") + "\n";
        let seen = |call: &str| {
            let at = text
                .find(&format!(r#"{{"call_id":"{call}","output""#))
                .unwrap();
            text[..at].rfind('\n').map_or(0, |i| i as u64 + 1)
        };
        // When each started: as its call gave it back running.
        let at = crate::util::iso_ms;
        let found = vec![
            Backgrounded {
                id: "2001".into(),
                what: Some("sleep 40; echo \"done\"".into()),
                started: at("2026-01-01T10:00:04.000Z"),
                ran: 0,
                from: seen("c1"),
                call: Some("c1".into()),
                called: at("2026-01-01T10:00:04.000Z"),
                ends_as: Vec::new(),
            },
            Backgrounded {
                id: "2004".into(),
                what: None,
                started: at("2026-01-01T10:00:08.000Z"),
                ran: 0,
                from: seen("c4"),
                call: Some("c4".into()),
                called: at("2026-01-01T10:00:08.000Z"),
                ends_as: Vec::new(),
            },
        ];
        assert_eq!(left(&path, None, &nothing), found);
        // In a thread begun by an older codex, which records no ends: none.
        let mut legacy = lines.clone();
        legacy[0] = header("legacy");
        std::fs::write(&path, legacy.join("\n") + "\n").unwrap();
        assert!(left(&path, None, &nothing).is_empty());
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        // Read once another turn has begun and ended: as far as the rollout
        // went as this one ended, not the later one.
        let stopped = std::fs::metadata(&path).unwrap().len();
        let mut later = lines.clone();
        later.push(started("2026-01-01T10:00:16.000Z"));
        later.push(line(
            "2026-01-01T10:00:17.000Z",
            json!({"type": "task_complete"}),
        ));
        std::fs::write(&path, later.join("\n") + "\n").unwrap();
        assert_eq!(left(&path, Some(stopped), &nothing), found);
        assert!(left(&path, None, &nothing).is_empty());
        // Cut mid-line: that line not yet written. Past its end: all of it.
        assert_eq!(left(&path, Some(stopped + 7), &nothing), found);
        assert!(left(&path, Some(u64::MAX), &nothing).is_empty());
        // Their ends, as codex records them: by itself, and closed with /stop.
        let mut more = lines.clone();
        more.push(completed("2001", 1_767_261_630_000, "completed"));
        more.push(completed("2004", 1_767_261_640_000, "failed"));
        more.push(completed("2005", 1_767_261_650_000, "completed"));
        std::fs::write(&path, more.join("\n") + "\n").unwrap();
        let (ends, read) = ended(&path, turn).unwrap();
        // Each by what it ran too, where it says, for one cut short whose id
        // went unsaid; and by the id codex gave the command, where it says.
        let unsaid = ends.iter().filter(|e| e.also == ["c8"]).count();
        assert_eq!(unsaid, 1, "{ends:?}");
        assert!((ends.iter()).all(|e| e.also == ["c8"] || e.also == [by_command("x")]));
        let ids: Vec<(&str, Option<i64>, bool)> =
            ends.iter().map(|e| (e.id.as_str(), e.at, e.told)).collect();
        assert_eq!(
            ids,
            [
                ("2003", Some(1_767_261_610_000), true),
                ("2005", Some(1_767_261_610_000), true),
                ("2008", Some(1_767_261_620_000), true),
                ("2001", Some(1_767_261_630_000), true),
                ("2004", Some(1_767_261_640_000), true),
                ("2005", Some(1_767_261_650_000), true)
            ]
        );
        assert_eq!(read, std::fs::metadata(&path).unwrap().len());
        assert!(super::left(&path, None, &nothing).is_empty());
        // None in no rollout.
        assert!(super::left(&dir.path().join("none.jsonl"), None, &nothing).is_empty());
    }

    #[test]
    fn a_turn_runs_where_its_context_says_however_far_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let header = json!({"type": "session_meta", "payload": {"id": "01a1", "cwd": "/thread"}});
        let context = json!({"type": "turn_context", "payload": {"cwd": "/turn"}});
        let filler = json!({"type": "response_item", "payload": {"type": "function_call_output",
            "output": "x".repeat(700_000)}})
        .to_string();
        let write = |lines: &[String]| std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        // Its own context, however much was written since.
        let lines = [
            header.to_string(),
            context.to_string(),
            filler.clone(),
            filler.clone(),
        ];
        write(&lines);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(launch_dir(&path, len).as_deref(), Some("/turn"));
        // None: the thread's.
        write(&[header.to_string(), filler]);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(launch_dir(&path, len).as_deref(), Some("/thread"));
    }

    /// A rollout of one turn of `lines`, codex recording commands' ends.
    fn turn(dir: &Path, lines: &[String]) -> std::path::PathBuf {
        let path = dir.join("rollout.jsonl");
        let mut all = vec![
            json!({"timestamp": "2026-01-01T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": "01a1", "history_mode": "paginated"}})
            .to_string(),
            line("2026-01-01T10:00:00.500Z", json!({"type": "task_started"})),
        ];
        all.extend_from_slice(lines);
        std::fs::write(&path, all.join("\n") + "\n").unwrap();
        path
    }

    fn exec(call: &str, at: &str, cmd: &str) -> String {
        let arguments = json!({"cmd": cmd, "yield_time_ms": 1000}).to_string();
        line(
            at,
            json!({"type": "function_call", "call_id": call, "name": "exec_command", "arguments": arguments}),
        )
    }

    fn poll(call: &str, at: &str, id: u64) -> String {
        let arguments = json!({"session_id": id, "chars": ""}).to_string();
        line(
            at,
            json!({"type": "function_call", "call_id": call, "name": "write_stdin", "arguments": arguments}),
        )
    }

    fn output(call: &str, at: &str, text: &str) -> String {
        line(
            at,
            json!({"type": "function_call_output", "call_id": call, "output": text}),
        )
    }

    fn runs(id: u64) -> String {
        format!("Wall time: 1.0 seconds\nProcess running with session ID {id}\nOutput:\n")
    }

    /// The end codex records of command `cmd`, as `id`, started and over
    /// at those times.
    fn over(id: &str, cmd: &str, started: &str, ended: &str) -> String {
        let ms = |at| crate::util::iso_ms(at).unwrap();
        line(
            ended,
            json!({"type": "item_completed", "item": {"type": "CommandExecution",
                "process_id": id, "command": ["/usr/bin/zsh", "-lc", cmd]},
                "started_at_ms": ms(started), "completed_at_ms": ms(ended)}),
        )
    }

    #[test]
    fn a_terminals_id_given_again_is_another_command() {
        let dir = tempfile::tempdir().unwrap();
        // Started, over, and its id given to the next started.
        let lines = [
            exec("c1", "2026-01-01T10:00:01.000Z", "cargo build"),
            output("c1", "2026-01-01T10:00:02.000Z", &runs(7001)),
            over(
                "7001",
                "cargo build",
                "2026-01-01T10:00:01.100Z",
                "2026-01-01T10:00:05.000Z",
            ),
            exec("c2", "2026-01-01T10:00:08.000Z", "npm run dev"),
            output("c2", "2026-01-01T10:00:09.000Z", &runs(7001)),
            // Polled, running still: the same, its call's.
            poll("c3", "2026-01-01T10:00:10.000Z", 7001),
            output("c3", "2026-01-01T10:00:11.000Z", &runs(7001)),
        ];
        let path = turn(dir.path(), &lines);
        let left = left(&path, None, &nothing);
        assert_eq!(
            left.iter()
                .map(|l| (l.id.as_str(), l.what.as_deref(), l.call.as_deref()))
                .collect::<Vec<_>>(),
            [("7001", Some("npm run dev"), Some("c2"))]
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(left[0].from, text.find(&lines[4]).unwrap() as u64);
        // A poll's, over after it: over, though started long before.
        let mut lines = lines.to_vec();
        lines.push(over(
            "7001",
            "npm run dev",
            "2026-01-01T10:00:08.100Z",
            "2026-01-01T10:00:12.000Z",
        ));
        assert!(super::left(&turn(dir.path(), &lines), None, &nothing).is_empty());
        // Only polled in it, started in a turn before: an end before the
        // poll is another's.
        let lines = [
            over(
                "7001",
                "cargo build",
                "2026-01-01T09:00:00.000Z",
                "2026-01-01T09:30:00.000Z",
            ),
            poll("c3", "2026-01-01T10:00:10.000Z", 7001),
            output("c3", "2026-01-01T10:00:11.000Z", &runs(7001)),
        ];
        let left = super::left(&turn(dir.path(), &lines), None, &nothing);
        assert_eq!(
            left.iter()
                .map(|l| (l.id.as_str(), l.what.as_deref(), l.call.as_deref()))
                .collect::<Vec<_>>(),
            [("7001", None, None)]
        );
    }

    #[test]
    fn a_command_cut_short_as_codex_waits_on_it_runs_on_once_seen_running() {
        let dir = tempfile::tempdir().unwrap();
        let cut = "aborted by user after 12.0s";
        // The same run to its end before: not this one.
        let mut lines = vec![
            exec("c0", "2026-01-01T10:00:01.000Z", "sleep 30"),
            over(
                "5001",
                "sleep 30",
                "2026-01-01T10:00:01.100Z",
                "2026-01-01T10:00:31.100Z",
            ),
            output(
                "c0",
                "2026-01-01T10:00:31.200Z",
                "Wall time: 30.0 seconds\nProcess exited with code 0\nOutput:\n",
            ),
            exec("c1", "2026-01-01T10:00:40.000Z", "sleep 30"),
            output("c1", "2026-01-01T10:00:52.000Z", cut),
            // Cut short as it polled: that one's known by its id, if at all.
            poll("c2", "2026-01-01T10:00:41.000Z", 6001),
            output("c2", "2026-01-01T10:00:52.000Z", cut),
        ];
        let path = turn(dir.path(), &lines);
        let at = crate::util::iso_ms;
        let text = std::fs::read_to_string(&path).unwrap();
        // Seen running, started between its call and Esc.
        let asked = std::cell::RefCell::new(Vec::new());
        let one = |cmd: &str, _: Option<&str>, from: Option<i64>, to: Option<i64>| {
            asked.borrow_mut().push((cmd.to_string(), from, to));
            match cmd == "sleep 30" {
                true => vec![process(101, at("2026-01-01T10:00:40.100Z").unwrap(), None)],
                false => Vec::new(),
            }
        };
        let found = Backgrounded {
            id: "cut-c1".into(),
            what: Some("sleep 30".into()),
            started: at("2026-01-01T10:00:52.000Z"),
            ran: 0,
            from: text.find(&lines[4]).unwrap() as u64,
            call: Some("c1".into()),
            called: at("2026-01-01T10:00:40.000Z"),
            ends_as: vec![by_command("sleep 30")],
        };
        assert_eq!(left(&path, None, &one), std::slice::from_ref(&found));
        assert_eq!(
            asked.borrow()[..],
            [(
                "sleep 30".to_string(),
                at("2026-01-01T10:00:39.000Z"),
                at("2026-01-01T10:00:53.000Z")
            )]
        );
        // Not seen running (an approval it waited on, cancelled): none.
        assert!(left(&path, None, &nothing).is_empty());
        // Where its command was run, for a script's path relative to it:
        // the turn's directory, or the one the call names.
        let dirs = std::cell::RefCell::new(Vec::new());
        let note = |_: &str, dir: Option<&str>, _: Option<i64>, _: Option<i64>| {
            dirs.borrow_mut().push(dir.map(String::from));
            Vec::new()
        };
        let context = json!({"type": "turn_context", "payload": {"cwd": "/work/app"}}).to_string();
        let in_dir = line(
            "2026-01-01T10:00:41.000Z",
            json!({"type": "function_call", "call_id": "c3", "name": "exec_command",
                "arguments": json!({"cmd": "./run", "workdir": "/work/app/sub"}).to_string()}),
        );
        let cut_at = output("c3", "2026-01-01T10:00:52.000Z", cut);
        let with_dirs = [context, lines[3].clone(), lines[4].clone(), in_dir, cut_at];
        left(&turn(dir.path(), &with_dirs), None, &note);
        assert_eq!(
            dirs.borrow()[..],
            [
                Some("/work/app".to_string()),
                Some("/work/app/sub".to_string())
            ]
        );
        // Its end, by what it ran, and when it started.
        let end = over(
            "7001",
            "sleep 30",
            "2026-01-01T10:00:40.100Z",
            "2026-01-01T10:01:10.100Z",
        );
        let mark = std::fs::metadata(&path).unwrap().len();
        std::fs::write(&path, format!("{text}{end}\n")).unwrap();
        let (ends, _) = ended(&path, mark).unwrap();
        let ids: Vec<(&str, Option<i64>, &[String])> = (ends.iter())
            .map(|e| (e.id.as_str(), e.started, e.also.as_slice()))
            .collect();
        let started = at("2026-01-01T10:00:40.100Z");
        let what = [found.ends_as[0].clone()];
        assert_eq!(ids, [("7001", started, what.as_slice())]);
        // Its process naming the id codex gave the command: by that.
        let named = |_: &str, _: Option<&str>, _: Option<i64>, _: Option<i64>| {
            vec![process(
                101,
                at("2026-01-01T10:00:40.100Z").unwrap(),
                Some("exec-1"),
            )]
        };
        let by_its_id = left(&turn(dir.path(), &lines), None, &named);
        assert_eq!(
            by_its_id[0].ends_as,
            ["exec-1".to_string(), by_command("sleep 30")]
        );
        // Over in the turn: not left.
        lines.push(end);
        assert!(left(&turn(dir.path(), &lines), None, &one).is_empty());
        // Two the same, both cut short: each a process of its own, started
        // since its call, the first started for the first called.
        let lines = [
            exec("c1", "2026-01-01T10:00:01.000Z", "sleep 30"),
            exec("c2", "2026-01-01T10:00:10.000Z", "sleep 30"),
            output("c1", "2026-01-01T10:00:12.000Z", cut),
            output("c2", "2026-01-01T10:00:12.000Z", cut),
        ];
        let path = turn(dir.path(), &lines);
        let ms = |t: &str| at(t).unwrap();
        let both = vec![
            process(201, ms("2026-01-01T10:00:01.100Z"), None),
            process(202, ms("2026-01-01T10:00:10.100Z"), None),
        ];
        let first = vec![both[0].clone()];
        let from = |procs: Vec<crate::procs::SessionCommand>| {
            move |cmd: &str, dir: Option<&str>, from, to| {
                crate::procs::running_in(&procs, cmd, dir, from, to)
            }
        };
        let ids = |left: Vec<Backgrounded>| left.into_iter().map(|l| l.id).collect::<Vec<_>>();
        assert_eq!(
            ids(left(&path, None, &from(both.clone()))),
            ["cut-c1", "cut-c2"]
        );
        // The later one's not running, if only the earlier one is.
        assert_eq!(ids(left(&path, None, &from(first))), ["cut-c1"]);
        // A command's several processes (codex's sandbox, its shell, the
        // command) are one, by the id codex gave it.
        let with = |pid, at: &str, call| process(pid, ms(at), Some(call));
        let one_each = vec![
            with(301, "2026-01-01T10:00:01.050Z", "exec-a"),
            with(302, "2026-01-01T10:00:01.060Z", "exec-a"),
            with(303, "2026-01-01T10:00:10.050Z", "exec-b"),
        ];
        let keys = |left: Vec<Backgrounded>| {
            (left.into_iter())
                .map(|l| l.ends_as[0].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(left(&path, None, &from(one_each))),
            ["exec-a", "exec-b"]
        );
        let two = from(both);
        // In a thread whose ends codex doesn't record: none.
        let legacy = std::fs::read_to_string(&path)
            .unwrap()
            .replacen("paginated", "legacy", 1);
        std::fs::write(&path, legacy).unwrap();
        assert!(left(&path, None, &two).is_empty());
    }

    #[test]
    fn an_end_is_one_commands_its_id_given_again_however_soon() {
        let dir = tempfile::tempdir().unwrap();
        // Within a second of the first's end, by another call.
        let lines = [
            exec("c1", "2026-01-01T10:00:01.000Z", "true"),
            output("c1", "2026-01-01T10:00:01.300Z", &runs(7001)),
            over(
                "7001",
                "true",
                "2026-01-01T10:00:01.050Z",
                "2026-01-01T10:00:01.400Z",
            ),
            exec("c2", "2026-01-01T10:00:01.500Z", "sleep 30"),
            output("c2", "2026-01-01T10:00:02.500Z", &runs(7001)),
        ];
        let left = left(&turn(dir.path(), &lines), None, &nothing);
        assert_eq!(
            left.iter()
                .map(|l| (l.id.as_str(), l.call.as_deref()))
                .collect::<Vec<_>>(),
            [("7001", Some("c2"))]
        );
        // In one script: started, over, its id given to the next it starts,
        // both said running in what it printed.
        let lines = code_mode(
            "c1",
            "2026-01-01T10:00:01.000Z",
            "const a = await tools.exec_command({cmd:'sleep 2', yield_time_ms:1000}); await tools.exec_command({cmd:'sleep 4'}); const b = await tools.exec_command({cmd:'sleep 30', yield_time_ms:1000}); text(JSON.stringify([a, b]))",
            json!([{"wall_time_seconds": 1.0, "session_id": 7001, "output": ""},
                {"wall_time_seconds": 1.0, "session_id": 7001, "output": ""}]),
        );
        let mut lines = lines.to_vec();
        lines.insert(
            1,
            over(
                "7001",
                "sleep 2",
                "2026-01-01T10:00:01.100Z",
                "2026-01-01T10:00:03.100Z",
            ),
        );
        let left = super::left(&turn(dir.path(), &lines), None, &nothing);
        assert_eq!(
            left.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["7001"]
        );
    }

    #[test]
    fn a_scripts_command_is_read_in_any_of_its_quotes() {
        let script = |s: &str| json!({"type": "custom_tool_call", "name": "exec", "input": s});
        // By its keys, not by what its strings hold.
        let call = script(
            "text(await tools.exec_command({cmd:'./workdir-waiter', env:{workdir:'/no'}, 'workdir': \"/work/sub\"}))",
        );
        assert_eq!(
            exec_argument(&call, "workdir").as_deref(),
            Some("/work/sub")
        );
        assert_eq!(command(&call).as_deref(), Some("./workdir-waiter"));
        assert_eq!(
            command(&script(
                r#"await tools.exec_command({"cmd": "npm run dev", "tty": true})"#
            )),
            Some("npm run dev".into())
        );
        assert_eq!(
            command(&script(
                "tools.exec_command({ cmd: `make watch`, tty: true })"
            )),
            Some("make watch".into())
        );
        assert_eq!(
            command(&script(r"tools.exec_command({cmd:'echo \'hi\''})")),
            Some("echo 'hi'".into())
        );
        // A variable's value isn't known.
        assert_eq!(command(&script("tools.exec_command({cmd})")), None);
        assert_eq!(command(&script("tools.exec_command({cmd: c})")), None);
        // Nor what more is added to a literal, or put in a template.
        assert_eq!(command(&script("tools.exec_command({cmd: 'x' + y})")), None);
        assert_eq!(
            command(&script("tools.exec_command({cmd: `ls ${dir}`})")),
            None
        );
        // Comments are no code: neither a property nor a call in one counts,
        // nor hides one.
        let commented = |s: &str| command(&script(s));
        assert_eq!(
            commented(
                "text(await tools.exec_command({\n  // Wait for the worker.\n  cmd: 'sleep 30', yield_time_ms: 60000\n}));"
            ),
            Some("sleep 30".into())
        );
        assert_eq!(
            commented(
                "text(await tools.exec_command({\n  // Previous value, cmd: 'sleep 30',\n  cmd: 'touch denied', yield_time_ms: 60000\n}));"
            ),
            Some("touch denied".into())
        );
        assert_eq!(
            commented("tools.exec_command({/* cmd: 'a', */ cmd /* it */ : 'b' /* , */})"),
            Some("b".into())
        );
        assert_eq!(
            commented("// tools.exec_command({cmd: 'old'})\ntools.exec_command({cmd: 'new'})"),
            Some("new".into())
        );
        let polls =
            script("// not tools.write_stdin({session_id: 1})\ntools.exec_command({cmd: 'x'})");
        assert!(starts(&polls));
        assert!(!starts(&script(
            "/* tools.exec_command({cmd: 'x'}) */ tools.write_stdin({})"
        )));
        assert!(!starts(&script("text('tools.exec_command(')")));
        // Nor what a string, a template or a regular expression holds.
        assert_eq!(
            commented(
                r#"const q = /['"{]/g; tools.exec_command({cmd: `a ${"}"} b`.length ? 'x' : 'y', workdir: '/w'})"#
            ),
            None
        );
        let call = script(
            r#"const q = /['"{]/g, t = `${`${"}"}`}`; text(await tools.exec_command({cmd: "sleep 1", workdir: '/w'}))"#,
        );
        assert_eq!(command(&call).as_deref(), Some("sleep 1"));
        assert_eq!(exec_argument(&call, "workdir").as_deref(), Some("/w"));
        // The last of a key counts, unless what's added after may change it.
        assert_eq!(
            commented("tools.exec_command({cmd: 'a', cmd: 'b'})"),
            Some("b".into())
        );
        assert_eq!(commented("tools.exec_command({cmd: 'a', ...more})"), None);
        assert_eq!(
            commented("tools.exec_command({...more, cmd: 'a'})"),
            Some("a".into())
        );
        assert_eq!(commented("tools.exec_command({cmd: 'a', [k]: 'b'})"), None);
        assert_eq!(commented("tools.exec_command(options)"), None);
        // A key however spelt, or anything else that may set it.
        let then =
            |more: &str| commented(&format!("tools.exec_command({{cmd: 'sleep 30', {more}}})"));
        assert_eq!(
            then(r"'\x63md': 'touch denied'"),
            Some("touch denied".into())
        );
        for more in [
            r"\u0063md: 'touch denied'",
            "get cmd() { return 'touch denied'; }",
            "cmd() {}",
            "async cmd() {}",
            "*cmd() {}",
        ] {
            assert_eq!(then(more), None, "{more}");
        }
        for more in ["'workdir'() {}", "1: 'one'", "workdir,", "tty: true,"] {
            assert_eq!(then(more), Some("sleep 30".into()), "{more}");
        }
        // A `/` as JavaScript takes it: a division after a value, a regular
        // expression where one starts.
        for before in [
            "let n = 1; const ratio = n++ / 2;",
            "let n = 1; const ratio = n-- / 2 / 1;",
            "const o = {}; const r = o.return / 2;",
            "const é = 4; const r = é / 2;",
            "if (true) /'/.test('a');",
            "function f(x) { return /'/.test(x); }",
            "const t = typeof /'/;",
            "const r =\u{a0}/'/;",
            "// a comment, its line ended by a CR\r",
            "// a comment, its line ended by U+2028\u{2028}",
        ] {
            let call = script(&format!(
                "{before} text(await tools.exec_command({{cmd:'sleep 30'}}));"
            ));
            assert!(starts(&call), "{before}");
            assert_eq!(command(&call).as_deref(), Some("sleep 30"), "{before}");
        }
        // A statement's words after a property's name are words again; `of`
        // is one in a loop's head, else a name; `for await` is a loop.
        for before in [
            "const o = {x: 'hello'};\nfunction f() {\n  const line = o.x\n  return /write_stdin(.*)/.test(line);\n}\nf();",
            "const of = 4; const ratio = of / 2;",
            "for (const x of /a/.exec('a')) /write_stdin(.*)/.test(x);",
            "for await (const x of ['x']) /write_stdin(.*)/.test(x);",
        ] {
            let call = script(&format!(
                "{before}\ntext(await tools.exec_command({{cmd:'sleep 30'}}));"
            ));
            assert!(starts(&call), "{before}");
            assert_eq!(command(&call).as_deref(), Some("sleep 30"), "{before}");
        }
        // However the call is spaced.
        for call in [
            "tools.exec_command ({cmd: 'a'})",
            "tools . exec_command/* it */({cmd: 'a',})",
            "tools.exec_command(\u{a0}{\u{2028}cmd\u{a0}:\u{a0}'a'\u{a0}})",
        ] {
            assert_eq!(commented(call), Some("a".into()), "{call}");
        }
        assert_eq!(commented("tools.my_exec_command({cmd: 'a'})"), None);
        // A regular expression ends at its line's end however long the line:
        // one long line of many is read at once, not once per each.
        let dense = format!(
            "const x = [{}0]; tools.exec_command({{cmd: 'a'}})",
            "/a/,".repeat(200_000)
        );
        let at = std::time::Instant::now();
        assert_eq!(commented(&dense).as_deref(), Some("a"));
        assert!(
            at.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            at.elapsed()
        );
    }

    #[test]
    fn a_string_literal_is_read_as_javascript_reads_it() {
        // As strict Node 24 reads them.
        assert_eq!(
            js_string(r"'a\\b\t\x41\u0042\u{43}\'\q'").as_deref(),
            Some("a\\b\tABC'q")
        );
        assert_eq!(
            js_string(r#""it\'s \"q\" \/ \u{1F600} \uD83D\uDE00""#).as_deref(),
            Some("it's \"q\" / 😀 😀")
        );
        assert_eq!(
            js_string("`a\r\nb\\\nc \\${x}`").as_deref(),
            Some("a\nbc ${x}")
        );
        // An octal escape strict code refuses, a half of a character alone,
        // or one not closed.
        for literal in [r"'\101'", r"'\8'", r"'\uD83D'", "'a"] {
            assert_eq!(js_string(literal), None, "{literal}");
        }
    }
}
