//! The words the TUI shows from its state: key hints, and the preview for
//! what isn't a session's screen.

use ratatui::text::{Line, Text};

use super::*;
use crate::tui::theme;

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
            Mode::Settings => self.settings_view_hints(),
        };
        // Then the same everywhere: Esc, help, and in the session list quit (in a
        // view the quit key goes back, like Esc).
        match self.mode {
            Mode::Sessions if !self.search.is_empty() => hints.push(("Esc".into(), "clear filter")),
            Mode::Sessions => {}
            Mode::Projects | Mode::Server | Mode::Settings => hints.push(("Esc".into(), "back")),
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

    /// The settings view's: what changes the selected setting.
    fn settings_view_hints(&self) -> Vec<(String, &'static str)> {
        use crate::settings::Kind;
        if let Some(capture) = &self.capturing {
            let mut hints = vec![("a key".to_string(), "sets it")];
            if capture.setting.starts_with("keys.") && capture.setting != "keys.detach" {
                hints.push(("Backspace".into(), "none"));
            }
            hints.push(("Esc".into(), "cancel"));
            return hints;
        }
        let Some(s) = self.selected_setting() else {
            return vec![("e".into(), "edit config.toml")];
        };
        let mut hints = match s.kind {
            Kind::Bool => self.labelled([(Action::Confirm, "switch")]),
            Kind::Choice(_) => {
                let mut hints = self.labelled([(Action::Confirm, "next")]);
                hints.push(("←→".into(), "choose"));
                hints
            }
            Kind::Keys => {
                let mut hints = self.labelled([(Action::Confirm, "press its key")]);
                hints.push(("a".into(), "add a key"));
                hints
            }
            Kind::TmuxKey => self.labelled([(Action::Confirm, "press its key")]),
            _ => self.labelled([(Action::Confirm, "change")]),
        };
        if s.set {
            hints.push(("d".into(), "default"));
        }
        if self.selected_agent().is_some() {
            hints.push(("x".into(), "remove its settings"));
        }
        hints.push(("n".into(), "new agent"));
        hints.push(("e".into(), "edit config.toml"));
        hints
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

    /// Preview for a session with no screen to show (archived, or stopped and
    /// gone from tmux): where it got to, what it is, what it was about (its
    /// last `prompts`), and what can be done with it.
    pub fn session_info(&self, s: &Session, prompts: &[String], now: i64) -> Text<'static> {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::Span;
        let muted = Style::new().fg(theme::MUTED);
        let field = |label: &str, mut value: Vec<Span<'static>>| {
            value.insert(0, Span::styled(format!("  {label:<11}"), muted));
            Line::from(value)
        };
        let ago = |at: i64| match crate::util::ago(now - at) {
            just if just == "now" => "just now".to_string(),
            ago => format!("{ago} ago"),
        };
        let mut lines = vec![match (s.deleted_at, s.exit_status) {
            (Some(at), _) => Line::from(format!("Archived {}.", ago(at))),
            (None, Some(code)) => Line::from(format!(
                "Stopped {}: its agent exited with {code}.",
                ago(s.state_since.unwrap_or(s.created_at))
            )),
            (None, None) => Line::from(format!(
                "Stopped {}.",
                ago(s.state_since.unwrap_or(s.created_at))
            )),
        }];
        lines.push(Line::default());
        let project = self.project(s.project_id);
        lines.push(field(
            "Project",
            vec![
                crate::tui::ui::badge(&s.project),
                Span::raw(format!(" {}  ", s.project)),
                Span::styled(
                    project.map_or_else(String::new, |p| crate::util::short_path(&p.origin)),
                    muted,
                ),
            ],
        ));
        if project.is_none_or(|p| p.origin != s.workdir) {
            lines.push(field(
                "Directory",
                vec![Span::raw(crate::util::short_path(&s.workdir))],
            ));
        }
        lines.push(field("Agent", crate::tui::ui::agent_spans(s)));
        let whose = match (&s.title_override, &s.title) {
            (Some(_), _) => Some("yours".to_string()),
            (None, Some(_)) => Some(format!("{}'s", crate::util::capitalize(&s.agent))),
            (None, None) => None,
        };
        if let (Some(title), Some(whose)) = (s.title(), whose) {
            lines.push(field(
                "Title",
                vec![
                    Span::raw(format!("{title} ")),
                    Span::styled(format!("({whose})"), muted),
                ],
            ));
        }
        if !prompts.is_empty() {
            lines.push(Line::default());
            lines.push(Line::styled(
                "Last prompts",
                Style::new().add_modifier(Modifier::BOLD),
            ));
            for p in prompts {
                lines.push(Line::from(vec![
                    Span::styled("  › ", muted),
                    Span::raw(crate::util::one_line(p)),
                ]));
            }
        }
        // What can be done with it.
        lines.push(Line::default());
        let conversation = match (&s.conversation_id, self.resumes(&s.agent)) {
            (Some(id), true) => format!("resumes its conversation ({})", id.get(..8).unwrap_or(id)),
            _ => "starts a new conversation".to_string(),
        };
        if s.deleted_at.is_some() {
            let restore = self.keymap.label(Action::Confirm).unwrap_or("?");
            let delete = self.chord(Menu::Session, Action::Delete);
            lines.push(Line::from(format!(
                "{restore} restores it; starting it then {conversation}."
            )));
            let kept = if crate::agent::adapter(&s.agent).adopts() {
                format!(
                    "; its {} conversation stays and can be adopted again",
                    crate::util::capitalize(&s.agent)
                )
            } else {
                String::new()
            };
            lines.push(Line::from(format!("{delete} deletes it for good{kept}.")));
        } else {
            let start = self.chord(Menu::Session, Action::Start);
            lines.push(Line::from(format!("{start} starts it; it {conversation}.")));
        }
        Text::from(lines)
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
            Mode::Projects | Mode::Server | Mode::Settings => Text::default(),
        }
    }
}
