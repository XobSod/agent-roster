//! How a session reads as spans, shared by the list, the preview's header and
//! tmux's status line while attached.

use super::*;

/// The project's badge and name, in its colour (`marks` matched the filter),
/// then `▤` if it has a note (`noted`), the name cut so both fit in `width`.
pub(super) fn project_name(
    project: &str,
    marks: &[usize],
    noted: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let mark = match noted {
        true => vec![Span::styled(
            format!(" {PROJECT_NOTE}"),
            Style::new().fg(MUTED),
        )],
        false => Vec::new(),
    };
    let badge = badge(project);
    let room =
        width.saturating_sub(badge.width() + 1 + mark.iter().map(Span::width).sum::<usize>());
    let style = Style::new().fg(theme::project_color(project));
    [
        vec![badge, Span::raw(" ")],
        marked(&truncate(project, room), marks, style),
        mark,
    ]
    .concat()
}

/// The colored two-letter project badge.
pub(crate) fn badge(project: &str) -> Span<'static> {
    Span::styled(
        theme::monogram(project),
        Style::new()
            .fg(theme::project_color(project))
            .add_modifier(Modifier::BOLD),
    )
}

/// The agent's mark (its adapter's), else the generic one.
pub(crate) fn glyph(agent: &str) -> Span<'static> {
    match crate::agent::adapter(agent).mark() {
        Some(m) => {
            let style = m.rgb.map_or(Style::new(), |(r, g, b)| {
                Style::new().fg(ratatui::style::Color::Rgb(r, g, b))
            });
            Span::styled(m.glyph, style)
        }
        None => Span::styled("◇", Style::new().fg(MUTED)),
    }
}

/// A card's second line: the agent's mark, then the name if it was chosen
/// (a made-up one only repeats the type), the type in words if its mark
/// doesn't say it, the model and the effort.
pub(super) fn details(s: &Session, name_marks: &[usize], style: Style) -> Vec<Span<'static>> {
    let mut parts = Vec::new();
    if !s.has_default_name() {
        parts.push(marked(&s.name, name_marks, style));
    }
    if crate::agent::adapter(&s.agent).mark().is_none() {
        parts.push(vec![Span::styled(s.agent.clone(), style)]);
    }
    let model = [s.model_label(), s.effort_label()];
    let model: Vec<String> = model.into_iter().flatten().collect();
    if !model.is_empty() {
        parts.push(vec![Span::styled(model.join(" "), style)]);
    }
    let mut spans = vec![glyph(&s.agent)];
    for (i, part) in parts.into_iter().enumerate() {
        spans.push(Span::styled(if i == 0 { " " } else { " · " }, style));
        spans.extend(part);
    }
    spans
}

/// The state at the right of a card: a word, a spinner with a clock, or an age.
pub(super) fn status(s: &Session, now: i64) -> Vec<Span<'static>> {
    let since = now - s.state_since.unwrap_or(s.created_at);
    let span = match (s.state, s.unread) {
        (State::NeedsInput, _) => {
            Span::styled("Input", Style::new().fg(INPUT).add_modifier(Modifier::BOLD))
        }
        (State::Working, _) => Span::styled(
            format!("{} {}", theme::spinner(now), theme::clock(since)),
            Style::new().fg(WORKING),
        ),
        (State::Idle, true) => {
            Span::styled(format!("● {}", theme::ago(since)), Style::new().fg(UNREAD))
        }
        (State::Idle, false) => Span::styled(theme::ago(since), Style::new().fg(MUTED)),
        (State::Stopped, _) => match s.exit_status {
            Some(code) if code != 0 => Span::styled(format!("exit {code}"), Style::new().fg(ERROR)),
            _ => Span::styled("stopped", Style::new().fg(FAINT)),
        },
    };
    vec![span]
}

/// The state in words, for the header.
pub(super) fn state_words(s: &Session, now: i64) -> Span<'static> {
    let since = now - s.state_since.unwrap_or(s.created_at);
    match (s.state, s.unread) {
        (State::NeedsInput, _) => Span::styled("needs input", Style::new().fg(INPUT)),
        (State::Working, _) => Span::styled(
            format!("working {}", theme::clock(since)),
            Style::new().fg(WORKING),
        ),
        (State::Idle, true) => Span::styled(
            format!("finished {}", theme::ago(since)),
            Style::new().fg(UNREAD),
        ),
        (State::Idle, false) => Span::styled(
            format!("idle {}", theme::ago(since)),
            Style::new().fg(MUTED),
        ),
        (State::Stopped, _) => match s.exit_status {
            Some(code) if code != 0 => {
                Span::styled(format!("exited {code}"), Style::new().fg(ERROR))
            }
            _ => Span::styled("stopped", Style::new().fg(MUTED)),
        },
    }
}

/// Project / name / title, as headers show a session: the name only if it
/// was chosen (not the made-up `claude-1`), the title in bold; with no title
/// the name takes its place.
pub(super) fn whereabouts(s: &Session) -> Vec<Span<'static>> {
    let slash = || Span::styled(" / ", Style::new().fg(FAINT));
    let mut spans = vec![Span::styled(s.project.clone(), Style::new().fg(MUTED))];
    let bold = Style::new().add_modifier(Modifier::BOLD);
    match s.title() {
        Some(title) => {
            if !s.has_default_name() {
                spans.extend([slash(), Span::raw(s.name.clone())]);
            }
            spans.extend([slash(), Span::styled(title.to_string(), bold)]);
        }
        None => spans.extend([slash(), Span::styled(s.name.clone(), bold)]),
    }
    spans
}

/// The agent's glyph and name, model and effort, as in the header.
pub(crate) fn agent_spans(s: &Session) -> Vec<Span<'static>> {
    let mut spans = vec![
        glyph(&s.agent),
        Span::raw(" "),
        Span::styled(s.agent.clone(), Style::new().fg(MUTED)),
    ];
    for value in [s.model_label(), s.effort_label()].into_iter().flatten() {
        spans.push(Span::styled(" · ", Style::new().fg(FAINT)));
        spans.push(Span::styled(value, Style::new().fg(MUTED)));
    }
    spans
}

/// The line roster's tmux shows at the top while attached to `s`, left and
/// right, as tmux markup: the preview's header, with the back key where the
/// state would go stale.
pub fn status_line(s: &Session, detach_key: &str) -> (String, String) {
    let left = [
        vec![Span::raw(" "), badge(&s.project), Span::raw(" ")],
        whereabouts(s),
    ]
    .concat();
    let mut right = agent_spans(s);
    right.extend([
        Span::raw("   "),
        Span::styled(
            detach_key.to_string(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(" back ", Style::new().fg(MUTED)),
    ]);
    (tmux_markup(&left), tmux_markup(&right))
}

/// Spans as tmux status-line markup, each with its colour (`terminal`: the
/// terminal's own; `default` would be the line's muted one) and weight, `#`
/// doubled so text shows as typed (verified on tmux 3.5a).
pub(super) fn tmux_markup(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        let fg = match span.style.fg {
            Some(ratatui::style::Color::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "terminal".to_string(),
        };
        let weight = if span.style.add_modifier.contains(Modifier::BOLD) {
            "bold"
        } else {
            "nobold"
        };
        out.push_str(&format!(
            "#[fg={fg},{weight}]{}",
            span.content.replace('#', "##")
        ));
    }
    out
}
