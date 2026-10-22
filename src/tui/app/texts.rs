//! The words the TUI shows from its state: key hints, and the preview for
//! what isn't a session's screen.

use ratatui::text::{Line, Text};

use super::*;

impl App {
    /// Footer hints: keys that act here and now.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        if self.typing.is_some() {
            return [("Enter", "keep"), ("Esc", "cancel"), ("↑↓ C-p C-n", "move")]
                .map(|(key, what)| (key.to_string(), what))
                .into();
        }
        let mut hints = match self.mode {
            Mode::Sessions => {
                let enter = match self.selected_entry() {
                    Some(Entry::Heading { open: true, .. }) => "fold",
                    Some(Entry::Heading { open: false, .. }) => "unfold",
                    Some(e) if e.session().is_some_and(|s| s.deleted_at.is_some()) => "restore",
                    _ => "attach",
                };
                let mut actions = vec![(Action::Confirm, enter)];
                actions.extend(Menu::ALL.map(|m| (Action::Open(m), m.name())));
                actions.push((Action::Search, "filter"));
                self.labelled(actions)
            }
            Mode::Projects => self.projects_view_hints(),
            Mode::Server if self.orphans > 0 => {
                self.labelled([(Action::Confirm, "kill orphans"), (Action::Down, "scroll")])
            }
            Mode::Server => self.labelled([(Action::Down, "scroll")]),
        };
        // Then the same everywhere: Esc, help, and in the session list quit (in a
        // view the quit key goes back, like Esc).
        match self.mode {
            Mode::Sessions if !self.search.is_empty() => hints.push(("Esc".into(), "clear filter")),
            Mode::Sessions => {}
            Mode::Projects | Mode::Server => hints.push(("Esc".into(), "back")),
        }
        hints.extend(self.labelled([(Action::Help, "help")]));
        if self.mode == Mode::Sessions {
            hints.extend(self.labelled([(Action::Quit, "quit")]));
        }
        hints
    }

    /// Hints for actions with a key.
    fn labelled(
        &self,
        actions: impl IntoIterator<Item = (Action, &'static str)>,
    ) -> Vec<(String, &'static str)> {
        actions
            .into_iter()
            .filter_map(|(a, what)| Some((self.keymap.label(a)?.to_string(), what)))
            .collect()
    }

    /// The projects view's own hints: its project keys work without `p`.
    fn projects_view_hints(&self) -> Vec<(String, &'static str)> {
        let km = &self.keymap;
        let mut hints = self.labelled([(Action::Confirm, "show its sessions")]);
        for (action, what) in [
            (Action::NewProject, "new"),
            (Action::EditProject, "edit"),
            (Action::RemoveProject, "remove"),
        ] {
            let key = km.direct_label(Menu::Project, action).or_else(|| {
                km.menu_label(Menu::Project, action)
                    .map(|_| self.chord(Menu::Project, action))
            });
            hints.extend(key.map(|key| (key, what)));
        }
        hints.extend(
            self.labelled([Menu::Session, Menu::View].map(|m| (Action::Open(m), m.name()))),
        );
        hints
    }

    /// "s n"-style text for a menu key, for hints in messages.
    pub fn chord(&self, menu: Menu, action: Action) -> String {
        let open = self.keymap.label(Action::Open(menu)).unwrap_or("?");
        let key = self.keymap.menu_label(menu, action).unwrap_or("?");
        format!("{open} {key}")
    }

    pub(super) fn new_conversation_question(&self, s: &Session) -> String {
        format!(
            "{} ({}) can't resume its conversation. Start a new one?",
            s.label(),
            s.agent
        )
    }

    /// Preview for a project heading.
    pub fn project_info(&self, p: &Project) -> Text<'static> {
        let env = p.origin.join(PROJECT_ENV);
        let env_state = if env.exists() {
            "sourced before every start"
        } else {
            "none"
        };
        let builtin = (self.chat_project == Some(p.id)).then(|| {
            Line::from(format!(
                "built-in  for chats ({}); its directory follows chats_dir in config.toml",
                self.chord(Menu::Session, Action::NewChat)
            ))
        });
        Text::from_iter(
            builtin.into_iter().chain([
                format!("origin    {}", p.origin.display()).into(),
                format!("sessions  {}", p.sessions_label(&self.sessions)).into(),
                format!("env file  {} ({env_state})", env.display()).into(),
                "".into(),
                format!(
                    "Press {} to start a session here.",
                    self.chord(Menu::Session, Action::NewSession)
                )
                .into(),
            ]),
        )
    }

    /// Preview for a shelf heading.
    pub fn shelf_info(&self, shelf: Shelf, count: usize) -> Text<'static> {
        let what = match shelf {
            Shelf::NeedsInput => {
                "waiting for your input: a question, a permission, or a finished turn you haven't seen"
            }
            Shelf::Idle => "idle, waiting for your next prompt",
            Shelf::Stopped => {
                "stopped: idle without a running agent; starting one resumes its conversation where the agent can"
            }
            Shelf::Working => "working; nothing to do until they finish",
            Shelf::Archived => "archived: finished, put away with their conversations",
        };
        let fold = self.keymap.label(Action::Collapse).unwrap_or("?");
        let unfold = self.keymap.label(Action::Expand).unwrap_or("?");
        Text::from(vec![
            format!(
                "{count} session{} {what}.",
                if count == 1 { "" } else { "s" }
            )
            .into(),
            "".into(),
            format!("{fold} folds the shelf, {unfold} unfolds it.").into(),
        ])
    }

    /// Preview for an archived session.
    pub fn archived_info(&self, s: &Session) -> Text<'static> {
        let restore = self.keymap.label(Action::Confirm).unwrap_or("?");
        let delete = self.chord(Menu::Session, Action::Delete);
        Text::from(vec![
            format!(
                "conversation  {}",
                s.conversation_id.as_deref().unwrap_or("none")
            )
            .into(),
            format!("directory     {}", s.workdir.display()).into(),
            "".into(),
            format!("Press {restore} to restore it; starting it then resumes the conversation.")
                .into(),
            format!("{delete} deletes it for good.").into(),
        ])
    }

    /// Preview when there's nothing to select.
    pub fn empty_info(&self) -> Text<'static> {
        match self.mode {
            Mode::Sessions | Mode::Projects if self.projects.is_empty() => Text::from(vec![
                Line::from(format!(
                    "No projects yet. Press {} to add one: a name and its directory.",
                    self.chord(Menu::Project, Action::NewProject)
                )),
                Line::from(format!(
                    "Or press {} to start a chat, which needs no project.",
                    self.chord(Menu::Session, Action::NewChat)
                )),
            ]),
            Mode::Sessions if self.hits.is_some() => {
                Text::from(format!("No session matches “{}”.", self.search.trim()))
            }
            Mode::Sessions => Text::from(format!(
                "No sessions yet. Press {} to start one, or {} for a chat.",
                self.chord(Menu::Session, Action::NewSession),
                self.chord(Menu::Session, Action::NewChat)
            )),
            Mode::Projects | Mode::Server => Text::default(),
        }
    }
}
