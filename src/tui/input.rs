//! Keys shared by everything you type into: the filter, dialogs, the picker
//! and forms.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Esc or Ctrl+C: leave without doing anything.
pub fn cancels(e: KeyEvent) -> bool {
    e.code == KeyCode::Esc || (e.code == KeyCode::Char('c') && ctrl(e))
}

/// ↑ or ↓, also as Ctrl+P / Ctrl+K and Ctrl+N / Ctrl+J: Some(true) for down.
pub fn vertical(e: KeyEvent) -> Option<bool> {
    match e.code {
        KeyCode::Up => Some(false),
        KeyCode::Char('p' | 'k') if ctrl(e) => Some(false),
        KeyCode::Down => Some(true),
        KeyCode::Char('n' | 'j') if ctrl(e) => Some(true),
        _ => None,
    }
}

/// Edits `text` as a line of input: a character, Backspace, Ctrl+U (all of
/// it), Ctrl+W (the last word). Whether the key was one of those.
pub fn edit_line(text: &mut String, e: KeyEvent) -> bool {
    let alt = e.modifiers.contains(KeyModifiers::ALT);
    match e.code {
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char('u') if ctrl(e) => text.clear(),
        KeyCode::Char('w') if ctrl(e) => {
            let kept = text.trim_end().rfind(' ').map_or(0, |i| i + 1);
            text.truncate(kept);
        }
        KeyCode::Char(c) if !ctrl(e) && !alt => text.push(c),
        _ => return false,
    }
    true
}

fn ctrl(e: KeyEvent) -> bool {
    e.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn edits_a_line() {
        let mut text = String::from("fix the parser");
        assert!(edit_line(
            &mut text,
            key(KeyCode::Char('w'), KeyModifiers::CONTROL)
        ));
        assert_eq!(text, "fix the ");
        assert!(edit_line(
            &mut text,
            key(KeyCode::Char('x'), KeyModifiers::NONE)
        ));
        assert!(edit_line(
            &mut text,
            key(KeyCode::Backspace, KeyModifiers::NONE)
        ));
        assert_eq!(text, "fix the ");
        assert!(!edit_line(
            &mut text,
            key(KeyCode::Char('x'), KeyModifiers::ALT)
        ));
        assert!(!edit_line(
            &mut text,
            key(KeyCode::Enter, KeyModifiers::NONE)
        ));
        assert!(edit_line(
            &mut text,
            key(KeyCode::Char('u'), KeyModifiers::CONTROL)
        ));
        assert_eq!(text, "");
        assert!(cancels(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert_eq!(
            vertical(key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(true)
        );
        assert_eq!(vertical(key(KeyCode::Char('j'), KeyModifiers::NONE)), None);
    }
}
