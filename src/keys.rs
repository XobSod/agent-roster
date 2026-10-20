//! TUI key bindings: every action remappable or disabled in
//! `[keys]` of config.toml, with key names like `j`, `D`, `Enter`, `C-c`, `M-x`.

use std::collections::HashMap;

use anyhow::{Result, bail};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{KeyList, Keys};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Up,
    Down,
    Attach,
    New,
    AddProject,
    Start,
    Stop,
    Restart,
    Delete,
    ToggleRead,
    Deleted,
    Quit,
}

impl Action {
    /// In the order the footer shows them.
    pub const ALL: [Action; 12] = [
        Action::Attach,
        Action::New,
        Action::AddProject,
        Action::Start,
        Action::Stop,
        Action::Restart,
        Action::Delete,
        Action::ToggleRead,
        Action::Deleted,
        Action::Quit,
        Action::Up,
        Action::Down,
    ];

    /// The setting's name in `[keys]`.
    pub fn setting(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::Attach => "attach",
            Action::New => "new",
            Action::AddProject => "add_project",
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Delete => "delete",
            Action::ToggleRead => "toggle_read",
            Action::Deleted => "deleted",
            Action::Quit => "quit",
        }
    }

    fn keys(self, keys: &Keys) -> &KeyList {
        match self {
            Action::Up => &keys.up,
            Action::Down => &keys.down,
            Action::Attach => &keys.attach,
            Action::New => &keys.new,
            Action::AddProject => &keys.add_project,
            Action::Start => &keys.start,
            Action::Stop => &keys.stop,
            Action::Restart => &keys.restart,
            Action::Delete => &keys.delete,
            Action::ToggleRead => &keys.toggle_read,
            Action::Deleted => &keys.deleted,
            Action::Quit => &keys.quit,
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

    /// How the footer shows the key.
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

pub struct Keymap {
    actions: HashMap<Key, Action>,
    labels: HashMap<Action, String>,
}

impl Keymap {
    pub fn new(keys: &Keys) -> Result<Keymap> {
        let mut actions = HashMap::new();
        let mut labels = HashMap::new();
        for action in Action::ALL {
            for name in &action.keys(keys).0 {
                let key = Key::parse(name)
                    .map_err(|e| anyhow::anyhow!("keys.{}: {e}", action.setting()))?;
                if let Some(other) = actions.insert(key, action) {
                    bail!(
                        "key {name:?} is bound to both keys.{} and keys.{}",
                        other.setting(),
                        action.setting()
                    );
                }
                labels.entry(action).or_insert_with(|| key.label());
            }
        }
        Ok(Keymap { actions, labels })
    }

    pub fn action(&self, e: &KeyEvent) -> Option<Action> {
        self.actions.get(&Key::from_event(e)).copied()
    }

    /// The first key bound to `action`, if any.
    pub fn label(&self, action: Action) -> Option<&str> {
        self.labels.get(&action).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
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
    fn matches_presses_like_terminals_send_them() {
        let map = Keymap::new(&Keys::default()).unwrap();
        // Shift+d arrives as 'D' with SHIFT; the character already carries the case.
        assert_eq!(
            map.action(&press(KeyCode::Char('D'), KeyModifiers::SHIFT)),
            Some(Action::Delete)
        );
        assert_eq!(
            map.action(&press(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Action::Stop)
        );
        assert_eq!(
            map.action(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(
            map.action(&press(KeyCode::Down, KeyModifiers::NONE)),
            Some(Action::Down)
        );
        assert_eq!(
            map.action(&press(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn rejects_conflicts_and_allows_disabling() {
        let keys = Keys {
            stop: KeyList(vec!["D".into()]),
            ..Keys::default()
        };
        assert!(Keymap::new(&keys).is_err());
        let keys = Keys {
            delete: KeyList(vec![]),
            ..Keys::default()
        };
        let map = Keymap::new(&keys).unwrap();
        assert_eq!(map.label(Action::Delete), None);
        assert_eq!(
            map.action(&press(KeyCode::Char('D'), KeyModifiers::SHIFT)),
            None
        );
    }
}
