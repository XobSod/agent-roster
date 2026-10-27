//! The words the TUI shows from its state: key hints, and the preview for
//! what isn't a session's screen.

use ratatui::text::{Line, Text};

use super::*;
use crate::tui::theme;

impl App {
    /// Footer hints: keys that act here and now.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        if let Some(editor) = &self.note {
            return editor.hints();
        }
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
            Mode::Usage => {
                // Nothing to ask again with account_limits off.
                let mut hints = match (self.usage_period, self.account_limits) {
                    (None, false) => Vec::new(),
                    (None, true) => self.labelled([(Action::Confirm, "ask again")]),
                    (Some(_), _) => self.labelled([(Action::Confirm, "read again")]),
                };
                if let (Some(left), Some(right)) = (
                    self.keymap.label(Action::Collapse),
                    self.keymap.label(Action::Expand),
                ) {
                    hints.push((format!("{left}/{right}"), "tabs"));
                }
                hints
            }
        };
        // Then the same everywhere: Esc, help, and in the session list quit (in a
        // view the quit key goes back, like Esc).
        match self.mode {
            Mode::Sessions if !self.search.is_empty() => hints.push(("Esc".into(), "clear filter")),
            Mode::Sessions => {}
            Mode::Projects | Mode::Server | Mode::Settings | Mode::Usage => {
                hints.push(("Esc".into(), "back"))
            }
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
            if capture.setting == "keys.detach" {
                return vec![
                    ("a key with C- or M-, or an F key".to_string(), "sets it"),
                    ("Esc".into(), "cancel"),
                ];
            }
            let mut hints = vec![("a key".to_string(), "sets it")];
            if capture.setting.starts_with("keys.") {
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
            Kind::Model => {
                let mut hints = self.labelled([(Action::Confirm, "type")]);
                hints.push(("←→".into(), "choose"));
                hints
            }
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
        crate::agent::new_conversation_question(s)
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
        // Its note's first line it shows (the pinned one).
        let note = p.note.as_ref().and_then(|n| {
            let shown = n.shown(self.notes.lines()).lines;
            let line = shown.into_iter().find(|l| !l.trim().is_empty())?;
            Some(Line::from(format!("note      {line}")))
        });
        Text::from_iter(
            builtin
                .into_iter()
                .chain([
                    format!("origin    {}", p.origin.display()).into(),
                    format!("sessions  {}", p.sessions_label(&self.sessions)).into(),
                    format!("env file  {} ({env_state})", env.display()).into(),
                ])
                .chain(note)
                .chain([
                    "".into(),
                    format!(
                        "Press {} to start a session here.",
                        self.chord(Menu::Session, Action::NewSession)
                    )
                    .into(),
                ]),
        )
    }

    /// Preview for a group's heading, a shelf or (grouped by project) a
    /// project: what it is, then its sessions at a glance, each with what
    /// it's waiting for and since when, its last prompt (`prompts`, by
    /// session) and its note's line; as many as fit `size`.
    pub fn group_info(
        &self,
        group: Group,
        prompts: &HashMap<String, String>,
        size: (u16, u16),
        now: i64,
    ) -> Text<'static> {
        use ratatui::style::Style;
        let (width, height) = (size.0 as usize, size.1 as usize);
        let muted = Style::new().fg(theme::MUTED);
        let Some(header) = self.group_header(group, width) else {
            return Text::default();
        };
        let members = self.group_members(group);
        let count = members.len();
        let mut blocks: Vec<Vec<Line<'static>>> = members
            .iter()
            .map(|s| {
                let prompt = prompts.get(&s.id).map(String::as_str);
                member_block(s, group, prompt, self.notes.lines(), width, now)
            })
            .collect();
        // What the group is only when a session fits below it (or "No
        // sessions here."); else sessions first.
        let below_header = height.saturating_sub(header.len() + 1);
        let with_header = match count {
            0 => below_header > 0,
            _ => fitting(&blocks, below_header) > 0,
        };
        let room = if with_header { below_header } else { height };
        let mut shown = fitting(&blocks, room);
        // Not even one whole: a line each.
        if shown == 0 {
            blocks.iter_mut().for_each(|b| b.truncate(1));
            shown = fitting(&blocks, height);
        }
        let mut lines = Vec::new();
        if with_header {
            lines.extend(header);
            lines.push(Line::default());
        }
        for block in blocks.into_iter().take(shown) {
            lines.extend(block);
        }
        match count - shown {
            _ if count == 0 => lines.push(Line::styled("  No sessions here.", muted)),
            0 => {}
            // Not even a line each: how many there are.
            _ if shown == 0 => lines.push(Line::styled(
                format!("  {count} session{}", if count == 1 { "" } else { "s" }),
                muted,
            )),
            more => lines.push(Line::styled(format!("     … and {more} more"), muted)),
        }
        lines.truncate(height);
        Text::from(lines)
    }

    /// What a heading's group is, in lines `width` wide (its name and how
    /// many it holds are the preview's header); None for a project gone
    /// meanwhile.
    fn group_header(&self, group: Group, width: usize) -> Option<Vec<Line<'static>>> {
        use crate::tui::ui::{fit, wrap};
        use ratatui::style::Style;
        use ratatui::text::Span;
        let muted = Style::new().fg(theme::MUTED);
        let p = match group {
            Group::Shelf(shelf) => {
                let meaning = wrap(shelf_meaning(shelf), width)
                    .into_iter()
                    .map(Line::from);
                let order = wrap(&self.shelf_order(shelf), width)
                    .into_iter()
                    .map(|l| Line::styled(l, muted));
                return Some(meaning.chain(order).collect());
            }
            Group::Project(id) => self.project(id)?,
        };
        let field = |label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(format!("  {label:<11}"), muted)];
            spans.extend(value);
            Line::from(fit(width, spans, Vec::new()))
        };
        let mut header = Vec::new();
        if self.chat_project == Some(p.id) {
            header.push(field(
                "Built-in",
                vec![Span::raw(format!(
                    "for chats ({}); its directory follows chats_dir in config.toml",
                    self.chord(Menu::Session, Action::NewChat)
                ))],
            ));
        }
        header.push(field(
            "Origin",
            vec![Span::raw(crate::util::short_path(&p.origin))],
        ));
        let env = p.origin.join(PROJECT_ENV);
        let sourced = match env.exists() {
            true => "sourced before every start",
            false => "none",
        };
        header.push(field(
            "Env file",
            vec![
                Span::raw(format!("{} ", crate::util::short_path(&env))),
                Span::styled(format!("({sourced})"), muted),
            ],
        ));
        header.push(field(
            "Defaults",
            vec![
                Span::raw(p.defaults_label(self.default_agent(), &self.models.names)),
                Span::styled(" for its new sessions", muted),
            ],
        ));
        Some(header)
    }

    /// How a shelf is ordered, as config.toml's `[order]` has it.
    fn shelf_order(&self, shelf: Shelf) -> String {
        use crate::config::First;
        let o = &self.order;
        let (first, key, how) = match shelf {
            Shelf::NeedsInput => (o.needs_input, "needs_input", "waiting"),
            Shelf::Idle => (o.idle, "idle", "idle"),
            Shelf::Stopped => (o.stopped, "stopped", "stopped"),
            Shelf::Working => (o.working, "working", "working"),
            Shelf::Archived => (o.archived, "archived", "archived"),
        };
        match (shelf, first) {
            // Questions and permissions come before turns to read, whatever
            // the order within them.
            (Shelf::NeedsInput, First::Oldest) => format!(
                "Questions and permissions come first, then turns to read; within each, the one {how} longest is on top (order.{key})."
            ),
            (Shelf::NeedsInput, First::Newest) => format!(
                "Questions and permissions come first, then turns to read; within each, the latest to get here is on top (order.{key})."
            ),
            (Shelf::Archived, First::Newest) => {
                "The latest archived is on top (order.archived).".into()
            }
            (Shelf::Archived, First::Oldest) => {
                "The first archived is on top (order.archived).".into()
            }
            (_, First::Oldest) => format!("The one {how} longest is on top (order.{key})."),
            (_, First::Newest) => format!("The latest to get here is on top (order.{key})."),
        }
    }

    /// Preview for a stopped or archived session: where it got to, what it
    /// is, how its screen ended if its agent exited with an error
    /// (`last_screen`, see `screen_tail`), its note, and what it was about
    /// (its last `prompts`).
    pub fn session_info(
        &self,
        s: &Session,
        prompts: &[String],
        last_screen: &[String],
        now: i64,
    ) -> Text<'static> {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::Span;
        let muted = Style::new().fg(theme::MUTED);
        let field = |label: &str, mut value: Vec<Span<'static>>| {
            value.insert(0, Span::styled(format!("  {label:<14}"), muted));
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
        // What starting it goes on with: its conversation once its agent has
        // written it. Until then, a fork of the one it was adopted from, or
        // a new one.
        let short = |id: &str| id.chars().take(8).collect::<String>();
        let written = s
            .transcript_path
            .as_deref()
            .is_some_and(|p| std::path::Path::new(p).exists());
        let conversation = match (&s.conversation_id, &s.fork_from) {
            _ if !self.resumes(&s.agent) => vec![Span::styled("a new one when started", muted)],
            (Some(id), _) if written => vec![
                Span::raw(short(id)),
                Span::styled(" (resumed when started)", muted),
            ],
            (_, Some(from)) => vec![
                Span::styled("a fork of ", muted),
                Span::raw(short(from)),
                Span::styled(" when started", muted),
            ],
            _ => vec![Span::styled("a new one when started", muted)],
        };
        lines.push(field("Conversation", conversation));
        // One its agent exited before it said it took (Codex, at a screen
        // it showed first, say): kept, to give again when asked.
        // Said on a line of its own, which a long prompt doesn't push off.
        if let Some(prompt) = &s.given_prompt {
            lines.push(field(
                "First prompt",
                vec![Span::raw(crate::util::one_line(prompt))],
            ));
            // The key first, where a narrow preview cuts the line; none
            // where it doesn't work (archived) or isn't bound.
            let key = (self.keymap.label(Action::Open(Menu::Session)))
                .zip(self.keymap.menu_label(Menu::Session, Action::PromptAgain));
            let said = match (s.deleted_at, key) {
                (None, Some((open, key))) => {
                    format!("{open} {key} gives it again (not seen taken)")
                }
                _ => "not seen taken".to_string(),
            };
            lines.push(field("", vec![Span::styled(said, muted)]));
        }
        // Before what's longer, so an error isn't the part that doesn't fit.
        if !last_screen.is_empty() {
            lines.push(Line::default());
            lines.push(Line::styled(
                "Its last screen",
                Style::new().add_modifier(Modifier::BOLD),
            ));
            lines.extend(last_screen.iter().map(|l| Line::from(format!("  {l}"))));
        }
        // Unless it's open in the editor above.
        let whose = crate::note::Whose::Session(s.id.clone());
        if let Some(note) = s.note.as_ref().filter(|_| {
            self.note
                .as_ref()
                .is_none_or(|open| open.shown().whose != whose)
        }) {
            lines.push(Line::default());
            lines.push(Line::styled(
                "Note",
                Style::new().add_modifier(Modifier::BOLD),
            ));
            for (i, line) in note.text.lines().enumerate() {
                let mark = match note.pin == Some(i) {
                    true => Span::styled("  ✎ ", Style::new().fg(theme::ACCENT)),
                    false => Span::raw("    "),
                };
                lines.push(Line::from(vec![
                    mark,
                    Span::raw(line.replace('\t', "    ")),
                ]));
            }
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
            Mode::Projects | Mode::Server | Mode::Settings | Mode::Usage => Text::default(),
        }
    }
}

/// How a dead pane's screen ended, worth showing after its agent's error:
/// `raw` (captured without colours) without its last row, tmux's "Pane is
/// dead" whatever `remain-on-exit-format` makes it, its rows wrapped to
/// `width` (the pane is often wider than the preview), and of those the last
/// `rows`, from the first that isn't blank.
pub fn screen_tail(raw: &str, rows: usize, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let mut lines: Vec<&str> = raw.lines().collect();
    lines.pop();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let width = width.max(1);
    let mut wrapped: Vec<String> = Vec::new();
    for line in lines {
        let (mut row, mut used) = (String::new(), 0);
        for c in line.trim_end().replace('\t', "    ").chars() {
            let w = c.width().unwrap_or(0);
            if used + w > width && used > 0 {
                wrapped.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(c);
            used += w;
        }
        wrapped.push(row);
    }
    let start = wrapped.len().saturating_sub(rows);
    wrapped
        .into_iter()
        .skip(start)
        .skip_while(|l| l.trim().is_empty())
        .collect()
}

/// What a shelf holds, in a sentence.
fn shelf_meaning(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::NeedsInput => {
            "Waiting for you: a question, a permission, or a turn you haven't read."
        }
        Shelf::Idle => "Their turn done and read, each waits for your next prompt.",
        Shelf::Stopped => {
            "Their agents aren't running; starting one resumes its conversation where its agent can."
        }
        Shelf::Working => "At work on what you gave them: nothing to do until they finish.",
        Shelf::Archived => {
            "Put away with their conversations, kept until restored or deleted for good."
        }
    }
}

/// A session's lines in a heading's preview: what it is (`member_line`),
/// its last prompt and its note's line (the first not blank of the `shown`
/// lines it shows), each cut to `width`.
fn member_block(
    s: &Session,
    group: Group,
    prompt: Option<&str>,
    shown: usize,
    width: usize,
    now: i64,
) -> Vec<Line<'static>> {
    use crate::tui::ui::truncate;
    use ratatui::style::Style;
    use ratatui::text::Span;
    let indented = |mark: Span<'static>, text: &str, style: Style| {
        let text = truncate(&crate::util::one_line(text), width.saturating_sub(7));
        Line::from(vec![Span::raw("     "), mark, Span::styled(text, style)])
    };
    let muted = Style::new().fg(theme::MUTED);
    let mut lines = vec![member_line(s, group, width, now)];
    if let Some(prompt) = prompt {
        lines.push(indented(Span::styled("› ", muted), prompt, muted));
    }
    let note = s.note.as_ref().and_then(|n| {
        n.shown(shown)
            .lines
            .into_iter()
            .find(|l| !l.trim().is_empty())
            .map(str::to_string)
    });
    if let Some(line) = note {
        let mark = Span::styled("✎ ", Style::new().fg(theme::ACCENT));
        lines.push(indented(mark, &line, Style::new()));
    }
    lines
}

/// How many of the sessions' `blocks` fit in `room` lines, with one for "…
/// and N more" if not all do.
fn fitting(blocks: &[Vec<Line>], room: usize) -> usize {
    let mut used = 0;
    for (i, block) in blocks.iter().enumerate() {
        let more = usize::from(i + 1 < blocks.len());
        if used + block.len() + more > room {
            return i;
        }
        used += block.len();
    }
    blocks.len()
}

/// A session's line in a heading's preview: its agent's mark, where it is
/// (its project too, on a shelf) and its title; on the right what it's
/// waiting for, and since when.
fn member_line(s: &Session, group: Group, width: usize, now: i64) -> Line<'static> {
    use crate::tui::ui::{badge, fit, glyph};
    use ratatui::style::Style;
    use ratatui::text::Span;
    let muted = Style::new().fg(theme::MUTED);
    let slash = || Span::styled(" / ", Style::new().fg(theme::FAINT));
    let mut left = vec![Span::raw("  "), glyph(&s.agent), Span::raw(" ")];
    if matches!(group, Group::Shelf(_)) {
        left.extend([
            badge(&s.project),
            Span::raw(" "),
            Span::styled(s.project.clone(), muted),
            slash(),
        ]);
    }
    match s.title() {
        Some(title) => {
            if !s.has_default_name() {
                left.extend([Span::styled(s.name.clone(), muted), slash()]);
            }
            left.push(Span::raw(title.to_string()));
        }
        None => left.push(Span::raw(s.name.clone())),
    }
    let right = vec![waiting_words(s, now), Span::raw(" ")];
    Line::from(fit(width, left, right))
}

/// What a session is waiting for, and since when.
fn waiting_words(s: &Session, now: i64) -> ratatui::text::Span<'static> {
    use ratatui::style::{Color, Style};
    let ago = |at: i64| theme::ago(now - at);
    let at = s.state_since.unwrap_or(s.created_at);
    let (words, color): (String, Color) = match (s.deleted_at, s.state, s.unread) {
        (Some(archived), ..) => (format!("archived {}", ago(archived)), theme::FAINT),
        (None, State::NeedsInput, _) => (format!("needs input {}", ago(at)), theme::INPUT),
        (None, State::Idle, true) => (format!("turn to read {}", ago(at)), theme::UNREAD),
        (None, State::Idle, false) => (format!("idle {}", ago(at)), theme::MUTED),
        (None, State::Working, _) => (format!("working {}", ago(at)), theme::WORKING),
        (None, State::Stopped, _) => match s.exit_status {
            Some(code) if code != 0 => (format!("exited {code} · {}", ago(at)), theme::ERROR),
            _ => (format!("stopped {}", ago(at)), theme::MUTED),
        },
    };
    ratatui::text::Span::styled(words, Style::new().fg(color))
}
