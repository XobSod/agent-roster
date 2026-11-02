//! A session's queue (`s q`): the prompts sent to it, waiting to be typed
//! into its agent, one a row, each read whole, edited or dropped. What's
//! typed in meanwhile goes from it as the list is read again.

use std::cell::Cell;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::input;
use super::note::NoteEditor;
use crate::model::Queued;

pub struct QueueView {
    /// Whose queue it is: the session's id, and what to call it.
    pub session: String,
    pub label: String,
    /// The row the cursor's on, and the prompt there by its id, so the
    /// cursor stays on it as those ahead of it go in.
    pub row: usize,
    selected: Option<i64>,
    /// The selected prompt shown whole, scrolled down this many lines, and
    /// how far it can be (as last drawn).
    pub reading: Option<usize>,
    pub reading_max: Cell<usize>,
    /// The prompt being edited, by its id and number, in the editor.
    pub editing: Option<Editing>,
}

pub struct Editing {
    pub id: i64,
    pub n: usize,
    pub editor: NoteEditor,
    /// The text it was opened on, or last saved as: what a save replaces,
    /// unless another changed it meanwhile.
    pub base: String,
}

/// The start of `prompt`, quoted, on one line: which prompt, to say.
pub fn quoted(prompt: &str) -> String {
    let start = super::ui::truncate(&crate::util::one_line(prompt), 30);
    format!("\"{start}\"")
}

/// What a key asks of the app.
#[derive(Debug, PartialEq)]
pub enum Asked {
    None,
    Close,
    /// Edit the selected prompt.
    Edit,
    /// Drop the selected prompt, or all.
    Drop,
    DropAll,
}

impl QueueView {
    pub fn new(session: String, label: String) -> QueueView {
        QueueView {
            session,
            label,
            row: 0,
            selected: None,
            reading: None,
            reading_max: Cell::new(0),
            editing: None,
        }
    }

    /// The cursor on its prompt in `queue` as it is now, else on the row
    /// it was; reading one that's gone, back to the list. The prompt being
    /// edited numbered as it is now, while it's there.
    pub fn follow(&mut self, queue: &[Queued]) {
        let at = self
            .selected
            .and_then(|id| queue.iter().position(|q| q.id == id));
        if at.is_none() {
            self.reading = None;
        }
        self.row = at.unwrap_or(self.row).min(queue.len().saturating_sub(1));
        self.selected = queue.get(self.row).map(|q| q.id);
        if let Some(editing) = self.editing.as_mut()
            && let Some(i) = queue.iter().position(|q| q.id == editing.id)
        {
            editing.n = i + 1;
        }
    }

    /// The prompt the cursor's on, and its number, in `queue`.
    pub fn selected<'a>(&self, queue: &'a [Queued]) -> Option<(usize, &'a Queued)> {
        queue.get(self.row).map(|q| (self.row + 1, q))
    }

    /// A key, while the editor isn't open, on `queue`.
    pub fn on_key(&mut self, e: KeyEvent, queue: &[Queued]) -> Asked {
        let down = match (e.code, input::vertical(e)) {
            (_, Some(down)) => Some(down),
            (KeyCode::Char('j'), _) => Some(true),
            (KeyCode::Char('k'), _) => Some(false),
            _ => None,
        };
        match (e.code, down) {
            (KeyCode::Char('e'), _) => return Asked::Edit,
            (KeyCode::Char('d'), _) => return Asked::Drop,
            (KeyCode::Char('D'), _) => return Asked::DropAll,
            // Reading: up and down scroll, and leaving it is back to the list.
            (_, Some(down)) if self.reading.is_some() => {
                // Drawn shorter since, it's no further than it can be.
                let scroll = self.reading.unwrap_or(0).min(self.reading_max.get());
                self.reading = Some(match down {
                    true => (scroll + 1).min(self.reading_max.get()),
                    false => scroll.saturating_sub(1),
                });
            }
            (KeyCode::Enter | KeyCode::Char('q'), _) if self.reading.is_some() => {
                self.reading = None
            }
            _ if input::cancels(e) && self.reading.is_some() => self.reading = None,
            (KeyCode::Char('q'), _) => return Asked::Close,
            _ if input::cancels(e) => return Asked::Close,
            (KeyCode::Enter, _) => self.reading = Some(0),
            // Around, as every list does.
            (_, Some(down)) => {
                self.row = super::app::wrap(self.row, queue.len(), down);
                self.selected = queue.get(self.row).map(|q| q.id);
            }
            _ => {}
        }
        Asked::None
    }

    /// What the keys do, for the footer.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        if let Some(editing) = &self.editing {
            return editing.editor.hints();
        }
        let hints: &[(&str, &'static str)] = match self.reading {
            Some(_) => &[
                ("j/k", "scroll"),
                ("e", "edit"),
                ("d", "drop"),
                ("Esc", "back"),
            ],
            None => &[
                ("j/k", "move"),
                ("Enter", "read"),
                ("e", "edit"),
                ("d", "drop"),
                ("D", "drop all"),
                ("Esc", "close"),
            ],
        };
        hints
            .iter()
            .map(|(k, what)| (k.to_string(), *what))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn queued(id: i64, prompt: &str) -> Queued {
        Queued {
            id,
            prompt: prompt.into(),
            sender: None,
            sender_label: None,
            sent_at: 0,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_cursor_stays_on_its_prompt_as_those_ahead_go_in() {
        let queue = vec![queued(1, "one"), queued(2, "two"), queued(3, "three")];
        let mut view = QueueView::new("w1".into(), "demo/w1".into());
        view.follow(&queue);
        assert_eq!(view.on_key(key(KeyCode::Char('j')), &queue), Asked::None);
        let at = |view: &QueueView, queue: &[Queued]| view.selected(queue).map(|(n, q)| (n, q.id));
        assert_eq!(at(&view, &queue), Some((2, 2)));
        // Around, as every list does.
        view.on_key(key(KeyCode::Up), &queue);
        view.on_key(key(KeyCode::Up), &queue);
        assert_eq!(at(&view, &queue), Some((3, 3)));
        view.on_key(key(KeyCode::Down), &queue);
        view.on_key(key(KeyCode::Down), &queue);
        // The first typed in: still on two, now first.
        view.follow(&queue[1..]);
        assert_eq!(at(&view, &queue[1..]), Some((1, 2)));
        // Two gone while it's read: back to the list, on the one after it.
        view.on_key(key(KeyCode::Enter), &queue[1..]);
        assert_eq!(view.reading, Some(0));
        view.follow(&queue[2..]);
        assert_eq!((at(&view, &queue[2..]), view.reading), (Some((1, 3)), None));
        // Those after it gone: on the row it was, as far as there are.
        view.follow(&queue);
        view.on_key(key(KeyCode::Char('k')), &queue);
        assert_eq!(at(&view, &queue), Some((2, 2)));
        view.follow(&queue[..1]);
        assert_eq!(at(&view, &queue[..1]), Some((1, 1)));
        // The one being edited, numbered as it is now.
        let editor = NoteEditor::prompt("w1", "demo/w1".into(), "three", None);
        view.editing = Some(Editing {
            base: "three".into(),
            id: 3,
            n: 3,
            editor,
        });
        view.follow(&queue[1..]);
        assert_eq!(view.editing.as_ref().map(|e| e.n), Some(2));
    }

    #[test]
    fn reading_a_prompt_scrolls_and_goes_back_to_the_list() {
        let queue = vec![queued(1, "one"), queued(2, "two")];
        let mut view = QueueView::new("w1".into(), "demo/w1".into());
        view.follow(&queue);
        view.on_key(key(KeyCode::Enter), &queue);
        // As far as it goes, as last drawn.
        view.reading_max.set(2);
        for _ in 0..3 {
            view.on_key(key(KeyCode::Char('j')), &queue);
        }
        view.on_key(key(KeyCode::Char('k')), &queue);
        assert_eq!((view.reading, view.row), (Some(1), 0));
        // Drawn taller since, up goes from as far as it can be now.
        view.reading = Some(2);
        view.reading_max.set(0);
        view.on_key(key(KeyCode::Char('k')), &queue);
        assert_eq!(view.reading, Some(0));
        (view.reading, view.reading_max) = (Some(1), Cell::new(2));
        assert_eq!(view.on_key(key(KeyCode::Char('e')), &queue), Asked::Edit);
        assert_eq!(view.on_key(key(KeyCode::Char('d')), &queue), Asked::Drop);
        assert_eq!(view.on_key(key(KeyCode::Esc), &queue), Asked::None);
        assert_eq!(view.reading, None);
        view.on_key(key(KeyCode::Enter), &queue);
        view.on_key(key(KeyCode::Char('q')), &queue);
        assert_eq!(view.reading, None);
        assert_eq!(view.on_key(key(KeyCode::Char('q')), &queue), Asked::Close);
        assert_eq!(view.on_key(key(KeyCode::Esc), &queue), Asked::Close);
        assert_eq!(
            view.on_key(
                KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT),
                &queue
            ),
            Asked::DropAll
        );
    }
}
