//! What the agents' accounts have used of their limits: the usage view, and
//! the sidebar's line under the session list.

use super::*;
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

/// Every agent that says what its account has used: its plan, then each
/// window as a bar with its use, and when it starts over; what else it says;
/// and why it couldn't be asked last time, if it couldn't.
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
    let state = if app.limits.asking() {
        Span::styled("asking…", Style::new().fg(MUTED))
    } else {
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
    };
    let title = vec![
        "roster".bold(),
        Span::styled("  usage", Style::new().fg(MUTED)),
    ];
    f.render_widget(
        Paragraph::new(Line::from(fit(head.width as usize, title, vec![state]))),
        head,
    );

    let width = body.width as usize;
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
    f.render_widget(Paragraph::new(lines), body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
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
