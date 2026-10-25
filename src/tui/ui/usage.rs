//! What the agents' accounts have used of their limits, and what they spent:
//! the usage view, and the sidebar's line under the session list.

use super::*;
use crate::tokens::{Counts, Period, Report, Spent, amount, dollars};
use crate::tui::limits::STALE_MS;
use crate::usage::{Pace, Usage, Window};

/// What a window's pace shows in: `quiet` while it lasts, then louder.
fn pace_color(pace: Pace, quiet: ratatui::style::Color) -> ratatui::style::Color {
    match pace {
        Pace::Fine => quiet,
        Pace::Short => INPUT,
        Pace::Out => ERROR,
    }
}

fn stale(usage: &Usage, now: i64) -> bool {
    now - usage.at > STALE_MS
}

/// The sidebar's line: what's left of each agent's windows, shortest first
/// (`claude 69% · 48%  codex 93% · 87%`), by its mark instead of its name
/// where names don't fit; dim if old, louder as it runs short. None until
/// an agent has said.
pub(super) fn usage_line(app: &App, width: usize, now: i64) -> Option<Line<'static>> {
    let said: Vec<(&str, &Usage)> = app
        .limits
        .agents
        .iter()
        .filter_map(|s| Some((s.agent.as_str(), s.usage.as_ref()?)))
        .collect();
    if said.is_empty() {
        return None;
    }
    let line = |named: bool| {
        let mut spans = vec![Span::raw(" ")];
        for (i, (agent, usage)) in said.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let color = if stale(usage, now) {
                FAINT
            } else {
                pace_color(usage.pace(now), MUTED)
            };
            spans.push(if named {
                Span::styled(agent.to_string(), Style::new().fg(FAINT))
            } else {
                glyph(agent)
            });
            spans.push(Span::styled(
                format!(" {}", usage.brief(now)),
                Style::new().fg(color),
            ));
        }
        spans
    };
    let width_of = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let named = line(true);
    let spans = if width_of(&named) <= width {
        named
    } else {
        line(false)
    };
    Some(Line::from(fit(width, spans, Vec::new())))
}

/// What's left of a window as a bar `width` long, full when none of it is
/// used, in eighths, with a mark where as much of its time is left: a bar
/// ending short of the mark is used faster than its time goes.
fn bar(window: &Window, width: usize, now: i64) -> Vec<Span<'static>> {
    const PARTS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let over = window.over(now);
    let left = if over { 0.0 } else { window.left() };
    let eighths = (left / 100.0 * (width * 8) as f64).round() as usize;
    // Just past the bar that much time would fill (four fifths of 10 cells
    // is the ninth, not 7.999… of them).
    let mark = window
        .elapsed(now)
        .filter(|_| !over)
        .map(|gone| (((1.0 - gone) * width as f64 + 1e-9) as usize).min(width.saturating_sub(1)));
    let fill = Style::new().fg(pace_color(window.pace(now), ACCENT));
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
            if let Some(error) = &app.tokens.error {
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

/// Every agent that says what its account has used: its plan, then each
/// window as a bar with its use, and when it starts over; what else it says;
/// and why it couldn't be asked last time, if it couldn't.
fn limits_lines(app: &App, width: usize, now: i64) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if app.limits.agents.is_empty() {
        let text = if app.limits.asking() || app.limits.asked().is_none() {
            "Asking the agents…"
        } else {
            "No agent here says what its account has used: Claude and codex do, logged in to a subscription."
        };
        lines.push(Line::styled(text, Style::new().fg(MUTED)));
    }
    let windows = app.limits.agents.iter().filter_map(|s| s.usage.as_ref());
    let label_w = windows
        .flat_map(|u| u.windows.iter().map(|w| w.label().width()))
        .max()
        .unwrap_or(0);
    // Indent, label, a space, the bar, what's left.
    let bar_w = width.saturating_sub(2 + label_w + 2 + 10).clamp(10, 50);
    let indent = " ".repeat(2 + label_w + 2);
    for said in &app.limits.agents {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut heading = vec![
            glyph(&said.agent),
            Span::raw(" "),
            Span::styled(
                crate::util::capitalize(&said.agent),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(plan) = said.usage.as_ref().and_then(|u| u.plan.as_ref()) {
            heading.push(Span::styled(format!("  {plan}"), Style::new().fg(MUTED)));
        }
        let mut right = Vec::new();
        if let Some(usage) = &said.usage
            && stale(usage, now)
        {
            let at = crate::util::local_when(usage.at, now);
            right.push(Span::styled(format!("as of {at}"), Style::new().fg(FAINT)));
        }
        lines.push(Line::from(fit(width, heading, right)));
        if let Some(usage) = &said.usage {
            let dim = stale(usage, now);
            for w in &usage.windows {
                let mut spans = vec![Span::styled(
                    format!("  {}  ", pad(&w.label(), label_w)),
                    Style::new().fg(MUTED),
                )];
                spans.extend(bar(w, bar_w, now));
                let left = if w.over(now) {
                    "–".into()
                } else {
                    w.percent()
                };
                spans.push(Span::styled(
                    format!("{left:>5} left"),
                    Style::new().fg(if dim {
                        FAINT
                    } else {
                        pace_color(w.pace(now), ACCENT)
                    }),
                ));
                lines.push(Line::from(spans));
                if let Some(when) = w.resets(now) {
                    lines.push(Line::styled(
                        format!("{indent}{when}"),
                        Style::new().fg(FAINT),
                    ));
                }
            }
            for note in &usage.notes {
                lines.push(Line::styled(
                    format!("{indent}{note}"),
                    Style::new().fg(FAINT),
                ));
            }
        }
        if let Some(error) = &said.error {
            lines.push(Line::styled(
                format!("  can't tell now: {error}"),
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
        assert_eq!(text(&bar(&w, 10, 0)), "█████▌··│·");
        // Started over: empty, unmarked.
        assert_eq!(text(&bar(&w, 10, 5 * hour)), "··········");
        // When it starts over unknown: no mark.
        let w = Window {
            resets_at: None,
            used: 25.0,
            ..w
        };
        assert_eq!(text(&bar(&w, 4, 0)), "███·");
        // None used: full; all of it: empty.
        let w = Window { used: 0.0, ..w };
        assert_eq!(text(&bar(&w, 4, 0)), "████");
        let w = Window { used: 100.0, ..w };
        assert_eq!(text(&bar(&w, 4, 0)), "····");
    }
}
