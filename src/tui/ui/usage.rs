//! What the agents' accounts have used of their limits, and what they spent:
//! the usage view, and the sidebar's line under the session list.

use super::*;
use crate::tokens::{Counts, Period, Report, Spent, amount, dollars};
use crate::tui::limits::STALE_MS;
use crate::usage::{Pace, Usage, Window};

/// What a window's bar, what's left of it and its pace show in: its pace's
/// colour, `quiet` without one.
fn pace_color(pace: Option<Pace>, quiet: ratatui::style::Color) -> ratatui::style::Color {
    match pace {
        Some(Pace::Slow) => PACE_SLOW,
        Some(Pace::Even) => PACE_EVEN,
        Some(Pace::Fast | Pace::Out) => PACE_FAST,
        None => quiet,
    }
}

fn stale(usage: &Usage, now: i64) -> bool {
    now - usage.at > STALE_MS
}

/// A window's pace, none for one known long ago (`stale`): its use then
/// against its time now says nothing.
fn paced(window: &Window, stale: bool, now: i64) -> Option<Pace> {
    window.pace(now).filter(|_| !stale)
}

/// The sidebar's line: what's left of each agent's windows, shortest first,
/// each with its pace (`claude 69%▲ 48%◆  codex 93%▼ 87%▼`) in its colour,
/// by its mark instead of its name where names don't fit, then only each
/// one's shortest window, then fewer agents: never a number cut short. Dim if
/// old. None until an agent has said, or if not even one window fits.
pub(super) fn usage_line(app: &App, width: usize, now: i64) -> Option<Line<'static>> {
    let said: Vec<(&str, &Usage)> = app
        .limits
        .agents
        .iter()
        .filter_map(|s| Some((s.agent.as_str(), s.usage.as_ref()?)))
        .collect();
    // The first `agents` that said, each named or by its mark, with its
    // first `windows`.
    let line = |agents: usize, named: bool, windows: usize| {
        let mut spans = vec![Span::raw(" ")];
        for (i, (agent, usage)) in said.iter().take(agents).enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let dim = stale(usage, now);
            spans.push(if named {
                Span::styled(agent.to_string(), Style::new().fg(FAINT))
            } else {
                glyph(agent)
            });
            for w in usage.windows.iter().take(windows) {
                let pace = paced(w, dim, now);
                let color = if dim { FAINT } else { pace_color(pace, MUTED) };
                let left = if w.over(now) {
                    "–".into()
                } else {
                    w.percent()
                };
                let mark = pace.map_or("", Pace::mark);
                spans.push(Span::styled(
                    format!(" {left}{mark}"),
                    Style::new().fg(color),
                ));
            }
        }
        spans
    };
    let width_of = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let all = usize::MAX;
    let shorter = (1..said.len()).rev().map(|agents| line(agents, false, 1));
    [
        line(all, true, all),
        line(all, false, all),
        line(all, false, 1),
    ]
    .into_iter()
    .chain(shorter)
    .filter(|_| !said.is_empty())
    .find(|spans| width_of(spans) <= width)
    .map(Line::from)
}

/// The sidebar's last lines: the agents' one line (`usage_line`) unless the
/// CLI Proxy's accounts are known; then an entry for each account (and each
/// agent asked of its own), two to a line, each in half the width, as many
/// lines as that takes: its provider's icon if set (`cli_proxy.icons`), its
/// name (`account_short`, numbered if another has it), and what's left of
/// its windows. Where a name would get fewer than 3 columns, every entry has
/// its shortest window alone; an entry still too wide, its icon (or first
/// letter) and that window; a half too narrow for even that, an entry a
/// line. Disabled accounts, and those that haven't said, aren't there. Never
/// a figure cut short.
pub(super) fn usage_lines(app: &App, width: usize, now: i64) -> Vec<Line<'static>> {
    if !app.limits.agents.iter().any(|s| s.account.is_some()) {
        return usage_line(app, width, now).into_iter().collect();
    }
    let icons = app.cli_proxy.icons.unwrap_or(false);
    let said: Vec<(&crate::tui::limits::Said, &Usage)> = ordered(app)
        .into_iter()
        .filter(|s| !s.account.as_ref().is_some_and(|a| a.disabled))
        .filter_map(|s| Some((s, s.usage.as_ref()?)))
        .collect();
    // Each one's name; one already shown numbered (`Max#2`), by a number
    // no other name has.
    let given: Vec<String> = said
        .iter()
        .map(|(s, _)| match &s.account {
            Some(a) => {
                let label = app.cli_proxy.label(&a.provider, a.email.as_deref());
                crate::usage::account_short(a, label)
            }
            None => s.agent.clone(),
        })
        .collect();
    let mut names: Vec<String> = Vec::new();
    for name in &given {
        let taken = |n: &String| names.contains(n) || (n != name && given.contains(n));
        let name = match names.contains(name) {
            false => name.clone(),
            true => (2..)
                .map(|n| format!("{name}#{n}"))
                .find(|n| !taken(n))
                .unwrap_or_default(),
        };
        names.push(name);
    }
    let entries: Vec<_> = said.iter().zip(&names).collect();
    // Each entry in `cell` columns with its first `windows`, its name cut as
    // need be; None if it doesn't fit, or `named` and its name would get
    // fewer than 3 columns.
    let entry = |(s, u): &(&crate::tui::limits::Said, &Usage),
                 name: &str,
                 cell: usize,
                 windows: usize,
                 named: bool|
     -> Option<Vec<Span<'static>>> {
        let dim = stale(u, now);
        let figures = window_spans(u, windows, dim, now);
        let icon = icons.then(|| glyph(&s.agent));
        // A space, the icon and a space after it if there's one, the name,
        // the figures: the name gets what's left.
        let around = 1 + icon.as_ref().map_or(0, |i| i.width() + 1) + spans_width(&figures);
        let room = cell.saturating_sub(around).min(NAME_MAX);
        let whole = name.width() <= room;
        if named && !whole && room < 3 {
            return None;
        }
        let shown = match () {
            _ if whole => name.to_string(),
            _ if room >= 2 => cut_name(name, room),
            // The icon alone stands for it; else its first letter.
            _ if icons => String::new(),
            _ if room >= 1 => name.chars().take(1).collect(),
            _ => return None,
        };
        let mut spans = vec![Span::raw(" ")];
        if let Some(icon) = icon {
            spans.push(icon);
            if !shown.is_empty() {
                spans.push(Span::raw(" "));
            }
        }
        spans.push(Span::styled(shown, Style::new().fg(FAINT)));
        spans.extend(figures);
        (spans_width(&spans) <= cell).then_some(spans)
    };
    let all = usize::MAX;
    let layouts = [(2, width / 2), (1, width)];
    for (per_line, cell) in layouts {
        let tries = [(all, true), (1, false)];
        let found = tries.into_iter().find_map(|(windows, named)| {
            entries
                .iter()
                .map(|(said, name)| entry(said, name, cell, windows, named))
                .collect::<Option<Vec<_>>>()
        });
        let Some(found) = found else {
            continue;
        };
        return found
            .chunks(per_line)
            .map(|line| {
                let mut spans = Vec::new();
                for (i, entry) in line.iter().enumerate() {
                    let w = spans_width(entry);
                    spans.extend(entry.iter().cloned());
                    if i + 1 < line.len() {
                        spans.push(Span::raw(" ".repeat(cell - w)));
                    }
                }
                Line::from(spans)
            })
            .collect();
    }
    Vec::new()
}

/// The most of a name the sidebar shows, `…` included.
const NAME_MAX: usize = 7;

/// `name` in `room` columns: as much of it as fits before `…`, keeping a
/// number it was given (`Max#2`).
pub(super) fn cut_name(name: &str, room: usize) -> String {
    let (base, number) = match name.rsplit_once('#') {
        Some((base, n)) if !base.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (base, format!("#{n}"))
        }
        _ => (name, String::new()),
    };
    let keep = room.saturating_sub(1 + number.width());
    if keep == 0 {
        return format!(
            "{}…",
            name.chars()
                .take(room.saturating_sub(1))
                .collect::<String>()
        );
    }
    let mut out = String::new();
    for c in base.chars() {
        if out.width() + c.to_string().width() > keep {
            break;
        }
        out.push(c);
    }
    format!("{out}…{number}")
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// What's left of `usage`'s first `windows`, each with its pace's mark in its
/// colour (` 69%▲`); `–` for one started over; dim if old.
fn window_spans(usage: &Usage, windows: usize, dim: bool, now: i64) -> Vec<Span<'static>> {
    usage
        .windows
        .iter()
        .take(windows)
        .map(|w| {
            let pace = paced(w, dim, now);
            let color = if dim { FAINT } else { pace_color(pace, MUTED) };
            let left = if w.over(now) {
                "–".into()
            } else {
                w.percent()
            };
            let mark = pace.map_or("", Pace::mark);
            Span::styled(format!(" {left}{mark}"), Style::new().fg(color))
        })
        .collect()
}

/// What's left of a window as a bar `width` long, full when none of it is
/// used, in eighths, with a mark where as much of its time is left: a bar
/// ending short of the mark is used faster than its time goes. Known long ago
/// (`stale`), it's dim, without the mark.
fn bar(window: &Window, width: usize, stale: bool, now: i64) -> Vec<Span<'static>> {
    const PARTS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let over = window.over(now);
    let left = if over { 0.0 } else { window.left() };
    let eighths = (left / 100.0 * (width * 8) as f64).round() as usize;
    // Just past the bar that much time would fill (four fifths of 10 cells
    // is the ninth, not 7.999… of them).
    let mark = window
        .elapsed(now)
        .filter(|_| !over && !stale)
        .map(|gone| (((1.0 - gone) * width as f64 + 1e-9) as usize).min(width.saturating_sub(1)));
    // Dim as what's said beside it, known long ago.
    let fill = Style::new().fg(match stale {
        true => FAINT,
        false => pace_color(window.pace(now), ACCENT),
    });
    let empty = Style::new().fg(FAINT);
    let marked = Style::new().fg(ratatui::style::Color::White).bold();
    (0..width)
        .map(|i| {
            if mark == Some(i) {
                return Span::styled("│", marked);
            }
            match eighths.saturating_sub(i * 8).min(8) {
                0 => Span::styled("·", empty),
                8 => Span::styled("█", fill),
                n => Span::styled(PARTS[n - 1], fill),
            }
        })
        .collect()
}

/// The usage view: its tabs, the limits and what was spent in each period,
/// and the one chosen, scrolled as far as it goes.
pub(super) fn draw_usage(f: &mut Frame, app: &App, area: Rect) {
    let now = now_ms();
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    let [head, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    let state = match app.usage_period {
        None => limits_state(app, now),
        Some(_) if app.tokens.reading() => Span::styled("reading…", Style::new().fg(MUTED)),
        Some(_) => Span::raw(""),
    };
    let mut title = vec![
        "roster".bold(),
        Span::styled("  usage   ", Style::new().fg(MUTED)),
    ];
    title.extend(tabs(app.usage_period));
    f.render_widget(
        Paragraph::new(Line::from(fit(head.width as usize, title, vec![state]))),
        head,
    );
    let width = body.width as usize;
    let lines = match app.usage_period {
        None => limits_lines(app, width, now),
        Some(period) => {
            let mut lines = match app.tokens.report(period) {
                Some(report) => spent_lines(report, period, width),
                None => vec![Line::styled(
                    "Reading the agents' conversations…",
                    Style::new().fg(MUTED),
                )],
            };
            if let Some(error) = app.tokens.error() {
                lines.insert(0, Line::default());
                lines.insert(
                    0,
                    Line::styled(
                        format!("can't read them now: {error}"),
                        Style::new().fg(ERROR),
                    ),
                );
            }
            lines
        }
    };
    let max = lines.len().saturating_sub(body.height as usize);
    app.usage_scroll_max.set(max);
    let scroll = app.usage_scroll.min(max) as u16;
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body);
}

/// The view's tabs, the one shown standing out: `Limits │ Tokens 24h 7 days …`.
fn tabs(shown: Option<Period>) -> Vec<Span<'static>> {
    let tab = |label: &'static str, on: bool| {
        if on {
            Span::styled(
                label,
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )
        } else {
            Span::styled(label, Style::new().fg(MUTED))
        }
    };
    let mut spans = vec![
        tab("Limits", shown.is_none()),
        Span::styled("  │  Tokens  ", Style::new().fg(FAINT)),
    ];
    for (i, period) in Period::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(tab(period.label(), shown == Some(period)));
    }
    spans
}

/// When the limits were last said, or that they're being asked.
fn limits_state(app: &App, now: i64) -> Span<'static> {
    if app.limits.asking() {
        return Span::styled("asking…", Style::new().fg(MUTED));
    }
    match app
        .limits
        .agents
        .iter()
        .filter_map(|s| s.usage.as_ref())
        .map(|u| u.at)
        .max()
    {
        Some(at) => Span::styled(
            format!("as of {}", crate::util::local_when(at, now)),
            Style::new().fg(MUTED),
        ),
        None => Span::raw(""),
    }
}

/// What the TUI knows of the accounts, in the order they show: by service,
/// then by name, with the CLI Proxy's accounts; the agents alone, in the
/// order they answered (as they're offered).
pub(super) fn ordered(app: &App) -> Vec<&crate::tui::limits::Said> {
    let mut said: Vec<_> = app.limits.agents.iter().collect();
    if said.iter().any(|s| s.account.is_some()) {
        said.sort_by_cached_key(|s| {
            let rank = crate::usage::rank(&s.agent);
            let name = s.account.as_ref().map(|a| {
                let label = app.cli_proxy.label(&a.provider, a.email.as_deref());
                crate::usage::account_short(a, label).to_lowercase()
            });
            ((rank.0, rank.1.to_string()), name)
        });
    }
    said
}

/// Every agent that says what its account has used, or each account the
/// CLI Proxy holds, under its service: its plan, then each window as a bar
/// with its use, and when it starts over; what else it says; and why it
/// couldn't be asked last time, if it couldn't. Above them, whether they
/// were asked through the CLI Proxy, or why not.
fn limits_lines(app: &App, width: usize, now: i64) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if !app.account_limits {
        lines.push(Line::styled(crate::usage::OFF, Style::new().fg(MUTED)));
        return lines;
    }
    if let Some(via) = &app.limits.via
        && let Some(said) = via.said()
    {
        let color = if via.why().is_some() { ERROR } else { MUTED };
        lines.push(Line::styled(said, Style::new().fg(color)));
    }
    if app.limits.agents.is_empty() {
        let text = if app.limits.asking() || app.limits.asked().is_none() {
            "Asking the agents…"
        } else {
            "No agent here says what its account has used: Claude and codex do, logged in to a subscription."
        };
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(text, Style::new().fg(MUTED)));
    }
    let windows = app.limits.agents.iter().filter_map(|s| s.usage.as_ref());
    let label_w = windows
        .flat_map(|u| u.windows.iter().map(|w| w.label().width()))
        .max()
        .unwrap_or(0);
    // An account's lines are under its service's heading, two further in.
    let deeper = if app.limits.agents.iter().any(|s| s.account.is_some()) {
        2
    } else {
        0
    };
    // Indent, label, a space, the bar, what's left and its pace.
    let bar_w = width
        .saturating_sub(2 + deeper + label_w + 2 + 10 + 11)
        .clamp(10, 50);
    let mut service: Option<&str> = None;
    for said in ordered(app) {
        let heading = |name: String| {
            vec![
                glyph(&said.agent),
                Span::raw(" "),
                Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
            ]
        };
        let plan = said.usage.as_ref().and_then(|u| u.plan.as_ref());
        let stale_at = said
            .usage
            .as_ref()
            .filter(|u| stale(u, now))
            .map(|u| crate::util::local_when(u.at, now));
        let as_of = |right: &mut Vec<Span<'static>>| {
            if let Some(at) = &stale_at {
                right.push(Span::styled(format!("as of {at}"), Style::new().fg(FAINT)));
            }
        };
        let indent = match &said.account {
            // A service's heading, once, then the account's name under it.
            Some(account) => {
                if service != Some(said.agent.as_str()) {
                    if !lines.is_empty() {
                        lines.push(Line::default());
                    }
                    lines.push(Line::from(heading(crate::util::capitalize(&said.agent))));
                    service = Some(said.agent.as_str());
                }
                let label = app
                    .cli_proxy
                    .label(&account.provider, account.email.as_deref());
                let dim = account.disabled;
                let mut name = vec![
                    Span::raw("  "),
                    Span::styled(
                        crate::usage::account_title(account, label),
                        if dim {
                            Style::new().fg(FAINT)
                        } else {
                            Style::new()
                        },
                    ),
                ];
                if let Some(plan) = plan {
                    name.push(Span::styled(format!("  {plan}"), Style::new().fg(MUTED)));
                }
                let mut right = Vec::new();
                if dim {
                    right.push(Span::styled("disabled", Style::new().fg(FAINT)));
                } else {
                    as_of(&mut right);
                }
                lines.push(Line::from(fit(width, name, right)));
                "    ".to_string()
            }
            None => {
                service = None;
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let mut name = heading(crate::util::capitalize(&said.agent));
                if let Some(plan) = plan {
                    name.push(Span::styled(format!("  {plan}"), Style::new().fg(MUTED)));
                }
                let mut right = Vec::new();
                as_of(&mut right);
                lines.push(Line::from(fit(width, name, right)));
                "  ".to_string()
            }
        };
        let under = format!("{indent}{}", " ".repeat(label_w + 2));
        if let Some(usage) = &said.usage {
            let dim = stale(usage, now);
            for w in &usage.windows {
                let mut spans = vec![Span::styled(
                    format!("{indent}{}  ", pad(&w.label(), label_w)),
                    Style::new().fg(MUTED),
                )];
                spans.extend(bar(w, bar_w, dim, now));
                let left = if w.over(now) {
                    "–".into()
                } else {
                    w.percent()
                };
                let pace = paced(w, dim, now);
                let mut text = format!("{left:>5} left");
                if let Some(pace) = pace {
                    text.push_str(&format!("  {} {}", pace.mark(), pace.word()));
                }
                let color = if dim { FAINT } else { pace_color(pace, ACCENT) };
                spans.push(Span::styled(text, Style::new().fg(color)));
                lines.push(Line::from(spans));
                if let Some(when) = w.resets(now) {
                    lines.push(Line::styled(
                        format!("{under}{when}"),
                        Style::new().fg(FAINT),
                    ));
                }
            }
            for note in &usage.notes {
                lines.push(Line::styled(
                    format!("{under}{note}"),
                    Style::new().fg(FAINT),
                ));
            }
        } else if let Some(account) = &said.account
            && said.error.is_none()
            && !account.disabled
        {
            let why = if account.asked() {
                "no plan's limits"
            } else {
                "usage unknown: roster can't ask its service"
            };
            lines.push(Line::styled(
                format!("{indent}{why}"),
                Style::new().fg(FAINT),
            ));
        }
        if let Some(error) = &said.error {
            lines.push(Line::styled(
                format!("{indent}can't tell now: {error}"),
                Style::new().fg(ERROR),
            ));
        }
    }
    lines
}

/// The kinds of tokens, each in its colour: what's sent beyond the cache,
/// read from it, written to it, and the output.
fn kinds(c: &Counts) -> [(&'static str, u64, ratatui::style::Color); 4] {
    [
        ("input", c.input, WORKING),
        ("cache read", c.cache_read, MUTED),
        ("cache write", c.cache_write, INPUT),
        ("output", c.output, UNREAD),
    ]
}

/// The kinds of tokens as one bar `width` long, each its share of it, a kind
/// with any at least a cell.
fn kinds_bar(c: &Counts, width: usize) -> Vec<Span<'static>> {
    let kinds = kinds(c);
    let total = c.total();
    if total == 0 || width == 0 {
        return Vec::new();
    }
    let mut cells: Vec<usize> = kinds
        .iter()
        .map(|(_, n, _)| match n {
            0 => 0,
            n => ((*n as f64 / total as f64 * width as f64).round() as usize).max(1),
        })
        .collect();
    // The largest takes up what the others' rounding left over, or gives it.
    let largest = (0..cells.len()).max_by_key(|i| cells[*i]).unwrap_or(0);
    let others: usize = cells.iter().sum::<usize>() - cells[largest];
    cells[largest] = width.saturating_sub(others).max(1);
    kinds
        .iter()
        .zip(cells)
        .filter(|(_, n)| *n > 0)
        .map(|((_, _, color), n)| Span::styled("█".repeat(n), Style::new().fg(*color)))
        .collect()
}

/// What was spent in a stretch after another, as bars eighths tall, all to
/// the scale of `tallest`: blank for none, the least bar for a little.
fn chart(values: &[u64], tallest: u64) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    values
        .iter()
        .map(|&v| match v {
            0 => ' ',
            v => {
                let eighths = (v as f64 / tallest.max(1) as f64 * 8.0).ceil() as usize;
                BARS[eighths.clamp(1, 8) - 1]
            }
        })
        .collect()
}

/// What was spent in `period`: in all, of each kind, by each agent (with a
/// chart of when), and by each model.
fn spent_lines(report: &Report, period: Period, width: usize) -> Vec<Line<'static>> {
    let muted = Style::new().fg(MUTED);
    let faint = Style::new().fg(FAINT);
    let total = &report.total;
    let all = total.tokens.total();
    if all == 0 {
        let span = match period {
            Period::Day => "24 hours".to_string(),
            p => p.label().to_string(),
        };
        return vec![Line::styled(
            format!("Nothing spent in the last {span}."),
            muted,
        )];
    }
    let cost = |s: &Spent| {
        if s.unpriced() {
            "unpriced".to_string()
        } else {
            dollars(s.cost)
        }
    };
    let mut lines = Vec::new();
    let sessions = match report.sessions {
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    };
    let summary = vec![
        Span::styled(amount(all), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" tokens · {sessions} · "), muted),
        Span::styled(
            dollars(total.cost),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(" at API prices", muted),
    ];
    let saved = vec![Span::styled(
        format!("the cache saved {}", dollars(total.saved)),
        faint,
    )];
    lines.push(Line::from(fit(width, summary, saved)));
    lines.push(Line::default());
    lines.push(Line::from(kinds_bar(&total.tokens, width.min(72))));
    let mut legend = Vec::new();
    for (i, (name, n, color)) in kinds(&total.tokens).into_iter().enumerate() {
        if i > 0 {
            legend.push(Span::raw("  "));
        }
        legend.push(Span::styled("■ ", Style::new().fg(color)));
        legend.push(Span::styled(format!("{name} "), muted));
        legend.push(Span::raw(amount(n)));
    }
    lines.push(Line::from(fit(width, legend, Vec::new())));
    if total.unpriced > 0 {
        lines.push(Line::styled(
            format!(
                "{} of them are of models without a known price, not in the cost.",
                amount(total.unpriced)
            ),
            faint,
        ));
    }

    // Each agent: its conversations, tokens, share, cost, and when.
    lines.push(Line::default());
    let name_w = report
        .agents
        .iter()
        .map(|a| a.agent.width())
        .max()
        .unwrap_or(0);
    let sessions_w = report
        .agents
        .iter()
        .map(|a| a.sessions.to_string().len())
        .max()
        .unwrap_or(1);
    // Mark, name, sessions, tokens, share, cost; then the chart.
    let row_w = 2 + name_w + 2 + sessions_w + 9 + 2 + 6 + 2 + 6 + 2 + 9;
    let (stretches, each) = period.stretches();
    let room = width.saturating_sub(row_w + 3).min(stretches);
    let tallest = report
        .agents
        .iter()
        .flat_map(|a| a.chart.iter().copied())
        .max()
        .unwrap_or(0);
    for a in &report.agents {
        let share = a.spent.tokens.total() as f64 * 100.0 / all as f64;
        let mut spans = vec![
            glyph(&a.agent),
            Span::raw(" "),
            Span::styled(
                pad(&crate::util::capitalize(&a.agent), name_w),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  {:>sessions_w$} {}",
                    a.sessions,
                    if a.sessions == 1 {
                        "session "
                    } else {
                        "sessions"
                    }
                ),
                muted,
            ),
            Span::raw(format!("  {:>6}", amount(a.spent.tokens.total()))),
            Span::styled(format!("  {share:>5.1}%"), muted),
            Span::raw(format!("  {:>9}", cost(&a.spent))),
        ];
        if room >= 8 {
            let color = crate::agent::adapter(&a.agent)
                .mark()
                .and_then(|m| m.rgb)
                .map_or(ACCENT, |(r, g, b)| ratatui::style::Color::Rgb(r, g, b));
            let shown = &a.chart[a.chart.len().saturating_sub(room)..];
            spans.push(Span::raw("   "));
            spans.push(Span::styled(chart(shown, tallest), Style::new().fg(color)));
        }
        lines.push(Line::from(spans));
    }
    if room >= 8 {
        let scale = format!("each bar is {each}, the tallest {}", amount(tallest));
        let indent = (row_w + 3 + room).saturating_sub(scale.width());
        lines.push(Line::styled(
            format!("{}{scale}", " ".repeat(indent)),
            faint,
        ));
    }

    // Each model: its tokens of each kind, and cost. Narrow, input and cache
    // write go.
    lines.push(Line::default());
    let all_columns = width >= 2 + 16 + 8 + 12 + 13 + 9 + 10;
    let columns: Vec<(&str, usize)> = [
        ("Input", 8, true),
        ("Cache read", 12, false),
        ("Cache write", 13, true),
        ("Output", 9, false),
        ("Cost", 10, false),
    ]
    .into_iter()
    .filter(|(_, _, droppable)| all_columns || !droppable)
    .map(|(name, w, _)| (name, w))
    .collect();
    let longest = report
        .models
        .iter()
        .map(|m| m.model.width())
        .max()
        .unwrap_or(0);
    let model_w = width
        .saturating_sub(2 + columns.iter().map(|c| c.1).sum::<usize>())
        .clamp(8, longest.max(5) + 2);
    let mut header = format!("  {}", pad("Model", model_w));
    for (name, w) in &columns {
        header.push_str(&format!("{name:>w$}"));
    }
    lines.push(Line::styled(header, muted));
    for m in &report.models {
        let t = &m.spent.tokens;
        let figures = |name: &str| match name {
            "Input" => amount(t.input),
            "Cache read" => amount(t.cache_read),
            "Cache write" => amount(t.cache_write),
            "Output" => amount(t.output),
            _ => cost(&m.spent),
        };
        let mut spans = vec![
            glyph(&m.agent),
            Span::raw(" "),
            Span::raw(pad(&m.model, model_w)),
        ];
        for (name, w) in &columns {
            let style = if *name == "Cost" && m.spent.unpriced() {
                faint
            } else {
                Style::new()
            };
            spans.push(Span::styled(format!("{:>w$}", figures(name)), style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_kinds_share_the_bar_each_with_some_at_least_a_cell() {
        let counts = Counts {
            input: 5,
            cache_read: 900,
            cache_write: 0,
            cache_write_hour: 0,
            output: 95,
        };
        let bar = kinds_bar(&counts, 20);
        let cells: Vec<usize> = bar.iter().map(|s| s.content.chars().count()).collect();
        assert_eq!(cells, [1, 17, 2]);
        assert!(kinds_bar(&Counts::default(), 20).is_empty());
    }

    #[test]
    fn a_chart_is_to_the_tallests_scale_and_shows_a_little() {
        assert_eq!(chart(&[0, 1, 50, 100], 100), " ▁▄█");
        assert_eq!(chart(&[0, 0], 0), "  ");
    }

    #[test]
    fn a_bar_fills_by_eighths_and_marks_how_far_its_time_got() {
        let hour = 3_600_000;
        // A five-hour window an hour in (four fifths of its time left, so
        // the mark on the ninth of 10 cells), 45% used: five cells and a half
        // left, short of the mark.
        let w = Window {
            minutes: 300,
            used: 45.0,
            resets_at: Some(4 * hour),
        };
        assert_eq!(text(&bar(&w, 10, false, 0)), "█████▌··│·");
        // Known long ago: its use then, no mark of its time now, no pace.
        let old = bar(&w, 10, true, 0);
        assert_eq!(text(&old), "█████▌····");
        assert!(old.iter().all(|s| s.style.fg == Some(FAINT)));
        assert_eq!(paced(&w, false, 0), Some(Pace::Fast));
        assert_eq!(paced(&w, true, 0), None);
        // Started over: empty, unmarked.
        assert_eq!(text(&bar(&w, 10, false, 5 * hour)), "··········");
        // When it starts over unknown: no mark.
        let w = Window {
            resets_at: None,
            used: 25.0,
            ..w
        };
        assert_eq!(text(&bar(&w, 4, false, 0)), "███·");
        // None used: full; all of it: empty.
        let w = Window { used: 0.0, ..w };
        assert_eq!(text(&bar(&w, 4, false, 0)), "████");
        let w = Window { used: 100.0, ..w };
        assert_eq!(text(&bar(&w, 4, false, 0)), "····");
    }
}
