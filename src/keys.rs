//! TUI key bindings: global keys plus three menus (session,
//! project, view). A menu key opens a popup with that menu's keys, so the same
//! letter can mean the same thing in each menu (`n` is new in all of them).
//! Every key is remappable or disabled in `[keys]` of config.toml, with names like
//! `j`, `D`, `Enter`, `C-c`, `M-x`.

use std::collections::HashMap;

use anyhow::{Result, bail};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{KeyList, Keys};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Menu {
    Session,
    Project,
    View,
}

impl Menu {
    pub const ALL: [Menu; 3] = [Menu::Session, Menu::Project, Menu::View];

    pub fn name(self) -> &'static str {
        match self {
            Menu::Session => "session",
            Menu::Project => "project",
            Menu::View => "view",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Up,
    Down,
    Collapse,
    Expand,
    Confirm,
    Quit,
    Help,
    Open(Menu),
    NewSession,
    /// A session in the built-in chats project.
    NewChat,
    EditSession,
    Start,
    Stop,
    Restart,
    /// Archive, or restore when archived.
    Archive,
    /// Delete an archived session for good.
    Delete,
    ToggleRead,
    Retitle,
    Adopt,
    NewProject,
    EditProject,
    RemoveProject,
    FilterProject,
    ToggleGroup,
    ShowProjects,
    ShowServer,
}

impl Action {
    /// How `menu`'s popup and the help describe it there: in the project menu
    /// the projects view is the (p)roject (l)ist.
    pub fn describe_in(self, menu: Menu) -> &'static str {
        match (menu, self) {
            (Menu::Project, Action::ShowProjects) => "list",
            _ => self.describe(),
        }
    }

    /// How the help and the menu popups describe it.
    pub fn describe(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::Collapse => "fold",
            Action::Expand => "unfold",
            Action::Confirm => "attach / open",
            Action::Quit => "quit",
            Action::Help => "help",
            Action::Open(menu) => menu.name(),
            Action::NewSession | Action::NewProject => "new",
            Action::NewChat => "new chat (no project)",
            Action::EditSession | Action::EditProject => "edit",
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Archive => "archive / restore",
            Action::Delete => "delete for good",
            Action::ToggleRead => "read/unread",
            Action::Retitle => "new title",
            Action::Adopt => "adopt a conversation",
            Action::RemoveProject => "remove",
            Action::FilterProject => "show only this",
            Action::ToggleGroup => "group by project / shelves",
            Action::ShowProjects => "projects",
            Action::ShowServer => "tmux server",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
}

const NAMED: &[(&str, KeyCode)] = &[
    ("Enter", KeyCode::Enter),
    ("Esc", KeyCode::Esc),
    ("Tab", KeyCode::Tab),
    ("BackTab", KeyCode::BackTab),
    ("Space", KeyCode::Char(' ')),
    ("Backspace", KeyCode::Backspace),
    ("Delete", KeyCode::Delete),
    ("Insert", KeyCode::Insert),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("PageUp", KeyCode::PageUp),
    ("PageDown", KeyCode::PageDown),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
];

impl Key {
    pub fn parse(name: &str) -> Result<Key> {
        let (mut rest, mut ctrl, mut alt) = (name, false, false);
        loop {
            if let Some(r) = rest.strip_prefix("C-").filter(|r| !r.is_empty()) {
                (rest, ctrl) = (r, true);
            } else if let Some(r) = rest.strip_prefix("M-").filter(|r| !r.is_empty()) {
                (rest, alt) = (r, true);
            } else {
                break;
            }
        }
        let mut chars = rest.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) if ctrl => KeyCode::Char(c.to_ascii_lowercase()),
            (Some(c), None) => KeyCode::Char(c),
            _ => match NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(rest)) {
                Some((_, code)) => *code,
                None => match rest
                    .strip_prefix(['F', 'f'])
                    .and_then(|n| n.parse::<u8>().ok())
                {
                    Some(n @ 1..=12) => KeyCode::F(n),
                    _ => bail!("unknown key {name:?}"),
                },
            },
        };
        Ok(Key { code, ctrl, alt })
    }

    pub fn from_event(e: &KeyEvent) -> Key {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let code = match e.code {
            KeyCode::Char(c) if ctrl => KeyCode::Char(c.to_ascii_lowercase()),
            code => code,
        };
        Key {
            code,
            ctrl,
            alt: e.modifiers.contains(KeyModifiers::ALT),
        }
    }

    /// How hints show the key.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            code => NAMED
                .iter()
                .find(|(_, c)| *c == code)
                .map_or("?".to_string(), |(n, _)| n.to_string()),
        };
        format!(
            "{}{}{base}",
            if self.ctrl { "C-" } else { "" },
            if self.alt { "M-" } else { "" }
        )
    }
}

/// One binding table: the top level or one menu, with keys in display order.
#[derive(Default)]
struct Table {
    keys: HashMap<Key, Action>,
    order: Vec<Action>,
    /// Each action's first key here (an action can be in several menus).
    labels: HashMap<Action, String>,
}

impl Table {
    fn items(&self) -> Vec<(&str, Action)> {
        self.order
            .iter()
            .filter_map(|a| Some((self.labels.get(a)?.as_str(), *a)))
            .collect()
    }

    fn bind(
        &mut self,
        table: &str,
        setting: &str,
        list: &KeyList,
        action: Action,
        labels: &mut HashMap<Action, String>,
    ) -> Result<()> {
        self.order.push(action);
        for name in &list.0 {
            let key = Key::parse(name).map_err(|e| anyhow::anyhow!("{table}.{setting}: {e}"))?;
            if let Some(other) = self.keys.insert(key, action) {
                bail!(
                    "key {name:?} is bound twice in {table} ({} and {})",
                    other.describe(),
                    action.describe()
                );
            }
            labels.entry(action).or_insert_with(|| key.label());
            self.labels.entry(action).or_insert_with(|| key.label());
        }
        Ok(())
    }
}

pub struct Keymap {
    top: Table,
    menus: HashMap<Menu, Table>,
    labels: HashMap<Action, String>,
    /// The back key while attached (bound in roster's tmux), for the help.
    pub detach: String,
}

impl Keymap {
    pub fn new(keys: &Keys) -> Result<Keymap> {
        let mut labels = HashMap::new();
        let mut top = Table::default();
        let t = "keys";
        top.bind(t, "up", &keys.up, Action::Up, &mut labels)?;
        top.bind(t, "down", &keys.down, Action::Down, &mut labels)?;
        top.bind(t, "collapse", &keys.collapse, Action::Collapse, &mut labels)?;
        top.bind(t, "expand", &keys.expand, Action::Expand, &mut labels)?;
        top.bind(t, "confirm", &keys.confirm, Action::Confirm, &mut labels)?;
        top.bind(
            "keys.session",
            "menu",
            &keys.session.menu,
            Action::Open(Menu::Session),
            &mut labels,
        )?;
        top.bind(
            "keys.project",
            "menu",
            &keys.project.menu,
            Action::Open(Menu::Project),
            &mut labels,
        )?;
        top.bind(
            "keys.view",
            "menu",
            &keys.view.menu,
            Action::Open(Menu::View),
            &mut labels,
        )?;
        top.bind(t, "help", &keys.help, Action::Help, &mut labels)?;
        top.bind(t, "quit", &keys.quit, Action::Quit, &mut labels)?;

        let mut session = Table::default();
        let (t, k) = ("keys.session", &keys.session);
        session.bind(t, "new", &k.new, Action::NewSession, &mut labels)?;
        session.bind(t, "chat", &k.chat, Action::NewChat, &mut labels)?;
        session.bind(t, "edit", &k.edit, Action::EditSession, &mut labels)?;
        session.bind(t, "start", &k.start, Action::Start, &mut labels)?;
        session.bind(t, "stop", &k.stop, Action::Stop, &mut labels)?;
        session.bind(t, "restart", &k.restart, Action::Restart, &mut labels)?;
        session.bind(
            t,
            "toggle_read",
            &k.toggle_read,
            Action::ToggleRead,
            &mut labels,
        )?;
        session.bind(t, "retitle", &k.retitle, Action::Retitle, &mut labels)?;
        session.bind(t, "adopt", &k.adopt, Action::Adopt, &mut labels)?;
        session.bind(t, "archive", &k.archive, Action::Archive, &mut labels)?;
        session.bind(t, "delete", &k.delete, Action::Delete, &mut labels)?;

        let mut project = Table::default();
        let (t, k) = ("keys.project", &keys.project);
        project.bind(t, "new", &k.new, Action::NewProject, &mut labels)?;
        project.bind(t, "edit", &k.edit, Action::EditProject, &mut labels)?;
        project.bind(t, "filter", &k.filter, Action::FilterProject, &mut labels)?;
        project.bind(t, "remove", &k.remove, Action::RemoveProject, &mut labels)?;
        project.bind(t, "list", &k.list, Action::ShowProjects, &mut labels)?;

        let mut view = Table::default();
        let (t, k) = ("keys.view", &keys.view);
        view.bind(t, "group", &k.group, Action::ToggleGroup, &mut labels)?;
        view.bind(
            t,
            "projects",
            &k.projects,
            Action::ShowProjects,
            &mut labels,
        )?;
        view.bind(t, "server", &k.server, Action::ShowServer, &mut labels)?;

        let menus = HashMap::from([
            (Menu::Session, session),
            (Menu::Project, project),
            (Menu::View, view),
        ]);
        Ok(Keymap {
            top,
            menus,
            labels,
            detach: keys.detach.clone(),
        })
    }

    /// The top-level action for a key press.
    pub fn top(&self, e: &KeyEvent) -> Option<Action> {
        self.top.keys.get(&Key::from_event(e)).copied()
    }

    /// The action for a key press inside an open menu.
    pub fn menu(&self, menu: Menu, e: &KeyEvent) -> Option<Action> {
        self.menus[&menu].keys.get(&Key::from_event(e)).copied()
    }

    /// The first key bound to `action`, if any.
    pub fn label(&self, action: Action) -> Option<&str> {
        self.labels.get(&action).map(String::as_str)
    }

    /// The first key bound to `action` inside `menu`, if any.
    pub fn menu_label(&self, menu: Menu, action: Action) -> Option<&str> {
        self.menus[&menu].labels.get(&action).map(String::as_str)
    }

    /// The action for a key press in a view about `menu`'s domain (the project
    /// menu in the projects view): top-level keys first, then the menu's, as if
    /// it were open.
    pub fn in_view(&self, menu: Menu, e: &KeyEvent) -> Option<Action> {
        self.top(e).or_else(|| self.menu(menu, e))
    }

    /// A key for `action` in `menu` that works without opening it there (see
    /// `in_view`): one the top level doesn't take, the menu's own first if it
    /// can.
    pub fn direct_label(&self, menu: Menu, action: Action) -> Option<String> {
        let mut free: Vec<String> = self.menus[&menu]
            .keys
            .iter()
            .filter(|(key, a)| **a == action && !self.top.keys.contains_key(key))
            .map(|(key, _)| key.label())
            .collect();
        free.sort();
        let first = self.menu_label(menu, action);
        match free.iter().position(|k| Some(k.as_str()) == first) {
            Some(i) => Some(free.swap_remove(i)),
            None => free.into_iter().next(),
        }
    }

    /// A menu's bound actions with their keys, in display order.
    pub fn menu_items(&self, menu: Menu) -> Vec<(&str, Action)> {
        self.menus[&menu].items()
    }

    /// The top level's bound actions with their keys, in display order.
    pub fn top_items(&self) -> Vec<(&str, Action)> {
        self.top.items()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn chr(c: char) -> KeyEvent {
        let shift = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        press(KeyCode::Char(c), shift)
    }

    #[test]
    fn parses_key_names() {
        assert_eq!(Key::parse("j").unwrap().label(), "j");
        assert_eq!(Key::parse("D").unwrap().label(), "D");
        assert_eq!(Key::parse("enter").unwrap().label(), "Enter");
        assert_eq!(Key::parse("C-C").unwrap().label(), "C-c");
        assert_eq!(Key::parse("M-x").unwrap().label(), "M-x");
        assert_eq!(Key::parse("F5").unwrap().label(), "F5");
        assert_eq!(Key::parse("-").unwrap().label(), "-");
        assert!(Key::parse("Nope").is_err());
        assert!(Key::parse("F13").is_err());
    }

    #[test]
    fn top_level_keys_and_menus() {
        let map = Keymap::new(&Keys::default()).unwrap();
        assert_eq!(map.top(&chr('j')), Some(Action::Down));
        assert_eq!(map.top(&chr('h')), Some(Action::Collapse));
        assert_eq!(map.top(&chr('s')), Some(Action::Open(Menu::Session)));
        assert_eq!(
            map.top(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(map.top(&chr('n')), None);
        // The same letter means the same in each menu.
        assert_eq!(map.menu(Menu::Session, &chr('n')), Some(Action::NewSession));
        assert_eq!(map.menu(Menu::Project, &chr('n')), Some(Action::NewProject));
        assert_eq!(map.menu(Menu::Session, &chr('d')), Some(Action::Archive));
        assert_eq!(map.menu(Menu::Session, &chr('D')), Some(Action::Delete));
        assert_eq!(
            map.menu(Menu::Project, &chr('d')),
            Some(Action::RemoveProject)
        );
        assert_eq!(map.menu(Menu::View, &chr('g')), Some(Action::ToggleGroup));
        // One action in two menus, under its key in each.
        assert_eq!(map.menu(Menu::View, &chr('p')), Some(Action::ShowProjects));
        assert_eq!(
            map.menu(Menu::Project, &chr('l')),
            Some(Action::ShowProjects)
        );
        assert_eq!(map.menu_label(Menu::View, Action::ShowProjects), Some("p"));
        assert_eq!(
            map.menu_label(Menu::Project, Action::ShowProjects),
            Some("l")
        );
        assert!(
            map.menu_items(Menu::View)
                .contains(&("p", Action::ShowProjects))
        );
        assert_eq!(Action::ShowProjects.describe_in(Menu::Project), "list");
        assert_eq!(Action::ShowProjects.describe_in(Menu::View), "projects");
        let items: Vec<&str> = map
            .menu_items(Menu::Session)
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(
            items,
            ["n", "c", "e", "s", "x", "r", "u", "t", "a", "d", "D"]
        );
    }

    #[test]
    fn a_views_menu_works_without_opening_it_unless_a_top_key_wins() {
        let map = Keymap::new(&Keys::default()).unwrap();
        assert_eq!(
            map.in_view(Menu::Project, &chr('e')),
            Some(Action::EditProject)
        );
        assert_eq!(map.in_view(Menu::Project, &chr('j')), Some(Action::Down));
        // "l" is unfold at the top level.
        assert_eq!(map.in_view(Menu::Project, &chr('l')), Some(Action::Expand));
        assert_eq!(
            map.direct_label(Menu::Project, Action::NewProject)
                .as_deref(),
            Some("n")
        );
        assert_eq!(map.direct_label(Menu::Project, Action::ShowProjects), None);
        // A project key rebound onto a top-level one: only through the menu.
        let mut keys = Keys::default();
        keys.project.edit = KeyList(vec!["j".into(), "E".into()]);
        let map = Keymap::new(&keys).unwrap();
        assert_eq!(map.in_view(Menu::Project, &chr('j')), Some(Action::Down));
        assert_eq!(
            map.direct_label(Menu::Project, Action::EditProject)
                .as_deref(),
            Some("E")
        );
    }

    #[test]
    fn rejects_conflicts_and_allows_disabling() {
        let mut keys = Keys::default();
        keys.session.stop = KeyList(vec!["d".into()]);
        assert!(Keymap::new(&keys).is_err());
        let mut keys = Keys::default();
        keys.view.menu = KeyList(vec!["s".into()]);
        assert!(Keymap::new(&keys).is_err());
        let mut keys = Keys::default();
        keys.session.archive = KeyList(vec![]);
        let map = Keymap::new(&keys).unwrap();
        assert_eq!(map.label(Action::Archive), None);
        assert_eq!(map.menu(Menu::Session, &chr('d')), None);
        assert!(
            !map.menu_items(Menu::Session)
                .iter()
                .any(|(_, a)| *a == Action::Archive)
        );
    }
}
