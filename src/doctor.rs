//! What's underneath: roster's tmux server and database, checked against each
//! other, and what each session's processes use. `roster doctor` prints it;
//! the TUI's server view (`v t`) shows it, its sessions folding to their
//! processes.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::engine::Roster;
use crate::model::{Session, State};
use crate::procs;
use crate::util::{self, now_ms};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Heading,
    Info,
    Ok,
    /// Worth knowing, but expected (e.g. a stopped session's dead pane).
    Note,
    Problem,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub level: Level,
    /// For a line of an `item`, without its indent.
    pub text: String,
    /// What a session's processes use, shown plainly whatever the level,
    /// after the first so many bytes of `text` (what names the session).
    pub figures: Option<(usize, String)>,
    pub item: Option<Item>,
}

/// What a line is about, for the server view to select, fold and act on:
/// a session, or one of its processes, the tree under it in the lines after.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub what: What,
    /// 0 for a session, 1 for its agent (or a process that left it), and
    /// one more for each level under.
    pub depth: usize,
    /// Lines of its own under it.
    pub children: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum What {
    /// A roster session (its id).
    Session(String),
    /// A process of the session, by pid and when it started.
    Process {
        session: String,
        pid: i32,
        started: u64,
    },
}

pub struct Diagnosis {
    pub lines: Vec<Line>,
    /// tmux sessions on roster's socket that no live session owns.
    pub orphans: Vec<String>,
    pub problems: usize,
}

/// Variables that mean the server was started from inside tmux or an agent:
/// the telling few of `tmux::LEAKY_ENV`, which roster keeps out of it.
const LEAKY: &[&str] = &["TMUX", "TMUX_PANE", "CLAUDECODE", "CODEX_SESSION_ID"];

struct Report {
    lines: Vec<Line>,
    problems: Vec<String>,
}

impl Report {
    fn push(&mut self, level: Level, text: impl Into<String>) {
        self.lines.push(Line {
            level,
            text: text.into(),
            figures: None,
            item: None,
        });
    }

    /// A session's line, `head` (what names it), what its processes use
    /// together, then `rest`; and its processes' lines after it.
    fn session(
        &mut self,
        level: Level,
        head: String,
        rest: String,
        id: &str,
        snapshot: Option<&procs::Snapshot>,
        tree: &[(&procs::Process, usize)],
    ) {
        let figures = (snapshot.and_then(|s| s.totals(tree))).map(|t| (head.len(), totals(&t)));
        self.lines.push(Line {
            level,
            text: head + &rest,
            figures,
            item: Some(Item {
                what: What::Session(id.to_string()),
                depth: 0,
                children: !tree.is_empty(),
            }),
        });
        // Each process's figures lined up, however deep it is (each level
        // indented two more).
        let width = (tree.iter())
            .map(|(p, depth)| 2 * depth + p.command.chars().count())
            .max()
            .unwrap_or(0);
        for (i, (p, depth)) in tree.iter().enumerate() {
            let cpu = snapshot.and_then(|s| s.cpu(p));
            let width = width - 2 * depth;
            self.push(
                Level::Info,
                format!(
                    "{:<width$}  {:>8}  CPU {:>5}  pid {}",
                    p.command,
                    procs::size(p.rss),
                    percent(cpu),
                    p.pid
                ),
            );
            let line = self.lines.last_mut().expect("just pushed");
            line.item = Some(Item {
                what: What::Process {
                    session: id.to_string(),
                    pid: p.pid,
                    started: p.started,
                },
                depth: depth + 1,
                children: tree.get(i + 1).is_some_and(|(_, next)| next > depth),
            });
        }
    }

    fn heading(&mut self, text: &str) {
        if !self.lines.is_empty() {
            self.push(Level::Info, "");
        }
        self.push(Level::Heading, text);
    }

    fn field(&mut self, level: Level, name: &str, value: impl AsRef<str>) {
        self.push(level, format!("  {name:<12}{}", value.as_ref()));
    }

    fn problem(&mut self, text: impl Into<String>) {
        self.problems.push(text.into());
    }
}

use util::short_path as short;

/// A CPU share as a percentage; `-` until known (between two readings).
fn percent(cpu: Option<f64>) -> String {
    cpu.map_or("-".to_string(), |cpu| format!("{cpu:.0}%"))
}

/// What a session's processes use together, for its line: memory, CPU,
/// how many, in columns.
fn totals(t: &procs::Totals) -> String {
    let count = match t.processes {
        1 => "1 process".to_string(),
        n => format!("{n} processes"),
    };
    format!(
        "{:>8}  CPU {:>5}  {count:<13}  ",
        procs::size(t.memory),
        percent(t.cpu)
    )
}

impl Roster {
    pub fn diagnose(&mut self) -> Result<Diagnosis> {
        self.refresh()?;
        let now = now_ms();
        let mut r = Report {
            lines: Vec::new(),
            problems: Vec::new(),
        };

        r.heading("tmux server");
        let server = self.tmux.server()?;
        let socket = server
            .as_ref()
            .map_or(String::new(), |s| format!(" ({})", s.socket_path));
        r.field(
            Level::Info,
            "socket",
            format!("{}{socket}", self.tmux.socket()),
        );
        match &server {
            Some(s) => {
                let up = util::clock((now / 1000 - s.started) * 1000);
                r.field(
                    Level::Ok,
                    "server",
                    format!("running, pid {}, tmux {}, up {up}", s.pid, s.version),
                );
            }
            None => r.field(
                Level::Note,
                "server",
                "not running (it starts with the first session)",
            ),
        }
        r.field(
            Level::Info,
            "config",
            format!("{} (generated)", short(self.tmux.conf_path())),
        );
        let user_conf = self.paths.user_tmux_conf();
        let present = if user_conf.exists() {
            "present"
        } else {
            "none: every key but the back key goes to the agent"
        };
        r.field(
            Level::Info,
            "your file",
            format!("{} ({present})", short(&user_conf)),
        );
        let detach = self.config.keys.detach.clone();
        match self.tmux.detach_bound(&detach)? {
            Some(true) => r.field(Level::Ok, "back key", format!("{detach}, bound")),
            Some(false) => {
                r.field(Level::Problem, "back key", format!("{detach}, NOT bound"));
                r.problem(format!(
                    "the back key {detach} isn't bound; the server didn't load roster's config"
                ));
            }
            None => {}
        }
        if let Some(env) = self.tmux.global_env()? {
            let leaked: Vec<&str> = LEAKY
                .iter()
                .copied()
                .filter(|v| env.iter().any(|e| e == v))
                .collect();
            if leaked.is_empty() {
                r.field(
                    Level::Ok,
                    "environment",
                    format!("clean (no {})", LEAKY.join(", ")),
                );
            } else {
                r.field(
                    Level::Problem,
                    "environment",
                    format!("has {}", leaked.join(", ")),
                );
                r.problem(format!(
                    "the server's environment has {}; it was started from inside tmux, Claude Code or codex. Stop all sessions and run `tmux -L {} kill-server`",
                    leaked.join(", "),
                    self.tmux.socket()
                ));
            }
        }

        let panes = self.tmux.pane_details()?;
        let sessions = self.db.sessions(true)?;
        let by_tmux: HashMap<String, &Session> =
            sessions.iter().map(|s| (s.tmux_name(), s)).collect();
        let mut orphans = Vec::new();
        // What each session's processes use; CPU only between readings (the
        // server view's), not in one.
        let agents: Vec<i32> = (panes.iter())
            .filter(|p| !p.dead)
            .filter_map(|p| p.pid)
            .collect();
        let snapshot = self.processes(&agents);
        let snap = snapshot.as_ref().ok();
        let tree = |id: &str, root: Option<i32>| match snap {
            Some(snapshot) => snapshot.of_session(id, root),
            None => Vec::new(),
        };
        // The sessions shown with their processes.
        let mut shown = HashSet::new();
        r.heading(&format!("tmux sessions ({})", panes.len()));
        if panes.is_empty() {
            r.push(Level::Info, "  none");
        }
        let pane_label = |name: &str| match by_tmux.get(name) {
            Some(s) if s.deleted_at.is_none() => s.label(),
            Some(s) => format!("{} (archived)", s.label()),
            None => "-".to_string(),
        };
        // Each section's labels lined up, their figures with them.
        let width = |labels: &mut dyn Iterator<Item = String>| {
            labels.map(|l| l.chars().count()).max().unwrap_or(0).max(7)
        };
        let label_width = width(&mut panes.iter().map(|p| pane_label(&p.session)));
        for p in &panes {
            let owner = by_tmux.get(&p.session).filter(|s| s.deleted_at.is_none());
            let label = pane_label(&p.session);
            let pane = if p.dead {
                match (p.dead_status, p.dead_signal) {
                    (Some(code), _) => format!("dead (exit {code})"),
                    (None, Some(signal)) => format!("dead (killed by signal {signal})"),
                    (None, None) => "dead".to_string(),
                }
            } else {
                format!(
                    "alive, pid {} {}",
                    p.pid.map_or("?".to_string(), |pid| pid.to_string()),
                    p.command
                )
            };
            let clients = match p.clients {
                0 => String::new(),
                1 => ", 1 client".to_string(),
                n => format!(", {n} clients"),
            };
            let head = format!("{:<12}{label:<label_width$}  ", p.session);
            let rest = format!(
                "{pane}, {}x{}, {} lines{clients}",
                p.width, p.height, p.history
            );
            match (owner, by_tmux.get(&p.session)) {
                (Some(s), _) if p.dead => {
                    let rest = format!("{rest}  (stopped; last screen kept)");
                    r.session(Level::Note, head, rest, &s.id, snap, &tree(&s.id, None));
                    shown.insert(s.id.clone());
                }
                (Some(s), _) => {
                    let processes = tree(&s.id, p.pid);
                    r.session(Level::Ok, head, rest, &s.id, snap, &processes);
                    shown.insert(s.id.clone());
                }
                // An archived session's, with what may still run in it.
                (None, Some(s)) => {
                    let rest = format!("{rest}  ← orphan");
                    let processes = tree(&s.id, p.pid.filter(|_| !p.dead));
                    r.session(Level::Problem, head, rest, &s.id, snap, &processes);
                    shown.insert(s.id.clone());
                    orphans.push(p.session.clone());
                }
                (None, None) => {
                    r.push(Level::Problem, format!("  {head}{rest}  ← orphan"));
                    orphans.push(p.session.clone());
                }
            }
        }
        if let Err(e) = &snapshot {
            r.push(
                Level::Note,
                format!("  processes: couldn't read them: {e:#}"),
            );
        }
        for name in &orphans {
            let why = match by_tmux.get(name) {
                Some(s) => format!("belongs to archived session {}", s.label()),
                None if name.starts_with("r-") => "belongs to no roster session".to_string(),
                None => "wasn't created by roster".to_string(),
            };
            r.problem(format!("tmux session {name} {why} (orphan)"));
        }

        let missing: Vec<&Session> = sessions
            .iter()
            .filter(|s| s.deleted_at.is_none() && !panes.iter().any(|p| p.session == s.tmux_name()))
            .collect();
        if !missing.is_empty() {
            r.heading(&format!(
                "roster sessions without a tmux session ({})",
                missing.len()
            ));
            let label_width = width(&mut missing.iter().map(|s| s.label()));
            for s in missing {
                // What outlived its agent, if anything.
                let processes = tree(&s.id, None);
                let head = format!("{:<label_width$}  ", s.label());
                shown.insert(s.id.clone());
                if s.state == State::Stopped {
                    let rest = "stopped (starting it creates one)".to_string();
                    r.session(Level::Note, head, rest, &s.id, snap, &processes);
                } else {
                    let rest = format!("recorded as {}", s.state.as_str());
                    r.session(Level::Problem, head, rest, &s.id, snap, &processes);
                    r.problem(format!(
                        "{} is recorded as {} but has no tmux session",
                        s.label(),
                        s.state.as_str()
                    ));
                }
            }
        }

        // What archived or deleted sessions left running: started in them,
        // outliving their tmux session.
        let mut left: Vec<String> = (snap.into_iter())
            .flat_map(|snap| snap.processes.iter())
            .filter_map(|p| p.session.clone())
            .filter(|id| !shown.contains(id))
            .collect();
        left.sort();
        left.dedup();
        if !left.is_empty() {
            r.heading(&format!(
                "left running by archived or deleted sessions ({})",
                left.len()
            ));
            let label = |id: &str| match sessions.iter().find(|s| s.id == id) {
                Some(s) => format!("{} (archived)", s.label()),
                None => format!("{id} (deleted)"),
            };
            let label_width = width(&mut left.iter().map(|id| label(id)));
            for id in left {
                let head = format!("{:<label_width$}  ", label(&id));
                r.session(
                    Level::Note,
                    head,
                    String::new(),
                    &id,
                    snap,
                    &tree(&id, None),
                );
            }
        }

        r.heading("database");
        r.field(
            Level::Info,
            "file",
            format!(
                "{} (schema {})",
                short(&self.paths.db()),
                self.db.schema_version()?
            ),
        );
        let (waiting, kept) = self.db.event_counts()?;
        let level = if waiting > 0 {
            Level::Note
        } else {
            Level::Info
        };
        r.field(level, "events", format!("{waiting} waiting, {kept} kept"));
        let prompts: Vec<String> = sessions
            .iter()
            .filter(|s| s.deleted_at.is_none() && s.pending_prompt.is_some())
            .map(Session::label)
            .collect();
        if prompts.is_empty() {
            r.field(Level::Info, "prompts", "none waiting");
        } else {
            r.field(
                Level::Note,
                "prompts",
                format!("waiting for the agent: {}", prompts.join(", ")),
            );
        }

        let problems = r.problems.len();
        r.heading(&format!("problems ({problems})"));
        if problems == 0 {
            r.push(Level::Ok, "  none");
        }
        for p in std::mem::take(&mut r.problems) {
            r.push(Level::Problem, format!("  {p}"));
        }
        if !orphans.is_empty() {
            r.push(
                Level::Info,
                "  orphans can be removed: `roster doctor --fix`, or Enter in the server view",
            );
        }
        Ok(Diagnosis {
            lines: r.lines,
            orphans,
            problems,
        })
    }

    /// Kills the orphaned tmux sessions the diagnosis found. Returns how many.
    pub fn kill_orphans(&mut self) -> Result<usize> {
        let orphans = self.diagnose()?.orphans;
        for name in &orphans {
            self.tmux.kill_session(name)?;
        }
        Ok(orphans.len())
    }
}
