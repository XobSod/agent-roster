//! What the CLI prints: lists, tables, a session's details, events.

use std::collections::HashMap;
use std::io::{self, Write};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use crate::engine::Roster;
use crate::model::{Session, State};
use crate::{doctor, search, stats, util};

pub fn list_adoptable(r: &Roster, all: bool) -> Result<()> {
    let found = r.adoptable(all)?;
    if found.is_empty() {
        let hint = if all {
            ""
        } else {
            " in your projects' directories (--all shows the rest)"
        };
        println!("No Claude conversations to adopt{hint}.");
        return Ok(());
    }
    let now = util::now_ms();
    let rows: Vec<Vec<String>> = found
        .iter()
        .map(|a| {
            let c = &a.conversation;
            let project = match &a.project {
                Some(p) => p.name.clone(),
                None => format!("new: {}", a.new_project),
            };
            let note = match (&a.archived, c.open_in) {
                (Some(s), _) => format!("archived in roster as {}", s.label()),
                (None, Some(pid)) => format!("open in Claude (pid {pid})"),
                (None, None) => String::new(),
            };
            let what = c.label();
            let id: String = c.id.chars().take(8).collect();
            vec![
                id,
                util::ago(now - c.modified),
                project,
                cut(&what, 50),
                note,
            ]
        })
        .collect();
    print_table(&["ID", "UPDATED", "PROJECT", "TITLE", ""], &rows);
    Ok(())
}

pub fn print_info(r: &Roster, s: &Session) {
    let now = util::now_ms();
    let title = match (&s.title_override, &s.title) {
        (Some(t), _) => format!("{t} (yours)"),
        (None, Some(t)) => format!("{t} ({}'s)", s.agent),
        (None, None) => "-".to_string(),
    };
    let agent = [Some(s.agent.clone()), s.model_label(), s.effort_label()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let state = match s.deleted_at {
        Some(t) => format!("archived {} ago", util::ago(now - t)),
        None => format!(
            "{} {}",
            s.state.as_str(),
            util::ago(now - s.state_since.unwrap_or(s.created_at))
        ),
    };
    let rows = [
        ("session", format!("{} ({})", s.label(), s.id)),
        ("title", title),
        ("agent", agent.join(" · ")),
        ("state", state),
        ("directory", s.workdir.display().to_string()),
        (
            "conversation",
            s.conversation_id.clone().unwrap_or_else(|| "-".into()),
        ),
    ];
    for (name, value) in rows {
        println!("{name:<13}{value}");
    }
    let stats = r.agent_stats(s);
    println!(
        "{:<13}{}",
        "statistics",
        stats::summary(s, stats.as_ref(), now).join(" · ")
    );
    if stats.as_ref().is_some_and(|a| a.partial) {
        println!(
            "{:<13}≥: cost and lines are Claude's totals up to its last restart",
            ""
        );
    }
}

/// A diagnosis line for a terminal (coloured) or a pipe (plain, with markers).
pub fn doctor_line(line: &doctor::Line, tty: bool) -> String {
    use doctor::Level;
    let (color, mark) = match line.level {
        Level::Heading => ("1", ""),
        Level::Info => ("", " "),
        Level::Ok => ("32", "✓"),
        Level::Note => ("2", "·"),
        Level::Problem => ("31", "✗"),
    };
    let text = if line.level == Level::Heading || line.text.is_empty() {
        line.text.clone()
    } else {
        format!("{mark}{}", &line.text[1..])
    };
    if tty && !color.is_empty() {
        format!("\x1b[{color}m{text}\x1b[0m")
    } else {
        text
    }
}

pub fn list_projects(r: &Roster) -> Result<()> {
    let projects = r.projects()?;
    if projects.is_empty() {
        println!("No projects. Add one with `roster project add <name> <path>`.");
        return Ok(());
    }
    let sessions = r.sessions(true)?;
    let models = r.db.model_names()?;
    let chats = r.chat_project_id()?;
    let rows: Vec<Vec<String>> = projects
        .iter()
        .map(|p| {
            let mut defaults = p.defaults_label(&models);
            if chats == Some(p.id) {
                defaults.push_str(" · built-in, for chats");
            }
            vec![
                p.name.clone(),
                p.origin.display().to_string(),
                p.sessions_label(&sessions),
                defaults,
            ]
        })
        .collect();
    print_table(&["PROJECT", "DIRECTORY", "SESSIONS", "DEFAULTS"], &rows);
    Ok(())
}

pub fn list_sessions(
    r: &Roster,
    query: &str,
    archived: bool,
    project: Option<&str>,
    by_attention: bool,
    json: bool,
) -> Result<()> {
    if let Some(name) = project
        && !r.projects()?.iter().any(|p| p.name == name)
    {
        bail!("no project {name:?}");
    }
    let mut sessions: Vec<Session> = r
        .sessions(archived)?
        .into_iter()
        .filter(|s| s.deleted_at.is_some() == archived && project.is_none_or(|p| s.project == p))
        .collect();
    if let Some(hits) = search::hits(query, &sessions) {
        sessions.retain(|s| hits.contains_key(&s.id));
    }
    if by_attention {
        sessions.sort_by(|a, b| a.list_order(b, &r.config.order));
    }
    if json {
        let items: Vec<_> = sessions
            .iter()
            .map(|s| {
                json!({
                    "id": s.id, "project": s.project, "name": s.name, "agent": s.agent,
                    "state": s.state.as_str(), "unread": s.unread, "exit_status": s.exit_status,
                    "conversation_id": s.conversation_id, "transcript_path": s.transcript_path, "workdir": s.workdir, "archived_at": s.deleted_at,
                    "title": s.title, "model": s.model, "effort": s.effort, "state_since": s.state_since,
                    "pending_prompt": s.pending_prompt.is_some(),
                    "running_model": s.running_model, "running_effort": s.running_effort,
                    "title_override": s.title_override, "created_at": s.created_at,
                    "turns": s.turns, "worked_ms": s.worked(util::now_ms()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if sessions.is_empty() {
        if !query.trim().is_empty() {
            println!("No sessions match {:?}.", query.trim());
        } else if archived {
            println!("No archived sessions.");
        } else {
            println!("No sessions. Start one with `roster new <project>`.");
        }
        return Ok(());
    }
    let now = util::now_ms();
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let state = match (s.state, s.exit_status) {
                (State::Stopped, Some(code)) if code != 0 => format!("stopped (exit {code})"),
                (State::Working, _) => format!(
                    "working {}",
                    util::clock(now - s.state_since.unwrap_or(s.created_at))
                ),
                (state, _) => format!(
                    "{} {}",
                    state.as_str(),
                    util::ago(now - s.state_since.unwrap_or(s.created_at))
                ),
            };
            let marker = if s.unread { "●" } else { " " };
            let agent = [Some(s.agent.clone()), s.model_label(), s.effort_label()]
                .into_iter()
                .flatten();
            vec![
                s.project.clone(),
                s.name.clone(),
                format!("{marker} {state}"),
                cut(s.title.as_deref().unwrap_or("-"), 40),
                agent.collect::<Vec<_>>().join(" · "),
                s.id.clone(),
            ]
        })
        .collect();
    print_table(
        &["PROJECT", "SESSION", "  STATE", "TITLE", "AGENT", "ID"],
        &rows,
    );
    Ok(())
}

/// `s` cut to `max` characters, with "…" if it was longer.
pub fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars()
        .take(max - 1)
        .chain(std::iter::once('…'))
        .collect()
}

pub fn print_table(header: &[&str], rows: &[Vec<String>]) {
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

pub fn events(r: &Roster, follow: bool, session: Option<&str>) -> Result<()> {
    let filter = match session {
        Some(addr) => Some(r.resolve_any(addr)?.id),
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
pub fn summarize(payload: Option<&str>) -> String {
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
