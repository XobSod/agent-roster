//! What's underneath: roster's tmux server and database, checked against each
//! other. `roster doctor` prints it; the TUI's server view (`v t`) shows it.

use std::collections::HashMap;

use anyhow::Result;

use crate::engine::Roster;
use crate::model::{Session, State};
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
    pub text: String,
}

pub struct Diagnosis {
    pub lines: Vec<Line>,
    /// tmux sessions on roster's socket that no live session owns.
    pub orphans: Vec<String>,
    pub problems: usize,
}

/// Variables that mean the server was started from inside tmux or Claude Code:
/// the telling few of `tmux::LEAKY_ENV`, which roster keeps out of it.
const LEAKY: &[&str] = &["TMUX", "TMUX_PANE", "CLAUDECODE"];

struct Report {
    lines: Vec<Line>,
    problems: Vec<String>,
}

impl Report {
    fn push(&mut self, level: Level, text: impl Into<String>) {
        self.lines.push(Line {
            level,
            text: text.into(),
        });
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

impl Roster {
    pub fn diagnose(&mut self) -> Result<Diagnosis> {
        self.refresh()?;
        let now = now_ms();
        let mut r = Report {
            lines: Vec::new(),
            problems: Vec::new(),
        };

        // The server.
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
                    "the server's environment has {}; it was started from inside tmux or Claude Code. Stop all sessions and run `tmux -L {} kill-server`",
                    leaked.join(", "),
                    self.tmux.socket()
                ));
            }
        }

        // Its sessions, matched with roster's.
        let panes = self.tmux.pane_details()?;
        let sessions = self.db.sessions(true)?;
        let by_tmux: HashMap<String, &Session> =
            sessions.iter().map(|s| (s.tmux_name(), s)).collect();
        let mut orphans = Vec::new();
        r.heading(&format!("tmux sessions ({})", panes.len()));
        if panes.is_empty() {
            r.push(Level::Info, "  none");
        }
        let label_width = sessions
            .iter()
            .map(|s| s.label().chars().count())
            .max()
            .unwrap_or(0)
            .max(7);
        for p in &panes {
            let owner = by_tmux.get(&p.session).filter(|s| s.deleted_at.is_none());
            let label = match (owner, by_tmux.get(&p.session)) {
                (Some(s), _) => s.label(),
                (None, Some(s)) => format!("{} (archived)", s.label()),
                (None, None) => "-".to_string(),
            };
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
            let text = format!(
                "  {:<12}{label:<label_width$}  {pane}, {}x{}, {} lines{clients}",
                p.session, p.width, p.height, p.history
            );
            match owner {
                Some(_) if p.dead => {
                    r.push(Level::Note, format!("{text}  (stopped; last screen kept)"))
                }
                Some(_) => r.push(Level::Ok, text),
                None => {
                    r.push(Level::Problem, format!("{text}  ← orphan"));
                    orphans.push(p.session.clone());
                }
            }
        }
        for name in &orphans {
            let why = match by_tmux.get(name) {
                Some(s) => format!("belongs to archived session {}", s.label()),
                None if name.starts_with("r-") => "belongs to no roster session".to_string(),
                None => "wasn't created by roster".to_string(),
            };
            r.problem(format!("tmux session {name} {why} (orphan)"));
        }

        // Roster's sessions that tmux doesn't have.
        let missing: Vec<&Session> = sessions
            .iter()
            .filter(|s| s.deleted_at.is_none() && !panes.iter().any(|p| p.session == s.tmux_name()))
            .collect();
        if !missing.is_empty() {
            r.heading(&format!(
                "roster sessions without a tmux session ({})",
                missing.len()
            ));
            for s in missing {
                if s.state == State::Stopped {
                    r.push(
                        Level::Note,
                        format!("  {}  stopped (starting it creates one)", s.label()),
                    );
                } else {
                    r.push(
                        Level::Problem,
                        format!("  {}  recorded as {}", s.label(), s.state.as_str()),
                    );
                    r.problem(format!(
                        "{} is recorded as {} but has no tmux session",
                        s.label(),
                        s.state.as_str()
                    ));
                }
            }
        }

        // The database.
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

        // The verdict.
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
