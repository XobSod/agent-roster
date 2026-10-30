//! Attaching a terminal to a session, and the read-only list beside it.

use super::*;

impl Roster {
    /// For the read-only list beside the agent of session `id`, attached:
    /// refreshing only reads, as what it'd start (typing a first prompt in,
    /// asking a title) would die with the list's pane, left to the roster
    /// that attached; what `id` does is seen as it does it; the agent's
    /// redrawing for the size it was just given isn't a turn.
    pub fn watch_beside(&mut self, id: &str) {
        self.watching = Some(id.to_string());
        self.resized
            .insert(id.to_string(), now_ms() + RESIZE_GRACE_MS);
    }

    /// Attaches the terminal to a session, starting it first if it's stopped, and
    /// marks it read once the user detaches.
    /// Returns whether you submitted a prompt while attached (for agents that
    /// report prompts: Claude, pi).
    pub fn attach(&mut self, id: &str) -> Result<bool> {
        let since = self.attach_terminal(id, None)?;
        self.detached(id, since)
    }

    /// `attach` up to the user detaching; returns when it attached, for
    /// `detached`. Apart so the TUI can draw itself again in between.
    pub fn attach_terminal(&mut self, id: &str, view: Option<&str>) -> Result<i64> {
        self.refresh()?;
        let s = self.live_session(id)?;
        if s.state == State::Stopped {
            self.start(id)?;
        }
        // A first prompt still to go in: nothing refreshes while you're
        // attached, so a roster of its own types it in once the agent is ready.
        if self.db.session(id)?.pending_prompt.is_some() {
            self.deliver_in_background(id);
        }
        let name = s.tmux_name();
        // Not known (tmux not answering), no layout: the attach says why.
        let pane = (self.tmux.panes().ok()).and_then(|panes| panes.get(&name).cloned());
        // Attached elsewhere, or the list beside it not closed yet: laid out,
        // and its status line set, for that terminal.
        let elsewhere = pane.as_ref().is_some_and(|p| p.clients > 0 || p.beside);
        // The status line says where you are, drawn like the TUI's header. Only
        // a nicety: if tmux won't take it, attach anyway.
        if self.config.status_line() && !elsewhere {
            let (left, right) = crate::tui::status_line(&s, &self.config.keys.detach);
            self.tmux.label(&name, &left, &right).ok();
        }
        // Fitted always: the agent's width kept, the preview's, the terminal's
        // height (less the status line) given, a height not wrapping its
        // history; the session list beside it, read only, where the TUI's
        // is, the agent where its preview was. Too narrow for both, the agent
        // alone, as wide as the terminal allows. Fitted otherwise: a size
        // kept from before, let go, so the terminal sizes it.
        let mut beside = None;
        if agent::fit_mode(&s.agent, &self.config) != FitPreview::Always {
            self.tmux.release_size(&name).ok();
        } else if let (Some((w, h)), Some(pane)) = (util::terminal_size(), pane)
            && !elsewhere
        {
            let size = (
                pane.size.0.min(w),
                h - u16::from(self.config.status_line() && h > 1),
            );
            // The list gone, the agent at the preview's height, as the TUI
            // would fit it: told once.
            let after = (size.0, self.preview_size().map_or(size.1, |(_, h)| h));
            let owner = procs::identity(std::process::id());
            match (crate::tui::attached_list(w, size.0), owner) {
                (Some(list), Some(owner)) => {
                    let exe = self.exe.to_string_lossy().into_owned();
                    let argv = [exe, "sidebar".to_string(), s.id.clone()];
                    let owned = (owner.as_str(), after);
                    let env = view
                        .map(|view| (crate::tui::LIST_VIEW, view.to_string()))
                        .into_iter()
                        .collect::<Vec<_>>();
                    let place = (s.workdir.as_path(), env.as_slice(), argv.as_slice());
                    match (self.tmux).split_sidebar(&name, size, list, owned, place) {
                        Ok(pane) => {
                            // The list's top line over it, kept by the list.
                            if self.config.status_line()
                                && let Ok(sessions) = self.sessions(true)
                            {
                                let (left, right) =
                                    crate::tui::status_line_beside(&sessions, &s, list);
                                self.tmux.label(&name, &left, &right).ok();
                            }
                            beside = Some((pane, after));
                        }
                        // Only a nicety: without it, the agent alone.
                        Err(_) => self.tmux.resize_fixed(&name, size)?,
                    }
                }
                _ => self.tmux.resize_fixed(&name, size)?,
            }
        }
        let since = now_ms();
        let status = self.tmux.attach(&name);
        // Ended mid-attach (its terminal closed), the list closes itself so;
        // kept for a terminal still attached, it closes once none is.
        if let Some((pane, after)) = beside {
            let closed = self.tmux.release_sidebar(&name, &pane, after);
            if closed.unwrap_or(true) {
                self.resized
                    .insert(s.id.clone(), now_ms() + RESIZE_GRACE_MS);
            }
        }
        if !status?.success() {
            bail!("couldn't attach to {}", s.label());
        }
        Ok(since)
    }

    /// After detaching from `id`, attached `since`: whatever happened while
    /// attached counts as seen. Whether a prompt was submitted meanwhile.
    pub fn detached(&mut self, id: &str, since: i64) -> Result<bool> {
        self.refresh()?;
        self.set_unread(id, false)?;
        self.db.prompted_since(id, since)
    }
}
