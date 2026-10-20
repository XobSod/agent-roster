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
    EditSession,
    Start,
    Stop,
    Restart,
    Delete,
    ToggleRead,
    NewProject,
    EditProject,
    RemoveProject,
    FilterProject,
    ToggleGroup,
    ShowDeleted,
}

impl Action {
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
            Action::EditSession | Action::EditProject => "edit",
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Delete => "delete",
            Action::ToggleRead => "read/unread",
            Action::RemoveProject => "remove",
            Action::FilterProject => "show only this",
            Action::ToggleGroup => "group by project",
            Action::ShowDeleted => "deleted sessions",
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
}

impl Table {
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
        session.bind(t, "delete", &k.delete, Action::Delete, &mut labels)?;

        let mut project = Table::default();
        let (t, k) = ("keys.project", &keys.project);
        project.bind(t, "new", &k.new, Action::NewProject, &mut labels)?;
        project.bind(t, "edit", &k.edit, Action::EditProject, &mut labels)?;
        project.bind(t, "filter", &k.filter, Action::FilterProject, &mut labels)?;
        project.bind(t, "remove", &k.remove, Action::RemoveProject, &mut labels)?;

        let mut view = Table::default();
        let (t, k) = ("keys.view", &keys.view);
        view.bind(t, "group", &k.group, Action::ToggleGroup, &mut labels)?;
        view.bind(t, "deleted", &k.deleted, Action::ShowDeleted, &mut labels)?;

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

    /// A menu's bound actions with their keys, in display order.
    pub fn menu_items(&self, menu: Menu) -> Vec<(&str, Action)> {
        self.menus[&menu]
            .order
            .iter()
            .filter_map(|a| Some((self.label(*a)?, *a)))
            .collect()
    }

    /// The top level's bound actions with their keys, in display order.
    pub fn top_items(&self) -> Vec<(&str, Action)> {
        self.top
            .order
            .iter()
            .filter_map(|a| Some((self.label(*a)?, *a)))
            .collect()
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
        assert_eq!(map.menu(Menu::Session, &chr('d')), Some(Action::Delete));
        assert_eq!(
            map.menu(Menu::Project, &chr('d')),
            Some(Action::RemoveProject)
        );
        assert_eq!(map.menu(Menu::View, &chr('p')), Some(Action::ToggleGroup));
        let items: Vec<&str> = map
            .menu_items(Menu::Session)
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(items, ["n", "e", "s", "x", "r", "u", "d"]);
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
        keys.session.delete = KeyList(vec![]);
        let map = Keymap::new(&keys).unwrap();
        assert_eq!(map.label(Action::Delete), None);
        assert_eq!(map.menu(Menu::Session, &chr('d')), None);
        assert!(
            !map.menu_items(Menu::Session)
                .iter()
                .any(|(_, a)| *a == Action::Delete)
        );
    }
}
