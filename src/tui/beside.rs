//! The read-only session list beside an attached agent: a roster of its own in a pane split off left of it.

use super::*;

/// The session list, read only, in a pane beside the agent you're attached to
/// (`fit_preview = "always"`, `Roster::attach_terminal`): where the TUI's list
/// is, the agent where its preview was, `id` selected. Kept up to date as the
/// TUI would, which waits on the attach meanwhile; it doesn't size agents,
/// nor take the TUI's lock. Ends with its pane.
pub fn run_sidebar(roster: &mut Roster, id: &str) -> Result<()> {
    let keymap = Keymap::new(&roster.config.keys)?;
    let mut app = App::new(keymap, String::new(), agent::available(&roster.config));
    app.order = roster.config.order.clone();
    app.notes = roster.config.notes.clone();
    app.card_subagents = roster.config.card_subagents();
    app.set_memory_limit(roster.config.memory_warning());
    app.set_grouped(roster.config.grouped());
    // The TUI's list as it was, its accounts' limits as it last knew them
    // (not asked here); from the CLI, none.
    app.sidebar_usage = false;
    if let Some(view) = std::env::var(LIST_VIEW)
        .ok()
        .and_then(|view| serde_json::from_str(&view).ok())
    {
        app.take_view(view);
    }
    app.select(list::EntryKey::Session(id.to_string()));
    roster.watch_beside(id);
    let mut terminal = ratatui::init();
    let result = sidebar_loop(&mut terminal, roster, &mut app, id);
    ratatui::restore();
    result
}

pub(super) fn sidebar_loop(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: &str,
) -> Result<()> {
    let pane = std::env::var("TMUX_PANE").ok();
    let status_line = roster.config.status_line();
    let back = roster.config.keys.detach.clone();
    let mut shown = None;
    loop {
        // Read only: the keys are the agent's, and what roster reads and
        // types of the session goes to the pane that has them.
        if let Some(pane) = &pane {
            roster.tmux.give_back_keys(pane).ok();
        }
        if let Err(e) = roster.refresh() {
            app.fail(format!("{e:#}"));
        }
        if let (Ok(projects), Ok(sessions)) = (roster.projects(), roster.sessions(true)) {
            // The session you're in, wherever it moves: out of a folded
            // group (which is selected meanwhile) too.
            app.select(list::EntryKey::Session(id.to_string()));
            app.set_data(projects, sessions);
        }
        read_subagents(app);
        // What codex wrote of its limits since, which costs nothing to read.
        if app.sidebar_usage {
            app.limits.read_transcripts(&app.sessions);
        }
        // The status line's left, the list's top line, kept as it changes,
        // and the title with it.
        if status_line && let Some(s) = app.sessions.iter().find(|s| s.id == id) {
            let line = status_line_beside(&app.sessions, s, terminal.size()?.width);
            if shown.as_ref() != Some(&line) {
                roster.tmux.label(&s.tmux_name(), &line.0, &line.1).ok();
                shown = Some(line);
            }
        }
        if app.memory_limit.is_some()
            && (app.resources_read).is_none_or(|at| at.elapsed() >= RESOURCES_EVERY)
        {
            app.set_resources(roster.resources().unwrap_or_default());
            app.resources_read = Some(Instant::now());
        }
        terminal.draw(|f| ui::draw_attached_sidebar(f, app, !status_line, &back))?;
        // A resize redraws; a key reaching the list, a click or a key of the
        // user's having given it the keys, gives them back.
        if event::poll(TICK)? {
            event::read()?;
        }
    }
}

/// The read-only list's width beside an agent `agent` columns wide (kept at
/// the preview's), attached in a terminal `width` wide: the TUI's sidebar and
/// its divider, where the TUI's are, so the agent, past the border tmux keeps
/// between them (in the TUI's blank column), is where its preview was;
/// narrower beside an agent wider than the preview; None if too narrow to be
/// of use.
pub fn attached_list(width: u16, agent: u16) -> Option<u16> {
    let list = (ui::sidebar_width(width) + 1).min(width.checked_sub(agent.saturating_add(1))?);
    (list >= MIN_LIST).then_some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_beside_an_attached_agent_is_where_the_tuis_is() {
        // An agent at the preview's width: the list as wide as the TUI's
        // sidebar and divider, the agent past the border, filling the rest.
        let preview = 120 - ui::sidebar_width(120) - 2;
        let list = attached_list(120, preview).unwrap();
        assert_eq!(list, ui::sidebar_width(120) + 1);
        assert_eq!(list + 1 + preview, 120);
        // A wider terminal: the TUI's width still, the rest blank.
        assert_eq!(
            attached_list(250, preview),
            Some(ui::sidebar_width(250) + 1)
        );
        // Narrower beside an agent wider than the preview, till it's of no use.
        assert_eq!(attached_list(120, 90), Some(29));
        assert_eq!(attached_list(120, 100), None);
        assert_eq!(attached_list(80, 90), None);
    }
}
