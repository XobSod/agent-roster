//! What the TUI's actions do: in the session list and the views.

use super::*;

impl App {
    pub(super) fn act(&mut self, action: Action) -> Option<Op> {
        // What works the same everywhere.
        match action {
            Action::Help => self.help = true,
            Action::Open(menu) => self.menu = Some(menu),
            // Only the session list quits; in a view it's back, like Esc.
            Action::Quit if self.mode != Mode::Sessions => self.set_mode(Mode::Sessions),
            Action::Quit => return Some(Op::Quit),
            Action::ShowServer => self.toggle_view(Mode::Server),
            Action::ShowSettings => self.toggle_view(Mode::Settings),
            Action::ShowUsage => self.toggle_view(Mode::Usage),
            Action::ShowProjects => self.toggle_view(Mode::Projects),
            _ => {
                return match self.mode {
                    Mode::Sessions => self.act_in_list(action),
                    Mode::Projects => self.act_in_projects_view(action),
                    Mode::Server => self.act_in_server_view(action),
                    Mode::Settings => self.act_in_settings_view(action),
                    Mode::Usage => self.act_in_usage_view(action),
                };
            }
        }
        None
    }

    /// A view, or back to the session list from it.
    fn toggle_view(&mut self, view: Mode) {
        self.set_mode(if self.mode == view {
            Mode::Sessions
        } else {
            view
        });
    }

    /// In the session list.
    fn act_in_list(&mut self, action: Action) -> Option<Op> {
        match action {
            // The list wraps around.
            Action::Up => self.selected = wrap(self.selected, self.entries.len(), false),
            Action::Down => self.selected = wrap(self.selected, self.entries.len(), true),
            Action::Collapse => self.fold(true),
            Action::Expand => self.fold(false),
            Action::Confirm => return self.confirm_selected(),
            Action::Search => self.typing = Some((self.search.clone(), self.selected_key())),
            Action::FilterProject => self.toggle_filter(),
            Action::EditSession
            | Action::Start
            | Action::Stop
            | Action::Restart
            | Action::Archive
            | Action::Delete
            | Action::ToggleRead
            | Action::Retitle
            | Action::Note => return self.session_action(action),
            _ => return self.act_on_projects(action),
        }
        None
    }

    /// In the projects view: move, pick a project to see its sessions; the
    /// project menu acts on the selected row.
    fn act_in_projects_view(&mut self, action: Action) -> Option<Op> {
        match action {
            Action::Up => self.project_row = wrap(self.project_row, self.projects.len(), false),
            Action::Down => self.project_row = wrap(self.project_row, self.projects.len(), true),
            Action::Confirm | Action::FilterProject => match self.projects.get(self.project_row) {
                Some(p) => {
                    self.filter = Some(p.id);
                    self.set_mode(Mode::Sessions);
                }
                None => {
                    let key = self.chord(Menu::Project, Action::NewProject);
                    self.fail(format!("No projects yet; {key} adds one"));
                }
            },
            Action::Collapse | Action::Expand => {}
            Action::NewProject
            | Action::EditProject
            | Action::RemoveProject
            | Action::ProjectNote
            | Action::NewSession
            | Action::NewChat
            | Action::Adopt => return self.act_on_projects(action),
            _ => self.fail("This is the projects view: Esc goes back to the sessions"),
        }
        None
    }

    /// Projects, new sessions, chats and adopting: from the session list or
    /// the projects view.
    fn act_on_projects(&mut self, action: Action) -> Option<Op> {
        match action {
            Action::NewProject => {
                self.open_form(Form::new_project(&self.default_path, &self.agents));
            }
            Action::EditProject => match self.selected_project() {
                Some(p) => {
                    let builtin = self.chat_project == Some(p.id);
                    self.open_form(Form::edit_project(&p, &self.agents, builtin))
                }
                None => {
                    self.fail("Select a project (group by project) or one of its sessions first")
                }
            },
            Action::RemoveProject => self.remove_project(),
            Action::ProjectNote => return self.project_note(),
            Action::NewSession => self.new_session(),
            Action::NewChat => return Some(Op::NewChat),
            Action::Adopt => return Some(Op::FindConversations),
            _ => {}
        }
        None
    }

    /// In the server view: scroll, kill orphans, or leave.
    fn act_in_server_view(&mut self, action: Action) -> Option<Op> {
        match action {
            Action::Up => self.report_scroll = self.report_scroll.saturating_sub(1),
            Action::Down => {
                self.report_scroll =
                    (self.report_scroll + 1).min(self.report.len().saturating_sub(1))
            }
            Action::Confirm if self.orphans > 0 => {
                let question = format!(
                    "Kill {} orphaned tmux session(s)? Their programs end.",
                    self.orphans
                );
                self.confirm(question, Op::KillOrphans);
            }
            Action::Confirm => self.flash("No orphaned tmux sessions to remove"),
            _ => self.fail("This is the server view: Esc goes back to the sessions"),
        }
        None
    }

    /// In the usage view: left and right go along its tabs (the limits, then
    /// what was spent in each period), Enter asks the agents again (the
    /// limits) or reads their files again (what they spent), up and down
    /// scroll.
    fn act_in_usage_view(&mut self, action: Action) -> Option<Op> {
        use crate::tokens::Period;
        match action {
            Action::Collapse | Action::Expand => {
                let tabs: Vec<Option<Period>> =
                    std::iter::once(None).chain(Period::ALL.map(Some)).collect();
                let at = tabs
                    .iter()
                    .position(|t| *t == self.usage_period)
                    .unwrap_or(0);
                let to = match action {
                    Action::Collapse => at.saturating_sub(1),
                    _ => (at + 1).min(tabs.len() - 1),
                };
                if to != at {
                    self.usage_period = tabs[to];
                    self.usage_scroll = 0;
                    if self.usage_period.is_none() {
                        self.limits.want(false);
                    }
                }
            }
            Action::Confirm if self.usage_period.is_some() => self.tokens.want(),
            Action::Confirm if self.limits.asking() => self.flash("Asking them already"),
            Action::Confirm => self.limits.want(true),
            Action::Up => self.usage_scroll = self.usage_scroll.saturating_sub(1),
            Action::Down => {
                self.usage_scroll = (self.usage_scroll + 1).min(self.usage_scroll_max.get())
            }
            _ => self.fail("This is the usage view: Esc goes back to the sessions"),
        }
        None
    }

    /// Folds (or unfolds) the selected group; from a session, its group.
    pub(super) fn fold(&mut self, fold: bool) {
        let group = match self.selected_entry() {
            Some(Entry::Heading { group, .. }) => *group,
            Some(Entry::Card(s) | Entry::Row(s)) if fold => list::group_of(s, self.grouped),
            _ => return,
        };
        let folds = if self.hits.is_some() {
            &mut self.search_folded
        } else {
            &mut self.folded
        };
        let changed = if fold {
            folds.insert(group)
        } else {
            folds.remove(&group)
        };
        if changed {
            self.select_next = Some(EntryKey::Group(group));
            self.rebuild();
        }
    }

    pub(super) fn confirm_selected(&mut self) -> Option<Op> {
        match self.selected_entry().cloned()? {
            Entry::Heading { open, .. } => {
                self.fold(open);
                None
            }
            Entry::Card(s) | Entry::Row(s) if s.deleted_at.is_some() => self.restore(s),
            Entry::Card(s) | Entry::Row(s) => {
                // Starting an agent that can't resume loses its conversation: ask.
                if s.state == State::Stopped && !crate::agent::resumes(&s) {
                    self.confirm(self.new_conversation_question(&s), Op::Attach(s.id));
                    return None;
                }
                Some(Op::Attach(s.id))
            }
        }
    }

    pub(super) fn session_action(&mut self, action: Action) -> Option<Op> {
        let Some(s) = self.selected_session().cloned() else {
            self.fail("Select a session first");
            return None;
        };
        // Archived sessions have notes too.
        if action == Action::Note {
            return self.open_note(Some(s.id.clone()), s.project_id, Side::Session, s.noted());
        }
        let archive = self.chord(Menu::Session, Action::Archive);
        if s.deleted_at.is_some() {
            return match action {
                Action::Archive => self.restore(s),
                Action::Delete => {
                    let kept = if crate::agent::adapter(&s.agent).adopts() {
                        format!(
                            "; its {} conversation stays and can be adopted again ({})",
                            crate::util::capitalize(&s.agent),
                            self.chord(Menu::Session, Action::Adopt)
                        )
                    } else {
                        String::new()
                    };
                    let note = if s.note.is_some() {
                        " and its note"
                    } else {
                        ""
                    };
                    self.confirm(
                        format!(
                            "Delete {} for good? Roster forgets it{note}{kept}.",
                            s.label()
                        ),
                        Op::Delete(s.id),
                    );
                    None
                }
                _ => {
                    self.fail(format!("{} is archived; {archive} restores it", s.label()));
                    None
                }
            };
        }
        match action {
            Action::EditSession => {
                self.open_form(Form::edit_session(&s));
                None
            }
            Action::Start | Action::Restart if !crate::agent::resumes(&s) => {
                let op = if action == Action::Start {
                    Op::Start(s.id.clone())
                } else {
                    Op::Restart(s.id.clone())
                };
                self.confirm(self.new_conversation_question(&s), op);
                None
            }
            Action::Start => Some(Op::Start(s.id)),
            // Stopping a working session cuts its turn short, as restarting
            // one does: both ask.
            Action::Stop if s.state == State::Working => {
                self.confirm(
                    format!("{} is working. Stop it?", s.label()),
                    Op::Stop(s.id),
                );
                None
            }
            Action::Stop => Some(Op::Stop(s.id)),
            Action::Restart if s.state == State::Working => {
                self.confirm(
                    format!("{} is working. Restart it?", s.label()),
                    Op::Restart(s.id),
                );
                None
            }
            Action::Restart => Some(Op::Restart(s.id)),
            Action::Archive => {
                let question = format!(
                    "Archive {}? Its agent stops; {archive} on it under Archived restores it.",
                    s.label()
                );
                self.confirm(question, Op::Archive(s.id));
                None
            }
            Action::Delete => {
                self.fail(format!(
                    "Only archived sessions can be deleted; {archive} archives {}",
                    s.label()
                ));
                None
            }
            Action::ToggleRead => Some(Op::SetUnread(s.id, !s.unread)),
            Action::Retitle => Some(Op::RetitleSession(s.id)),
            _ => None,
        }
    }

    /// The project the selection belongs to; with only one project, that one.
    /// `p o`: the selected project's note, opened beside the selected
    /// session's (`[` shows that), if it's one of the project's.
    fn project_note(&mut self) -> Option<Op> {
        let Some(p) = self.selected_project() else {
            self.fail("Select a project (group by project) or one of its sessions first");
            return None;
        };
        let session = self
            .selected_session()
            .filter(|s| s.project_id == p.id && self.mode == Mode::Sessions)
            .map(|s| s.id.clone());
        self.open_note(session, p.id, Side::Project, p.noted())
    }

    /// Opens note `n`, `side`'s of the session `session` (if opened on one)
    /// in `project`: in the editor, or in `$EDITOR` if config.toml says so.
    fn open_note(
        &mut self,
        session: Option<String>,
        project: i64,
        side: Side,
        n: crate::note::Noted,
    ) -> Option<Op> {
        match self.notes.editor() {
            crate::config::NoteEditor::External => Some(Op::EditNote(n.whose)),
            crate::config::NoteEditor::Builtin => {
                let editor = NoteEditor::new(&n, self.notes.escape());
                self.note = Some(OpenNotes::new(session, project, side, editor));
                None
            }
        }
    }

    pub(super) fn selected_project(&self) -> Option<Project> {
        if self.mode == Mode::Projects {
            return self.projects.get(self.project_row).cloned();
        }
        let id = match self.selected_entry() {
            Some(Entry::Heading {
                group: Group::Project(id),
                ..
            }) => Some(*id),
            Some(Entry::Card(s) | Entry::Row(s)) => Some(s.project_id),
            _ => self.filter,
        };
        match id {
            Some(id) => self.project(id).cloned(),
            None if self.projects.len() == 1 => self.projects.first().cloned(),
            None => None,
        }
    }

    pub(super) fn new_session(&mut self) {
        if self.projects.is_empty() {
            let key = self.chord(Menu::Project, Action::NewProject);
            let chat = self.chord(Menu::Session, Action::NewChat);
            self.fail(format!(
                "Add a project first ({key}), or start a chat, which needs none ({chat})"
            ));
            return;
        }
        let selected = self.selected_project();
        self.open_form(Form::new_session(
            &self.projects,
            selected.as_ref(),
            &self.agents,
        ));
    }

    /// Offers `found` to pick from.
    pub fn pick_conversation(&mut self, found: Vec<crate::adopt::Adoptable>) {
        if found.is_empty() {
            self.flash("No conversations to adopt");
            return;
        }
        self.dialog = Some(Dialog::Picker(Picker::new(found)));
    }

    /// The form for adopting `a`, or for an archived session's conversation, restoring it.
    pub(super) fn adopt(&mut self, a: crate::adopt::Adoptable) {
        match a.archived {
            // Its name taken meanwhile: restoring asks for another.
            Some(s) if self.name_taken(&s) => {
                self.restore(s);
            }
            Some(s) => self.confirm(
                format!(
                    "That conversation is the archived session {}. Restore it?",
                    s.label()
                ),
                Op::Restore {
                    id: s.id,
                    name: None,
                },
            ),
            None => self.open_form(Form::adopt(&a, self.default_agent())),
        }
    }

    /// The new-chat form, once the chats project exists (`projects` has it).
    pub fn open_chat_form(&mut self, projects: &[Project], chats: &Project) {
        self.open_form(Form::new_chat(projects, chats, &self.agents));
    }

    pub(super) fn toggle_filter(&mut self) {
        if self.filter.is_some() {
            self.filter = None;
        } else if let Some(p) = self.selected_project() {
            self.filter = Some(p.id);
        } else {
            self.fail("Select a session or a project first");
            return;
        }
        self.rebuild();
    }

    pub(super) fn remove_project(&mut self) {
        let Some(p) = self.selected_project() else {
            self.fail("Select a project (group by project) or one of its sessions first");
            return;
        };
        let (archived, live): (Vec<&Session>, Vec<&Session>) = self
            .sessions
            .iter()
            .filter(|s| s.project_id == p.id)
            .partition(|s| s.deleted_at.is_some());
        if !live.is_empty() {
            let archive = self.chord(Menu::Session, Action::Archive);
            self.fail(format!(
                "{} still has sessions; archive them first ({archive})",
                p.name
            ));
            return;
        }
        let question = crate::engine::project_removal_question(&p, archived);
        self.confirm(question, Op::RemoveProject(p.name));
    }

    /// Whether a live session in `s`'s project has its name.
    pub(super) fn name_taken(&self, s: &Session) -> bool {
        self.sessions
            .iter()
            .any(|o| o.deleted_at.is_none() && o.project_id == s.project_id && o.name == s.name)
    }

    pub(super) fn restore(&mut self, s: Session) -> Option<Op> {
        if self.name_taken(&s) {
            let prompt = format!(
                "{} already has a session named {}. Restore as:",
                s.project, s.name
            );
            self.input(&prompt, String::new(), Purpose::RestoreName { id: s.id });
            return None;
        }
        Some(Op::Restore {
            id: s.id,
            name: None,
        })
    }
}
