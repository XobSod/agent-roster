//! What the CLI prints: lists, tables, a session's details, events.

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use crate::engine::Roster;
use crate::model::{Session, State};
use crate::procs::{self, Totals};
use crate::{doctor, search, stats, util};

pub fn list_adoptable(r: &Roster, all: bool) -> Result<()> {
    let found = r.adoptable(all)?;
    if found.is_empty() {
        let hint = if all {
            ""
        } else {
            " in your projects' directories (--all shows the rest)"
        };
        println!("No conversations to adopt{hint}.");
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
                (None, Some(pid)) => format!("open in {} (pid {pid})", util::capitalize(&c.agent)),
                (None, None) => String::new(),
            };
            let what = c.label();
            let id: String = c.id.chars().take(8).collect();
            vec![
                id,
                c.agent.clone(),
                util::ago(now - c.modified),
                project,
                cut(&what, 50),
                note,
            ]
        })
        .collect();
    print_table(&["ID", "AGENT", "UPDATED", "PROJECT", "TITLE", ""], &rows);
    Ok(())
}

/// Every setting: name, value, whether it's yours or the default, what it does.
pub fn settings(r: &Roster) {
    let tty = io::stdout().is_terminal();
    println!("{}", r.paths.config_file().display());
    for section in r.settings() {
        let mut title = section.title.clone();
        if let Some((_, note)) = section.agent.as_ref().filter(|(_, n)| !n.is_empty()) {
            title.push_str(&format!(" ({note})"));
        }
        println!("\n{title}");
        let width = section
            .settings
            .iter()
            .map(|s| s.name().len())
            .max()
            .unwrap_or(0);
        for s in &section.settings {
            let source = if s.set { "yours  " } else { "default" };
            let line = format!(
                "  {:<width$}  {:<22}  {source}  {}",
                s.name(),
                cut(&s.value, 22),
                s.doc
            );
            // Defaults dimmed, so what you set stands out.
            if tty && !s.set {
                println!("\x1b[2m{line}\x1b[0m");
            } else {
                println!("{line}");
            }
        }
    }
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
            "worktree",
            s.worktree.as_ref().map_or("-".into(), |w| {
                let standing = crate::engine::standing(w);
                let made = if w.made { "" } else { " (not roster's)" };
                format!(
                    "{}{made}",
                    crate::engine::standing_said(w, standing.as_ref())
                )
            }),
        ),
        (
            "conversation",
            s.conversation_id.clone().unwrap_or_else(|| "-".into()),
        ),
    ];
    for (name, value) in rows {
        println!("{name:<13}{value}");
    }
    if let Some(note) = &s.note {
        for (i, line) in note.text.lines().enumerate() {
            let name = if i == 0 { "note" } else { "" };
            let mark = match note.pin {
                Some(pin) if pin == i => "▸ ",
                Some(_) => "  ",
                None => "",
            };
            println!("{name:<13}{mark}{line}");
        }
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
    let colored = |text: &str| match tty && !color.is_empty() && !text.is_empty() {
        true => format!("\x1b[{color}m{text}\x1b[0m"),
        false => text.to_string(),
    };
    match &line.item {
        _ if line.level == Level::Heading || line.text.is_empty() => colored(&line.text),
        // A session or a process: indented by how deep it is, all unfolded;
        // a session's figures plain.
        Some(item) => {
            let (head, figures, rest) = match &line.figures {
                Some((at, figures)) => (&line.text[..*at], figures.as_str(), &line.text[*at..]),
                None => (line.text.as_str(), "", ""),
            };
            let indent = "  ".repeat(item.depth);
            format!(
                "{}{figures}{}",
                colored(&format!("{mark} {indent}{head}")),
                colored(rest)
            )
        }
        None => colored(&format!("{mark}{}", &line.text[1..])),
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
            let mut defaults = p.defaults_label(r.config.default_agent(), &models);
            if p.origin.join(crate::paths::PROJECT_ENV).exists() {
                defaults.push_str(" · env file");
            }
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

/// What each agent's account has used of its limits, asked now: what's left
/// of each window, whether it's used slower or faster than its time goes,
/// and when it starts over. An error if an agent's couldn't be read.
pub fn print_usage(r: &Roster, json: bool) -> Result<()> {
    use crate::usage::Pace;
    if !r.config.account_limits() {
        anyhow::bail!("{}", crate::usage::OFF);
    }
    let said = r.usage_reader()();
    let now = util::now_ms();
    let failed = said.iter().filter(|(_, u)| u.is_err()).count();
    if json {
        let items: Vec<_> = said
            .iter()
            .map(|(agent, usage)| match usage {
                Ok(u) => json!({
                    "agent": agent, "plan": u.plan, "at": u.at, "notes": u.notes,
                    "windows": u.windows.iter().map(|w| json!({
                        "label": w.label(), "minutes": w.minutes, "used": w.used, "left": w.left(),
                        "resets_at": w.resets_at, "started_over": w.over(now),
                        "pace": w.pace(now).map(|p| match p {
                            Pace::Slow => "slow", Pace::Even => "even", Pace::Fast => "fast", Pace::Out => "out",
                        }),
                    })).collect::<Vec<_>>(),
                }),
                Err(e) => json!({"agent": agent, "error": format!("{e:#}")}),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if said.is_empty() {
        println!(
            "No agent here says what its account has used: Claude and codex do, logged in to a subscription."
        );
    } else {
        for (i, (agent, usage)) in said.iter().enumerate() {
            if i > 0 {
                println!();
            }
            let u = match usage {
                Ok(u) => u,
                Err(e) => {
                    println!("{agent}  can't tell: {e:#}");
                    continue;
                }
            };
            match &u.plan {
                Some(plan) => println!("{agent}  {plan}"),
                None => println!("{agent}"),
            }
            let label_w = u.windows.iter().map(|w| w.label().len()).max().unwrap_or(0);
            let paced = u.windows.iter().any(|w| w.pace(now).is_some());
            for w in &u.windows {
                let left = if w.over(now) {
                    "–".into()
                } else {
                    w.percent()
                };
                let mut line = format!("  {:label_w$}  {left:>4} left", w.label());
                match w.pace(now) {
                    Some(pace) => line.push_str(&format!("  {} {:7}", pace.mark(), pace.word())),
                    None if paced => line.push_str(&" ".repeat(11)),
                    None => {}
                }
                if let Some(resets) = w.resets(now) {
                    line.push_str(&format!("  {resets}"));
                }
                println!("{line}");
            }
            for note in &u.notes {
                println!("  {note}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} agent(s) couldn't say");
    }
    Ok(())
}

/// What the agents spent in the last `days`, from every conversation they
/// keep on this machine and what roster kept of those gone: tokens of each
/// kind, and their cost at the API's prices, in all, by agent and by model.
/// The files changed since are read first.
pub fn print_tokens(r: &Roster, days: i64, json: bool) -> Result<()> {
    use crate::tokens::{Report, Scan, Spent, amount, dollars};
    Scan::default().update(&r.db, &crate::tokens::Prices::new(&r.config.prices))?;
    let now = util::now_ms();
    let from = now - days * 86_400_000;
    let report = Report::of(&r.db.spent_hours(from, now)?, from, now, 0);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let span = if days == 1 {
        "the last 24h".to_string()
    } else {
        format!("the last {days} days")
    };
    let total = &report.total;
    if total.tokens.total() == 0 {
        println!("Nothing spent in {span}.");
        return Ok(());
    }
    let cost = |s: &Spent| {
        if s.unpriced() {
            "unpriced".to_string()
        } else {
            dollars(s.cost)
        }
    };
    println!(
        "In {span}: {} tokens in {} session{}, {} at API prices (the cache saved {})",
        amount(total.tokens.total()),
        report.sessions,
        if report.sessions == 1 { "" } else { "s" },
        dollars(total.cost),
        dollars(total.saved)
    );
    let t = &total.tokens;
    println!(
        "input {} · cache read {} · cache write {} · output {}",
        amount(t.input),
        amount(t.cache_read),
        amount(t.cache_write),
        amount(t.output)
    );
    if total.unpriced > 0 {
        println!(
            "{} tokens are of models without a known price, not in the cost.",
            amount(total.unpriced)
        );
    }
    println!();
    let share = |s: &Spent| {
        format!(
            "{:.1}%",
            s.tokens.total() as f64 * 100.0 / t.total().max(1) as f64
        )
    };
    let rows: Vec<Vec<String>> = report
        .agents
        .iter()
        .map(|a| {
            vec![
                a.agent.clone(),
                a.sessions.to_string(),
                amount(a.spent.tokens.total()),
                share(&a.spent),
                cost(&a.spent),
            ]
        })
        .collect();
    print_table(&["AGENT", "SESSIONS", "TOKENS", "SHARE", "COST"], &rows);
    println!();
    let rows: Vec<Vec<String>> = report
        .models
        .iter()
        .map(|m| {
            let t = &m.spent.tokens;
            vec![
                m.model.clone(),
                m.agent.clone(),
                amount(t.input),
                amount(t.cache_read),
                amount(t.cache_write),
                amount(t.output),
                cost(&m.spent),
            ]
        })
        .collect();
    print_table(
        &[
            "MODEL",
            "AGENT",
            "INPUT",
            "CACHE READ",
            "CACHE WRITE",
            "OUTPUT",
            "COST",
        ],
        &rows,
    );
    Ok(())
}

/// How `roster ls` lists: archived sessions or live ones, of one project or
/// all, in the shelves' order, with what their processes use (`usage`, by
/// session id), as JSON.
pub struct ListOptions<'a> {
    pub archived: bool,
    pub project: Option<&'a str>,
    pub by_attention: bool,
    pub usage: Option<&'a HashMap<String, Totals>>,
    pub json: bool,
}

pub fn list_sessions(r: &Roster, query: &str, options: &ListOptions) -> Result<()> {
    let ListOptions {
        archived,
        project,
        by_attention,
        usage,
        json,
    } = *options;
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
                let mut item = json!({
                    "id": s.id, "project": s.project, "name": s.name, "agent": s.agent,
                    "state": s.state.as_str(), "unread": s.unread, "exit_status": s.exit_status,
                    "conversation_id": s.conversation_id, "transcript_path": s.transcript_path, "workdir": s.workdir, "archived_at": s.deleted_at,
                    "title": s.title, "model": s.model, "effort": s.effort, "state_since": s.state_since,
                    "pending_prompt": s.pending_prompt.is_some(),
                    "given_prompt": s.given_prompt.is_some(),
                    "running_model": s.running_model, "running_effort": s.running_effort,
                    "title_override": s.title_override, "created_at": s.created_at,
                    "turns": s.turns, "worked_ms": s.worked(util::now_ms()),
                    "subagents": s.subagents_running(util::now_ms()),
                    "shells": s.shells.len(),
                    "note": s.note.as_ref().map(|n| &n.text),
                    "note_pin": s.note.as_ref().and_then(|n| n.pin),
                    "worktree": s.worktree.as_ref().map(|w| json!({
                        "path": w.root, "branch": w.branch, "base": w.base,
                        "made_by_roster": w.made, "set_up": w.set_up,
                    })),
                });
                // Asked for: null for a session with no processes.
                if let Some(usage) = usage {
                    item["resources"] = json!(usage.get(&s.id).map(|t| json!({
                        "memory": t.memory, "cpu": t.cpu, "processes": t.processes,
                        "heaviest": {"command": t.heaviest.0, "memory": t.heaviest.1},
                    })));
                }
                item
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
                (State::Stopped, Some(code)) if code != 0 => format!(
                    "stopped {} (exit {code})",
                    util::ago(now - s.state_since.unwrap_or(s.created_at))
                ),
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
            let title = s.title().unwrap_or("-");
            let title = match s.note {
                Some(_) => format!("✎ {title}"),
                None => title.to_string(),
            };
            let agent = [Some(s.agent.clone()), s.model_label(), s.effort_label()]
                .into_iter()
                .flatten();
            let mut row = vec![
                s.project.clone(),
                s.name.clone(),
                format!("{marker} {state}"),
                cut(&title, 40),
                agent.collect::<Vec<_>>().join(" · "),
                s.id.clone(),
            ];
            if let Some(usage) = usage {
                let totals = usage.get(&s.id);
                let cpu = totals.and_then(|t| t.cpu).map(|cpu| format!("{cpu:.0}%"));
                let memory = totals.map(|t| procs::size(t.memory));
                row.insert(3, cpu.unwrap_or_else(|| "-".into()));
                row.insert(4, memory.unwrap_or_else(|| "-".into()));
            }
            row
        })
        .collect();
    let mut header = vec!["PROJECT", "SESSION", "  STATE", "TITLE", "AGENT", "ID"];
    if usage.is_some() {
        header.splice(3..3, ["CPU", "MEMORY"]);
    }
    print_table(&header, &rows);
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
    // Each session's label and agent, by id.
    let mut known: HashMap<String, (String, String)> = HashMap::new();
    let mut last = r.db.event_id_before_last(20, filter.as_deref())?;
    loop {
        for e in r.db.events_after(last, filter.as_deref(), 1000)? {
            if !known.contains_key(&e.session_id) {
                known = r
                    .sessions(true)?
                    .into_iter()
                    .map(|s| (s.id.clone(), (s.label(), s.agent)))
                    .collect();
                // Gone (deleted, say): its id, without looking again.
                known
                    .entry(e.session_id.clone())
                    .or_insert_with(|| (e.session_id.clone(), String::new()));
            }
            let (label, agent) = &known[&e.session_id];
            println!(
                "{}  {label}  {}  {}",
                util::local_time(e.at),
                e.kind,
                summarize(agent, e.payload.as_deref())
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

/// The interesting parts of a report's payload: what `agent`'s adapter reads
/// of it, and the hook and tool Claude names.
pub fn summarize(agent: &str, payload: Option<&str>) -> String {
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
    let report = crate::agent::adapter(agent).report(&v);
    if let Some(id) = &report.conversation_id {
        parts.push(format!("conversation={}", id.get(..8).unwrap_or(id)));
    }
    if report.compacted {
        parts.push("compacted".into());
    }
    if report.cleared {
        parts.push("cleared".into());
    }
    if let Some(agent) = &report.subagent {
        parts.push(format!("subagent={}", agent.get(..8).unwrap_or(agent)));
    }
    if !report.shells.is_empty() {
        let ids: Vec<&str> = report.shells.iter().map(|s| s.id.as_str()).collect();
        parts.push(format!("shells={}", ids.join(",")));
    }
    if !report.shells_ended.is_empty() {
        parts.push(format!("shells-ended={}", report.shells_ended.join(",")));
    }
    if report.woken {
        parts.push("woken".into());
    }
    for (key, value) in [("model", &report.model), ("effort", &report.effort)] {
        if let Some(value) = value {
            parts.push(format!("{key}={value}"));
        }
    }
    match &report.title {
        Some(Some(title)) => parts.push(format!("title={title:?}")),
        Some(None) => parts.push("untitled".into()),
        None => {}
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_summarized_in_their_agents_terms() {
        let claude = r#"{"hook_event_name":"SessionStart","source":"clear","session_id":"c0ffee123456","model":"claude-opus-5-5"}"#;
        assert_eq!(
            summarize("claude", Some(claude)),
            "SessionStart source=clear conversation=c0ffee12 cleared model=claude-opus-5-5"
        );
        assert_eq!(
            summarize("claude", Some(r#"{"source":"compact"}"#)),
            "source=compact compacted"
        );
        let pi = r#"{"conversation_id":"01a16240-5d71","title":"Fix the parser","effort":"high"}"#;
        assert_eq!(
            summarize("pi", Some(pi)),
            r#"conversation=01a16240 effort=high title="Fix the parser""#
        );
        assert_eq!(summarize("pi", Some(r#"{"title":null}"#)), "untitled");
        let subagent = r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","session_id":"c0ffee123456","agent_id":"019a0000-0000-7000","model":"gpt-6-luna"}"#;
        assert_eq!(
            summarize("codex", Some(subagent)),
            "PostToolUse tool_name=Bash subagent=019a0000"
        );
        assert_eq!(summarize("aider", Some("not json")), "not json");
    }
}
