//! Picking a conversation to adopt: typing filters, ↑/↓ move, Tab shows those
//! outside your projects too, Enter picks.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::input;

use crate::adopt::Adoptable;

pub struct Picker {
    items: Vec<Adoptable>,
    /// What typing matches in each item, made once: the list is filtered on
    /// every frame.
    haystacks: Vec<String>,
    pub query: String,
    /// Also those outside your projects' directories.
    pub all: bool,
    pub selected: usize,
}

pub enum Picked {
    Continue,
    Cancel,
    Pick(Box<Adoptable>),
}

impl Picker {
    /// Shows those in your projects; all of them if none are.
    pub fn new(items: Vec<Adoptable>) -> Picker {
        let all = !items.iter().any(|a| a.project.is_some());
        Picker {
            haystacks: items.iter().map(haystack).collect(),
            items,
            query: String::new(),
            all,
            selected: 0,
        }
    }

    /// The conversations shown, newest first.
    pub fn shown(&self) -> Vec<&Adoptable> {
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.items
            .iter()
            .zip(&self.haystacks)
            .filter(|(a, text)| {
                (self.all || a.project.is_some()) && words.iter().all(|w| text.contains(w))
            })
            .map(|(a, _)| a)
            .collect()
    }

    /// How many are outside your projects, hidden until Tab.
    pub fn outside(&self) -> usize {
        self.items.iter().filter(|a| a.project.is_none()).count()
    }

    pub fn on_key(&mut self, e: KeyEvent) -> Picked {
        if input::cancels(e) {
            return Picked::Cancel;
        }
        match (e.code, input::vertical(e)) {
            (KeyCode::Enter, _) => {
                if let Some(a) = self.shown().get(self.selected) {
                    return Picked::Pick(Box::new((*a).clone()));
                }
            }
            (_, Some(false)) => self.selected = self.selected.saturating_sub(1),
            (_, Some(true)) => self.selected += 1,
            (KeyCode::Tab | KeyCode::BackTab, _) => {
                self.all = !self.all;
                self.selected = 0;
            }
            _ => {
                if input::edit_line(&mut self.query, e) {
                    self.selected = 0;
                }
            }
        }
        self.selected = self.selected.min(self.shown().len().saturating_sub(1));
        Picked::Continue
    }
}

/// What typing matches: title, last prompt, id, directory, project.
fn haystack(a: &Adoptable) -> String {
    let c = &a.conversation;
    let project = a
        .project
        .as_ref()
        .map_or(a.new_project.as_str(), |p| p.name.as_str());
    [
        c.custom_title.as_deref().unwrap_or(""),
        c.title.as_deref().unwrap_or(""),
        c.first_prompt.as_deref().unwrap_or(""),
        c.last_prompt.as_deref().unwrap_or(""),
        &c.id,
        &c.cwd.to_string_lossy(),
        project,
    ]
    .join(" ")
    .to_lowercase()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::agent::claude::conversations::Conversation;
    use ratatui::crossterm::event::KeyModifiers;

    pub fn adoptable(id: &str, title: &str, project: Option<&str>) -> Adoptable {
        Adoptable {
            conversation: Conversation {
                id: id.into(),
                path: format!("/c/{id}.jsonl").into(),
                cwd: format!("/{}", project.unwrap_or("elsewhere")).into(),
                branch: None,
                custom_title: None,
                title: Some(title.into()),
                first_prompt: None,
                last_prompt: None,
                model: None,
                modified: 0,
                open_in: None,
            },
            project: project.map(|name| crate::model::Project {
                id: 1,
                name: name.into(),
                origin: format!("/{name}").into(),
                agent: None,
                model: None,
                effort: None,
            }),
            new_project: "elsewhere".into(),
            archived: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn filters_by_typing_and_shows_the_rest_with_tab() {
        let mut p = Picker::new(vec![
            adoptable("c1", "Fix the parser", Some("alpha")),
            adoptable("c2", "Elsewhere work", None),
            adoptable("c3", "Parser tests", Some("alpha")),
        ]);
        let ids = |p: &Picker| -> Vec<String> {
            p.shown()
                .iter()
                .map(|a| a.conversation.id.clone())
                .collect()
        };
        assert_eq!((ids(&p), p.outside()), (vec!["c1".into(), "c3".into()], 1));
        for c in "parser tes".chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(ids(&p), ["c3"]);
        p.on_key(key(KeyCode::Char('x')));
        assert!(ids(&p).is_empty());
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Picked::Continue));
        p.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        p.on_key(key(KeyCode::Tab));
        assert_eq!(ids(&p), ["c1", "c2", "c3"]);
        p.on_key(key(KeyCode::Down));
        p.on_key(key(KeyCode::Down));
        p.on_key(key(KeyCode::Down));
        assert_eq!(p.selected, 2);
        let Picked::Pick(a) = p.on_key(key(KeyCode::Enter)) else {
            panic!("nothing picked")
        };
        assert_eq!(a.conversation.id, "c3");
        assert!(matches!(p.on_key(key(KeyCode::Esc)), Picked::Cancel));
    }
}
