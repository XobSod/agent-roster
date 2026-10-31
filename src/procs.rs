//! What the agents' processes use, read from `/proc`: each process's parent,
//! memory and CPU time, and the roster session it belongs to. Each inherits
//! `ROSTER_SESSION` (and `ROSTER_DB`, telling which roster's), so one that
//! left its agent (`nohup`, a build daemon) is still found, as is one that
//! outlived it: a leak is as likely in what the agent ran as in the agent
//! itself.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result};

/// A process, as `/proc` says it.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub pid: i32,
    pub ppid: i32,
    /// When it started (clock ticks since boot): with the pid, which process
    /// it is, pids being reused.
    pub started: u64,
    /// Resident memory, in bytes.
    pub rss: u64,
    /// CPU time it has used, in ms.
    pub cpu_ms: u64,
    pub command: String,
    /// The session it was started in (`ROSTER_SESSION`) of the roster that
    /// reads it, if its environment can be read (only one's own processes').
    pub session: Option<String>,
    /// A session's agent, whose tree is its session's.
    pub agent: bool,
}

impl Process {
    fn key(&self) -> (i32, u64) {
        (self.pid, self.started)
    }
}

/// Every process running now; which session of the roster keeping its
/// database at `db` each was started in, read only for one's own processes
/// outside the trees of `roots` (the agents', whose processes are theirs
/// anyway), and only once a process (`known`, by pid and start, its
/// environment set as it started): reading a process's environment waits on
/// its memory, which a process thrashing holds up.
pub fn processes(
    roots: &[i32],
    db: &Path,
    known: &mut HashMap<(i32, u64), Option<String>>,
) -> Result<Vec<Process>> {
    // SAFETY: sysconf only reads system constants.
    let (ticks, page) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let (ticks, page) = (ticks.max(1) as u64, page.max(1) as u64);
    // SAFETY: getuid can't fail.
    let me = unsafe { libc::getuid() };
    let mut all = Vec::new();
    let mut own = HashSet::new();
    for entry in fs::read_dir("/proc")
        .context("couldn't read /proc")?
        .flatten()
    {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        // Gone since it was listed: left out.
        let Ok(stat) = fs::read(entry.path().join("stat")) else {
            continue;
        };
        let Some(p) = parse_stat(pid, &String::from_utf8_lossy(&stat), ticks, page) else {
            continue;
        };
        if entry.metadata().is_ok_and(|m| m.uid() == me) {
            own.insert(pid);
        }
        all.push(p);
    }
    let agents: HashSet<i32> = (roots.iter())
        .flat_map(|&root| descendants(&all, root))
        .collect();
    // This roster's database however its path is spelled (a home reached
    // through a symlink, say): the same file.
    let file = |path: &Path| fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
    let ours = file(db);
    let mut same_db: HashMap<Vec<u8>, bool> = HashMap::new();
    for p in &mut all {
        p.agent = roots.contains(&p.pid);
        if !own.contains(&p.pid) || agents.contains(&p.pid) {
            continue;
        }
        if let Some(session) = known.get(&p.key()) {
            p.session = session.clone();
            continue;
        }
        let env = fs::read(format!("/proc/{}/environ", p.pid)).ok();
        p.session = env
            .as_deref()
            .and_then(session_in)
            .and_then(|(id, its_db)| {
                let same = *same_db.entry(its_db.to_vec()).or_insert_with(|| {
                    its_db == db.as_os_str().as_bytes()
                        || ours
                            .is_some_and(|f| file(Path::new(OsStr::from_bytes(its_db))) == Some(f))
                });
                same.then_some(id)
            });
        known.insert(p.key(), p.session.clone());
    }
    let alive: HashSet<(i32, u64)> = all.iter().map(Process::key).collect();
    known.retain(|key, _| alive.contains(key));
    Ok(all)
}

/// Who the process `pid` (of this process's pid namespace) is, as `alive`
/// reads it back: its pid and start, so another process given the pid later
/// isn't taken for it, and the namespace.
pub fn identity(pid: u32) -> Option<String> {
    let start = started(pid.try_into().ok()?)?;
    Some(format!(
        "{pid}:{start}:{}",
        pid_namespace().unwrap_or_default()
    ))
}

/// Whether the process an `identity` names still runs; seen from another pid
/// namespace (a sandbox's), where its pid isn't its own, taken for running.
pub fn alive(identity: &str) -> bool {
    let [pid, start, namespace] = identity.split(':').collect::<Vec<_>>()[..] else {
        return false;
    };
    if namespace != pid_namespace().unwrap_or_default() {
        return true;
    }
    pid.parse()
        .ok()
        .and_then(started)
        .is_some_and(|at| start.parse() == Ok(at))
}

fn pid_namespace() -> Option<String> {
    let link = fs::read_link("/proc/self/ns/pid").ok()?;
    let inode = link.to_str()?.strip_prefix("pid:[")?.strip_suffix(']')?;
    Some(inode.to_string())
}

/// When the process `pid` started, while it runs.
fn started(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(parse_stat(pid, &stat, 1, 1)?.started)
}

/// The command lines of the processes of roster session `session` of the
/// roster keeping its database at `db` (by the `ROSTER_SESSION` and
/// `ROSTER_DB` they inherited), each with its pid and when it started (ms):
/// the most
/// recent `MAX_COMMANDS`, none longer than `MAX_COMMAND` (bytes). For an
/// agent that doesn't say whether a command it was waiting on runs on
/// (codex's, cut short by Esc): none if `/proc` can't be read.
pub fn session_commands(session: &str, db: &Path) -> Vec<SessionCommand> {
    // SAFETY: sysconf only reads a system constant; getuid can't fail.
    let (ticks, me) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK).max(1) as i64,
            libc::getuid(),
        )
    };
    let (Some(boot), Ok(entries)) = (boot_ms(), fs::read_dir("/proc")) else {
        return Vec::new();
    };
    let file = |path: &Path| fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
    let ours = file(db);
    let same_db = |its: &[u8]| {
        its == db.as_os_str().as_bytes()
            || ours.is_some_and(|f| file(Path::new(OsStr::from_bytes(its))) == Some(f))
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if !entry.metadata().is_ok_and(|m| m.uid() == me) {
            continue;
        }
        let Some(start) = started(pid) else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if cmdline.is_empty() || cmdline.len() > MAX_COMMAND {
            continue;
        }
        let env = fs::read(entry.path().join("environ")).unwrap_or_default();
        if !session_in(&env).is_some_and(|(id, its_db)| id == session && same_db(its_db)) {
            continue;
        }
        let argv = (cmdline.split(|b| *b == 0))
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let call = (env.split(|b| *b == 0))
            .find_map(|var| var.strip_prefix(b"CODEX_TOOL_CALL_ID="))
            .filter(|id| !id.is_empty())
            .map(|id| String::from_utf8_lossy(id).into_owned());
        let cwd = (fs::read_link(entry.path().join("cwd")).ok())
            .map(|dir| dir.to_string_lossy().into_owned());
        found.push(SessionCommand {
            pid,
            started: boot + start as i64 * 1000 / ticks,
            argv,
            call,
            cwd,
        });
    }
    found.sort_by_key(|c| std::cmp::Reverse(c.started));
    found.truncate(MAX_COMMANDS);
    found
}

/// One of a session's processes, as `session_commands` reads it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionCommand {
    pub pid: i32,
    /// When it started (ms).
    pub started: i64,
    pub argv: Vec<String>,
    /// The id its agent gave the command it runs, where its environment
    /// says (codex's `CODEX_TOOL_CALL_ID`, which its end names too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// Where it runs: what a relative path in its command line is relative
    /// to (`./script`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Those of `commands` that started between `from` and `to` (ms, as far as
/// given) running `cmd` (`runs`), the first started first.
pub fn running_in(
    commands: &[SessionCommand],
    cmd: &str,
    dir: Option<&str>,
    from: Option<i64>,
    to: Option<i64>,
) -> Vec<SessionCommand> {
    let mut found: Vec<SessionCommand> = (commands.iter())
        .filter(|c| {
            from.is_none_or(|f| c.started >= f)
                && to.is_none_or(|t| c.started <= t)
                && runs(&c.argv, &[c.cwd.as_deref(), dir], cmd)
        })
        .cloned()
        .collect();
    found.sort_by_key(|c| c.started);
    found
}

/// The session's processes, read now, running `cmd`, started between
/// `from` and `to` (`session_commands`, `running_in`).
pub fn running(
    session: &str,
    db: &Path,
    cmd: &str,
    dir: Option<&str>,
    from: Option<i64>,
    to: Option<i64>,
) -> Vec<SessionCommand> {
    running_in(&session_commands(session, db), cmd, dir, from, to)
}

/// How many of a session's processes `session_commands` keeps: the most
/// recent, a payload holding them.
const MAX_COMMANDS: usize = 64;
/// The longest command line it keeps (bytes).
const MAX_COMMAND: usize = 2048;

/// Whether a process's command line runs `cmd`: as a shell's
/// (`sh -c <cmd>`), or as the command itself once the shell gave way to it
/// (a script's interpreter, if any, before it): its words as
/// `util::one_line` has them, or as the shell splits a simple command.
fn runs(argv: &[String], dirs: &[Option<&str>], cmd: &str) -> bool {
    use crate::util::one_line;
    if argv.len() >= 3
        && argv[argv.len() - 2].starts_with('-')
        && argv[argv.len() - 2].contains('c')
        && one_line(&argv[argv.len() - 1]) == cmd
    {
        return true;
    }
    let name = |word: &str| word.rsplit('/').next().unwrap_or(word).to_string();
    // Where the command's words start: at its first, or, a script run by its
    // shebang, at its path, after the interpreter and the arguments its
    // shebang gives it (`#!/usr/bin/env -S python3 -W default`).
    let starts = |k: usize| {
        k == 0
            || (argv[k].contains('/')
                && (dirs.iter()).any(|dir| {
                    shebang(&Path::new(dir.unwrap_or("/")).join(&argv[k])).is_some_and(|line| {
                        name(&argv[0]) == name(&line[0]) && argv[1..k] == line[1..]
                    })
                }))
    };
    // As the shell splits it, a command by its path; or as written.
    let words = words(cmd).filter(|w| !w.is_empty());
    let as_written = |k: usize, first: String| {
        let rest = argv[k + 1..].iter().map(String::as_str);
        one_line(
            &std::iter::once(first.as_str())
                .chain(rest)
                .collect::<Vec<_>>()
                .join(" "),
        ) == cmd
    };
    (0..argv.len()).filter(|&k| starts(k)).any(|k| {
        words
            .as_ref()
            .is_some_and(|w| name(&argv[k]) == name(&w[0]) && argv[k + 1..] == w[1..])
            || as_written(k, argv[k].clone())
            || as_written(k, name(&argv[k]))
    })
}

/// What the script at `path` is run by, as its first line (`#!`) says: the
/// interpreter's command line it's given after, as the interpreter gets it.
/// As Linux runs it: the interpreter, and the rest of the line as one
/// argument; `env` gives way to the program it's given, its `-S` splitting
/// the rest into words. None for what isn't an executable file starting
/// so, read only if it's a plain file (a pipe or a terminal could keep a
/// read waiting).
fn shebang(path: &Path) -> Option<Vec<String>> {
    use std::io::Read;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    let mut file = (fs::File::options().read(true))
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .ok()?;
    let mut head = [0u8; 256];
    let n = file.read(&mut head).ok()?;
    let line = head[..n].strip_prefix(b"#!")?;
    let line = &line[..memchr::memchr(b'\n', line).unwrap_or(line.len())];
    let line = String::from_utf8_lossy(line);
    let line = line.trim_matches([' ', '\t']);
    let (interpreter, arg) = match line.split_once([' ', '\t']) {
        Some((interpreter, arg)) => (interpreter, Some(arg.trim_matches([' ', '\t']))),
        None => (line, None),
    };
    if interpreter.is_empty() {
        return None;
    }
    let arg = arg.filter(|a| !a.is_empty());
    if interpreter.rsplit('/').next() == Some("env") {
        let arg = arg?;
        return match arg.strip_prefix("-S") {
            Some(words) => env_split(words).filter(|w| !w.is_empty()),
            None => Some(vec![arg.to_string()]),
        };
    }
    Some(
        [interpreter]
            .into_iter()
            .chain(arg)
            .map(String::from)
            .collect(),
    )
}

/// The words `env -S` splits `line` into: at blanks (a carriage return,
/// so a CRLF line's end, among them) and `\_`, quotes (`'…'`, `"…"`) and
/// backslashes taken off as it takes them, the rest of it from a word
/// starting `#` or from `\c` left out. None for what it
/// would expand (`${…}`), which can't be told from the line alone, and for
/// what it takes for an error (a `$` alone, an unknown `\` sequence).
fn env_split(line: &str) -> Option<Vec<String>> {
    fn part(words: &mut Vec<String>, word: &mut String, quoted: &mut bool) {
        if !word.is_empty() || *quoted {
            words.push(std::mem::take(word));
            *quoted = false;
        }
    }
    let mut words = Vec::new();
    let (mut word, mut quoted) = (String::new(), false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' => part(&mut words, &mut word, &mut quoted),
            '#' if word.is_empty() && !quoted => break,
            '$' => return None,
            // Taken as written, but for `\\` and `\'`.
            '\'' => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        '\\' => match chars.next()? {
                            c @ ('\\' | '\'') => word.push(c),
                            c => word.extend(['\\', c]),
                        },
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' => return None,
                        '\\' => word.push(env_escape(chars.next()?)?),
                        c => word.push(c),
                    }
                }
            }
            '\\' => match chars.next()? {
                // A blank, parting words.
                '_' => part(&mut words, &mut word, &mut quoted),
                'c' => break,
                c => word.push(env_escape(c)?),
            },
            c => word.push(c),
        }
    }
    part(&mut words, &mut word, &mut quoted);
    Some(words)
}

/// What `env -S` takes a backslash and `c` for, within double quotes or
/// none; None for what it doesn't (`\c` within double quotes among them).
fn env_escape(c: char) -> Option<char> {
    match c {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        '_' => Some(' '),
        '\\' | '\'' | '"' | '#' | '$' => Some(c),
        _ => None,
    }
}

/// The words of `cmd` as a POSIX shell splits a simple command, its
/// variables set before it (`A=1 cmd`) left out, quotes and backslashes
/// taken off; None for anything else (a list, a pipe, an expansion, a
/// redirection), whose shell may stay its own process.
fn words(cmd: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let (mut word, mut quoted) = (String::new(), false);
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if !word.is_empty() || quoted {
                    words.push(std::mem::take(&mut word));
                    quoted = false;
                }
            }
            '\\' => word.push(chars.next()?),
            '\'' => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '[' | '{' | '}'
            | '~' | '#' | '\n' => return None,
            c => word.push(c),
        }
    }
    if !word.is_empty() || quoted {
        words.push(word);
    }
    let assignment = |w: &String| {
        w.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    let first = words.iter().position(|w| !assignment(w))?;
    Some(words.split_off(first))
}

/// When the machine started, in ms, as `/proc/stat` says (`btime`).
fn boot_ms() -> Option<i64> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().find_map(|l| l.strip_prefix("btime "))?;
    Some(line.trim().parse::<i64>().ok()? * 1000)
}

/// `root` and every process under it.
fn descendants(all: &[Process], root: i32) -> Vec<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in all.iter().filter(|p| p.ppid != p.pid) {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut found = vec![root];
    let mut i = 0;
    while i < found.len() {
        if let Some(under) = children.get(&found[i]) {
            found.extend(under);
        }
        i += 1;
    }
    found
}

/// A process from its `/proc/<pid>/stat`: its command in parentheses (which
/// may hold any character, parentheses too), then the fields `proc(5)`
/// numbers, CPU time in clock ticks of `ticks` a second, memory in pages of
/// `page` bytes; None once it's ended, waiting to be reaped (a parent that
/// never reaps keeps it so), using nothing and not to be killed. Shown as
/// ended while threads of it run on (its first having exited), it hasn't.
fn parse_stat(pid: i32, stat: &str, ticks: u64, page: u64) -> Option<Process> {
    let command = &stat[stat.find('(')? + 1..stat.rfind(')')?];
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // From field 3 (the state) on.
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    match fields.first() {
        Some(&"X") => return None,
        Some(&"Z") if field(20).is_none_or(|threads| threads <= 1) => return None,
        _ => {}
    }
    Some(Process {
        pid,
        ppid: fields.get(1)?.parse().ok()?,
        started: field(22)?,
        rss: field(24)? * page,
        cpu_ms: (field(14)? + field(15)?) * 1000 / ticks,
        command: command.to_string(),
        session: None,
        agent: false,
    })
}

/// `ROSTER_SESSION` in an environment as `/proc/<pid>/environ` holds it, with
/// its `ROSTER_DB`, telling which roster's session it is (a test's isn't
/// this one's).
fn session_in(env: &[u8]) -> Option<(String, &[u8])> {
    let var = |name: &[u8]| (env.split(|b| *b == 0)).find_map(|var| var.strip_prefix(name));
    let id = var(b"ROSTER_SESSION=").filter(|id| !id.is_empty())?;
    Some((
        String::from_utf8_lossy(id).into_owned(),
        var(b"ROSTER_DB=")?,
    ))
}

/// The processes as read once, with the CPU each took lately.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub processes: Vec<Process>,
    /// Each process's share of a CPU since the reading before (100: one core
    /// all the time), by pid and start; none on a first reading.
    cpu: HashMap<(i32, u64), f64>,
}

/// What some processes use together.
#[derive(Clone, Debug, PartialEq)]
pub struct Totals {
    /// Resident memory, in bytes, all of it added up (pages they share
    /// counted for each).
    pub memory: u64,
    /// Their share of a CPU lately, if known.
    pub cpu: Option<f64>,
    pub processes: usize,
    /// The one using most memory, and how much.
    pub heaviest: (String, u64),
}

impl Snapshot {
    pub fn cpu(&self, p: &Process) -> Option<f64> {
        self.cpu.get(&p.key()).copied()
    }

    /// The session `id`'s processes, each before those under it, with how
    /// deep it is (0 for a root): those under its agent (`root`, if it runs),
    /// then any other carrying its id, which left it or outlived it.
    pub fn of_session(&self, id: &str, root: Option<i32>) -> Vec<(&Process, usize)> {
        let ours: HashSet<i32> = (self.processes.iter())
            .filter(|p| Some(p.pid) == root || p.session.as_deref() == Some(id))
            .map(|p| p.pid)
            .collect();
        let mut children: HashMap<i32, Vec<&Process>> = HashMap::new();
        for p in &self.processes {
            // A parent of itself (pid 0's) isn't its child.
            if p.ppid != p.pid {
                children.entry(p.ppid).or_default().push(p);
            }
        }
        let roots = (self.processes.iter())
            .filter(|p| ours.contains(&p.pid) && (Some(p.pid) == root || !ours.contains(&p.ppid)));
        let (agent, others): (Vec<&Process>, Vec<&Process>) =
            roots.partition(|p| Some(p.pid) == root);
        // Under a root, unless known to be another session's.
        let mut tree = Vec::new();
        let mut seen = HashSet::new();
        for start in agent.into_iter().chain(others) {
            let mut stack = vec![(start, 0)];
            while let Some((p, depth)) = stack.pop() {
                if !seen.insert(p.pid) {
                    continue;
                }
                tree.push((p, depth));
                // Not into another session's agent, or a process known to be
                // another session's.
                let under = (children.get(&p.pid).into_iter().flatten().rev())
                    .filter(|c| !c.agent || Some(c.pid) == root)
                    .filter(|c| c.session.as_deref().is_none_or(|s| s == id));
                stack.extend(under.map(|c| (*c, depth + 1)));
            }
        }
        tree
    }

    /// What `processes` use together; None if there are none.
    pub fn totals(&self, processes: &[(&Process, usize)]) -> Option<Totals> {
        let heaviest = processes.iter().max_by_key(|(p, _)| p.rss)?.0;
        let cpu: Vec<f64> = processes.iter().filter_map(|(p, _)| self.cpu(p)).collect();
        Some(Totals {
            memory: processes.iter().map(|(p, _)| p.rss).sum(),
            cpu: (!cpu.is_empty()).then(|| cpu.iter().sum()),
            processes: processes.len(),
            heaviest: (heaviest.command.clone(), heaviest.rss),
        })
    }
}

/// `process` (a pid and when it started) and those under it in `tree`, a
/// session's (`Snapshot::of_session`); None if it's not there (it ended).
pub fn subtree<'a>(
    tree: &[(&'a Process, usize)],
    (pid, started): (i32, u64),
) -> Option<Vec<&'a Process>> {
    let at = (tree.iter()).position(|(p, _)| p.pid == pid && p.started == started)?;
    let depth = tree[at].1;
    let under = tree[at + 1..].iter().take_while(|(_, d)| *d > depth);
    Some(
        std::iter::once(tree[at].0)
            .chain(under.map(|(p, _)| *p))
            .collect(),
    )
}

/// A process held on to, to signal it and no other, pids being reused:
/// through a pidfd, which holds on to the process its start time is then
/// checked of; without pidfds (Linux before 5.3), by its pid, its start time
/// checked each time.
#[derive(Debug)]
pub struct Held {
    process: Process,
    fd: Option<libc::c_int>,
}

impl Held {
    /// `p`, if it's still the process it was.
    pub fn new(p: &Process) -> Option<Held> {
        // SAFETY: pidfd_open takes a pid and flags, and gives a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, p.pid, 0) } as libc::c_int;
        let no_pidfd = std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS);
        if fd < 0 && !no_pidfd {
            return None;
        }
        let held = Held {
            process: p.clone(),
            fd: (fd >= 0).then_some(fd),
        };
        held.same().then_some(held)
    }

    pub fn process(&self) -> &Process {
        &self.process
    }

    /// Still the process it was.
    fn same(&self) -> bool {
        let p = &self.process;
        (fs::read(format!("/proc/{}/stat", p.pid)).ok())
            .and_then(|stat| parse_stat(p.pid, &String::from_utf8_lossy(&stat), 1, 1))
            .is_some_and(|now| now.started == p.started)
    }

    /// Sends it `sig`; whether it was sent (not to another user's process).
    pub fn signal(&self, sig: libc::c_int) -> bool {
        match self.fd {
            Some(fd) => {
                let none: *const libc::siginfo_t = std::ptr::null();
                // SAFETY: the fd is ours and open, the signal info none, the
                // flags 0.
                unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd, sig, none, 0) == 0 }
            }
            // SAFETY: kill(2) has no memory effects.
            None => self.same() && unsafe { libc::kill(self.process.pid, sig) } == 0,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(fd) = self.fd {
            // SAFETY: the fd is ours, closed once.
            unsafe { libc::close(fd) };
        }
    }
}

/// Readings shorter apart than this keep the last figures: a CPU share
/// worked out over a few ms is noise.
const MIN_INTERVAL_MS: i64 = 1000;

/// Readings longer apart than this give no figures: an average over minutes
/// (the view left and come back to) isn't what they took lately.
const MAX_INTERVAL_MS: i64 = 5000;

/// Reads processes again and again, for the CPU each took between readings:
/// what a process used over its life hides a spike now.
#[derive(Debug, Default)]
pub struct Sampler {
    /// Each process's CPU time at the reading counted from, and when.
    base: HashMap<(i32, u64), u64>,
    base_at: Option<i64>,
    /// The shares worked out then.
    last: HashMap<(i32, u64), f64>,
}

impl Sampler {
    /// `processes`, read at `now`, with each one's CPU share since the last
    /// reading at least `MIN_INTERVAL_MS` before; one started since took all
    /// its time in it.
    pub fn read(&mut self, processes: Vec<Process>, now: i64) -> Snapshot {
        if self.base_at.is_none_or(|at| now - at >= MIN_INTERVAL_MS) {
            self.last.clear();
            if let Some(at) = self.base_at.filter(|at| now - at <= MAX_INTERVAL_MS) {
                let elapsed = (now - at) as f64;
                self.last = (processes.iter())
                    .map(|p| {
                        let before = self.base.get(&p.key()).copied().unwrap_or(0);
                        let used = p.cpu_ms.saturating_sub(before) as f64;
                        (p.key(), used * 100.0 / elapsed)
                    })
                    .collect();
            }
            self.base = processes.iter().map(|p| (p.key(), p.cpu_ms)).collect();
            self.base_at = Some(now);
        }
        let cpu = (processes.iter())
            .filter_map(|p| Some((p.key(), *self.last.get(&p.key())?)))
            .collect();
        Snapshot { processes, cpu }
    }
}

/// The machine's memory, in bytes (`MemTotal` in `/proc/meminfo`), or a
/// container's, if it's given less (its cgroup's `memory.max`).
pub fn total_memory() -> Option<u64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let total = line.split_whitespace().nth(1)?.parse::<u64>().ok()? * 1024;
    let given = (fs::read_to_string("/sys/fs/cgroup/memory.max").ok())
        .and_then(|max| max.trim().parse::<u64>().ok());
    Some(given.map_or(total, |given| given.min(total)))
}

/// Bytes as a person reads them: `640 MB`, `11.8 GB`.
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb < 1000.0 {
        format!("{} MB", mb.round())
    } else {
        format!("{:.1} GB", mb / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, ppid: i32, rss_mb: u64, cpu_ms: u64, command: &str) -> Process {
        Process {
            pid,
            ppid,
            started: pid as u64,
            rss: rss_mb * 1024 * 1024,
            cpu_ms,
            command: command.into(),
            session: None,
            agent: false,
        }
    }

    #[test]
    fn a_process_is_known_by_its_pid_start_and_namespace() {
        let me = identity(std::process::id()).unwrap();
        assert!(alive(&me));
        // Its pid given to another, started later; none at all.
        let [pid, start, namespace] = me.split(':').collect::<Vec<_>>()[..] else {
            panic!("{me}");
        };
        let later = start.parse::<u64>().unwrap() + 1;
        assert!(!alive(&format!("{pid}:{later}:{namespace}")));
        assert!(!alive("1"));
        assert!(!alive(""));
        // Of another pid namespace: not to be told from here.
        assert!(alive(&format!("{pid}:{later}:1")));
        // Ended, not reaped yet: not running.
        let mut child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let its = identity(child.id()).unwrap();
        assert!(alive(&its));
        // Its input ended, it ends.
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while alive(&its) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!alive(&its));
        child.wait().unwrap();
    }

    #[test]
    fn stat_and_environ_are_read_as_proc_writes_them() {
        // A command with spaces and parentheses; times in ticks of 1/100 s,
        // memory in pages of 4 KiB.
        let stat = "4242 (node (claude) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 1234 266 0 0 20 0 11 0 98765 1234567 51200 18446744073709551615";
        let p = parse_stat(4242, stat, 100, 4096).unwrap();
        assert_eq!(
            (p.ppid, p.started, p.rss, p.cpu_ms, p.command.as_str()),
            (1, 98765, 51200 * 4096, 15_000, "node (claude) x")
        );
        assert_eq!(parse_stat(1, "1 (init) S", 100, 4096), None);
        // Ended, not reaped yet: none; but one whose first thread alone
        // ended (its 11 others on) runs on.
        let zombie = stat.replacen(") S ", ") Z ", 1);
        assert!(parse_stat(4242, &zombie, 100, 4096).is_some());
        let zombie = zombie.replacen(" 0 11 0 ", " 0 1 0 ", 1);
        assert_eq!(parse_stat(4242, &zombie, 100, 4096), None);
        let env = b"HOME=/h\0ROSTER_SESSION=3f2a\0ROSTER_DB=/state/roster.db\0";
        assert_eq!(
            session_in(env),
            Some(("3f2a".to_string(), &b"/state/roster.db"[..]))
        );
        // Of no roster known, or of none.
        assert_eq!(session_in(b"ROSTER_SESSION=3f2a\0"), None);
        assert_eq!(
            session_in(b"ROSTER_SESSION=\0ROSTER_DB=/state/roster.db\0"),
            None
        );
        // This process's own, read from /proc: its session, if it runs in
        // one, read from its environment; not under an agent's tree.
        let me = std::process::id() as i32;
        let db = std::env::var_os("ROSTER_DB").map_or("/nowhere".into(), std::path::PathBuf::from);
        let all = processes(&[], &db, &mut HashMap::new()).unwrap();
        assert!(all.iter().any(|p| p.pid == me && p.rss > 0));
        let session = (std::env::var("ROSTER_SESSION").ok())
            .filter(|s| !s.is_empty() && db != Path::new("/nowhere"));
        assert_eq!(all.iter().find(|p| p.pid == me).unwrap().session, session);
        assert!(
            processes(&[], Path::new("/nowhere"), &mut HashMap::new())
                .unwrap()
                .iter()
                .all(|p| p.session.is_none())
        );
        // Its roster's database by another path to it: the same.
        if db != Path::new("/nowhere") {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("roster.db");
            std::os::unix::fs::symlink(&db, &link).unwrap();
            let all = processes(&[], &link, &mut HashMap::new()).unwrap();
            assert_eq!(all.iter().find(|p| p.pid == me).unwrap().session, session);
        }
        // Known already: not read again; one ended, forgotten.
        let mine = all.iter().find(|p| p.pid == me).unwrap().key();
        let mut known = HashMap::from([(mine, Some("known".to_string())), ((me, 0), None)]);
        let again = processes(&[], &db, &mut known).unwrap();
        assert_eq!(
            again
                .iter()
                .find(|p| p.pid == me)
                .unwrap()
                .session
                .as_deref(),
            Some("known")
        );
        assert!(!known.contains_key(&(me, 0)));
        let under = processes(&[me], &db, &mut HashMap::new()).unwrap();
        assert_eq!(under.iter().find(|p| p.pid == me).unwrap().session, None);
    }

    #[test]
    fn a_sessions_processes_are_its_agents_tree_and_any_carrying_its_id() {
        let mut detached = process(300, 1, 50, 0, "node");
        detached.session = Some("s1".into());
        let mut other = process(400, 1, 1, 0, "sleep");
        other.session = Some("s2".into());
        // Under one that left, but started in another session, or another
        // session's agent: that one's.
        let mut elsewhere = process(301, 300, 1, 0, "roster");
        elsewhere.session = Some("s2".into());
        let mut agent = process(302, 300, 1, 0, "claude");
        agent.agent = true;
        let snapshot = Snapshot {
            processes: vec![
                process(0, 0, 0, 0, "kernel"),
                process(1, 0, 1, 0, "init"),
                process(100, 1, 300, 0, "claude"),
                process(101, 100, 3, 0, "bash"),
                process(102, 101, 9000, 0, "cargo"),
                process(103, 100, 96, 0, "tsserver"),
                detached,
                elsewhere,
                agent,
                other,
            ],
            cpu: HashMap::new(),
        };
        let tree: Vec<(i32, usize)> = (snapshot.of_session("s1", Some(100)).iter())
            .map(|(p, depth)| (p.pid, *depth))
            .collect();
        assert_eq!(tree, [(100, 0), (101, 1), (102, 2), (103, 1), (300, 0)]);
        // Its agent gone: what outlived it.
        let left: Vec<i32> = (snapshot.of_session("s1", None).iter())
            .map(|(p, _)| p.pid)
            .collect();
        assert_eq!(left, [300]);
        let totals = snapshot
            .totals(&snapshot.of_session("s1", Some(100)))
            .unwrap();
        assert_eq!((totals.memory, totals.processes), (9449 * 1024 * 1024, 5));
        assert_eq!(totals.heaviest, ("cargo".to_string(), 9000 * 1024 * 1024));
        assert_eq!(totals.cpu, None);
        assert_eq!(snapshot.totals(&[]), None);
    }

    #[test]
    fn the_cpu_share_is_each_processs_own_since_a_second_before() {
        let mut sampler = Sampler::default();
        let mut read = |procs: Vec<Process>, at| {
            let s = sampler.read(procs, at);
            let shares: Vec<Option<f64>> = s.processes.iter().map(|p| s.cpu(p)).collect();
            shares
        };
        assert_eq!(read(vec![process(1, 0, 1, 5_000, "rustc")], 0), [None]);
        // A second later: the first rustc ended (what it used since, gone
        // with it), and another took 0.9 s since it started.
        assert_eq!(
            read(vec![process(2, 0, 1, 900, "rustc")], 1_000),
            [Some(90.0)]
        );
        // Read again 50 ms later: the same figures, not noise.
        assert_eq!(
            read(vec![process(2, 0, 1, 950, "rustc")], 1_050),
            [Some(90.0)]
        );
        // Its pid reused by another, started since: that one's own time.
        let mut reused = process(2, 0, 1, 300, "cc");
        reused.started = 99;
        assert_eq!(read(vec![reused.clone()], 2_000), [Some(30.0)]);
        // Read again after minutes: no figure, an average over them not
        // being what it took lately; then again a second on.
        reused.cpu_ms = 60_000;
        assert_eq!(read(vec![reused.clone()], 200_000), [None]);
        reused.cpu_ms = 60_500;
        assert_eq!(read(vec![reused], 201_000), [Some(50.0)]);
    }

    #[test]
    fn a_process_is_taken_with_those_under_it() {
        let procs = [
            process(100, 1, 1, 0, "claude"),
            process(101, 100, 1, 0, "bash"),
            process(102, 101, 1, 0, "cargo"),
            process(103, 102, 1, 0, "rustc"),
            process(104, 100, 1, 0, "tsserver"),
            process(300, 1, 1, 0, "node"),
        ];
        let tree: Vec<(&Process, usize)> = [0, 1, 2, 3, 1, 0]
            .iter()
            .zip(&procs)
            .map(|(d, p)| (p, *d))
            .collect();
        let pids = |found: Option<Vec<&Process>>| {
            found.map(|f| f.iter().map(|p| p.pid).collect::<Vec<_>>())
        };
        assert_eq!(pids(subtree(&tree, (101, 101))), Some(vec![101, 102, 103]));
        assert_eq!(pids(subtree(&tree, (104, 104))), Some(vec![104]));
        assert_eq!(
            pids(subtree(&tree, (100, 100))),
            Some(vec![100, 101, 102, 103, 104])
        );
        // Another process under the same pid (started since): not it.
        assert_eq!(pids(subtree(&tree, (101, 7))), None);
    }

    #[test]
    fn a_signal_goes_to_the_process_it_was_only() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let all = processes(&[], Path::new("/nowhere"), &mut HashMap::new()).unwrap();
        let p = all.iter().find(|p| p.pid == pid).unwrap().clone();
        // Taken for another started at another time: not held.
        let mut other = p.clone();
        other.started += 1;
        assert!(Held::new(&other).is_none());
        // Held, it's signalled, then gone: nothing more to send to.
        let held = Held::new(&p).unwrap();
        assert!(held.signal(libc::SIGSTOP));
        assert!(held.signal(libc::SIGKILL));
        assert!(!child.wait().unwrap().success());
        assert!(!held.signal(libc::SIGKILL));
        assert!(Held::new(&p).is_none());
    }

    #[test]
    fn a_command_is_found_running_in_its_session_as_started_then() {
        let now = crate::util::now_ms();
        let db = Path::new("/nowhere/roster.db");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30; true"])
            .env("ROSTER_SESSION", "test-running")
            .env("ROSTER_DB", db)
            .env("CODEX_TOOL_CALL_ID", "exec-procs")
            .spawn()
            .unwrap();
        let found = |session: &str, cmd: &str, from: i64, to: i64| {
            running(session, db, cmd, None, Some(now + from), Some(now + to)).len()
        };
        // Its start is read to the clock's tick.
        let mut n = 0;
        for _ in 0..50 {
            n = found("test-running", "sleep 30; true", -2_000, 5_000);
            if n == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(n, 1);
        // With the id its agent gave the command, as its environment says.
        let read = running("test-running", db, "sleep 30; true", None, None, None);
        assert_eq!(read[0].call.as_deref(), Some("exec-procs"));
        // Another session's, another command, or started at another time:
        // not it.
        assert_eq!(found("other", "sleep 30; true", -2_000, 5_000), 0);
        let elsewhere = Path::new("/elsewhere/roster.db");
        assert_eq!(
            running(
                "test-running",
                elsewhere,
                "sleep 30; true",
                None,
                None,
                None
            )
            .len(),
            0
        );
        assert_eq!(found("test-running", "sleep 31", -2_000, 5_000), 0);
        assert_eq!(found("test-running", "sleep 30; true", -60_000, -30_000), 0);
        child.kill().unwrap();
        child.wait().unwrap();
        // The command itself, the shell gone, or a script by its
        // interpreter.
        let argv = |line: &str| line.split('|').map(String::from).collect::<Vec<_>>();
        assert!(runs(&argv("sleep|30"), &[None], "sleep 30"));
        assert!(runs(
            &argv("/usr/bin/zsh|-lc|sleep 30; echo done"),
            &[None],
            "sleep 30; echo done"
        ));
        assert!(!runs(&argv("vim|notes"), &[None], "sleep 30"));
        // A script, run by its shebang: after what that gives its
        // interpreter, and only there.
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, shebang: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join(name);
            fs::write(&path, format!("{shebang}\nbody\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path.to_str().unwrap().to_string()
        };
        let npm = script("npm", "#!/usr/bin/env node");
        let waiter = script("waiter", "#!/usr/bin/env -S python3 -W default");
        let tool = script("tool", "#!/usr/bin/python3 -u");
        let two = script("two", "#!/usr/bin/python3 -W default");
        let monitor = script("monitor.py", "#!/usr/bin/env python3");
        let quoted = script(
            "quoted",
            "#!/usr/bin/env -S python3 -W 'ignore:message with spaces:UserWarning'",
        );
        let filter = "ignore:message with spaces:UserWarning";
        assert!(runs(
            &["python3".into(), "-W".into(), filter.into(), quoted.clone()],
            &[None],
            "./quoted"
        ));
        let crlf = script("crlf", "#!/usr/bin/env -S python3 -u\r");
        assert!(runs(
            &["python3".into(), "-u".into(), crlf.clone()],
            &[None],
            "./crlf"
        ));
        // As env -S splits: quotes and backslashes off, a comment left out,
        // what it would expand not guessed at.
        // As GNU env 9.4 has it.
        let split = |line: &str| env_split(line).map(|w| w.join("|"));
        for (line, words) in [
            (r#"a "b \"c\"" d\_e #f g"#, Some(r#"a|b "c"|d|e"#)),
            (r"a\_\_b \_c", Some("a|b|c")),
            (r#""a\_b\#c" a#b \#c"#, Some("a b#c|a#b|#c")),
            (
                r"'a\\b' 'it\'s' 'x\ny' 'a\cb' '$x' '${HOME}'",
                Some(r"a\b|it's|x\ny|a\cb|$x|${HOME}"),
            ),
            (r#"a'b'c"d"e "" x"#, Some("abcde||x")),
            (r#""a" b\c d"#, Some("a|b")),
            (r"a\$b a\tb", Some("a$b|a\tb")),
            ("python3 ${OPTS}", None),
            ("a$b", None),
            (r#""$x""#, None),
            (r#""a\cb""#, None),
            (r"a\qb", None),
            (r"a\ b", None),
            (r"x\", None),
            ("'a", None),
            // A line ending CRLF, and blanks as env takes them.
            ("python3 -u\r", Some("python3|-u")),
            ("a\x0bb\x0cc", Some("a|b|c")),
            ("'a\rb' \"c\rd\"", Some("a\rb|c\rd")),
        ] {
            assert_eq!(split(line).as_deref(), words, "{line}");
        }
        let run = |line: String, cmd: &str| runs(&argv(&line), &[None], cmd);
        // Relative to where it runs.
        let here = dir.path().to_str().unwrap();
        assert!(runs(
            &argv("python3|-W|default|./waiter"),
            &[Some(here)],
            "./waiter"
        ));
        assert!(!runs(
            &argv("python3|-W|default|./waiter"),
            &[Some("/")],
            "./waiter"
        ));
        // Moved elsewhere since it started: by where its command was run.
        assert!(runs(
            &argv("python3|-W|default|./waiter"),
            &[Some("/"), Some(here)],
            "./waiter"
        ));
        assert!(run(format!("node|{npm}|run|dev"), "npm run dev"));
        assert!(run(format!("python3|-W|default|{waiter}"), "./waiter"));
        assert!(run(format!("/usr/bin/python3|-u|{tool}|x"), "tool x"));
        // The rest of a shebang's line one argument, as Linux gives it.
        assert!(run(format!("/usr/bin/python3|-W default|{two}"), "./two"));
        assert!(!run(format!("/usr/bin/python3|-W|default|{two}"), "./two"));
        // Not a plain executable file: not read, nor waited on (a pipe no
        // one writes to).
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(!run(format!("cat|{}", fifo.display()), "sleep 30"));
        let plain = dir.path().join("plain");
        fs::write(&plain, "#!/usr/bin/env node\n").unwrap();
        assert!(!run(format!("node|{}", plain.display()), "plain"));
        // Not what the shebang gives it, nor a script's own arguments.
        assert!(!run(format!("python3|-W|error|{waiter}"), "./waiter"));
        assert!(!run(
            format!("python3|{monitor}|/usr/bin/sleep|30"),
            "/usr/bin/sleep 30"
        ));
        assert!(!run(format!("node|{npm}|run|dev"), "dev"));
        // As the shell splits it, quoted, or after its variables.
        assert!(runs(&argv("sleep|30"), &[None], r#"sleep "30""#));
        assert!(runs(
            &argv("/bin/echo|a b|it's"),
            &[None],
            r#"echo 'a b' it\'s"#
        ));
        assert!(runs(&argv("sleep|30"), &[None], "TZ=UTC A=1 sleep 30"));
        // Its words from the start, or a script's after its interpreter:
        // not a command's argument that happens to be named alike.
        assert!(!runs(&argv("cat|ls"), &[None], "ls"));
        assert!(!runs(&argv("vim|/x/ls"), &[None], "ls"));
        assert!(!runs(&argv("watch|sleep|30"), &[None], "sleep 30"));
        assert!(!runs(&argv("python3|tool.py|/x/ls"), &[None], "ls"));
        assert!(!runs(&argv("sleep|30"), &[None], "sleep 30 | cat"));
        assert!(!runs(&argv("sleep|30"), &[None], r#"sleep "$N""#));
    }

    #[test]
    fn the_machines_memory_is_read() {
        assert!(total_memory().is_some_and(|bytes| bytes > 64 * 1024 * 1024));
    }

    #[test]
    fn sizes_read_as_megabytes_then_gigabytes() {
        assert_eq!(size(640 * 1024 * 1024), "640 MB");
        assert_eq!(size(12_390 * 1024 * 1024), "12.1 GB");
    }
}
