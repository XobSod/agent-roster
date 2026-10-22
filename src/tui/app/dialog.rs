//! Dialogs: a question, a line of input, a form or the conversation picker,
//! each taking the keys while it's open.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;

pub enum Dialog {
    Confirm {
        question: String,
        op: Op,
    },
    Input {
        prompt: String,
        value: String,
        purpose: Purpose,
    },
    Form(Form),
    /// Choosing a conversation to adopt.
    Picker(Picker),
}

#[derive(Clone, Debug)]
pub enum Purpose {
    RestoreName { id: String },
}

impl App {
    pub(super) fn on_dialog_key(&mut self, e: KeyEvent) -> Option<Op> {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        match self.dialog.take()? {
            Dialog::Form(mut form) => match form.on_key(e) {
                Outcome::Continue => {
                    self.dialog = Some(Dialog::Form(form));
                    None
                }
                Outcome::Cancel => None,
                // Two Claudes would write the same conversation: ask.
                Outcome::Submit(op @ Op::Adopt { fork: false, .. })
                    if let form::Purpose::Adopt {
                        open_in: Some(pid), ..
                    } = form.purpose =>
                {
                    let agent = crate::util::capitalize(form.agent());
                    self.confirm(
                        format!(
                            "It's open in {agent} (pid {pid}): two would write one conversation. Adopt it anyway?"
                        ),
                        op,
                    );
                    None
                }
                Outcome::Submit(op) => Some(op),
            },
            Dialog::Picker(mut picker) => match picker.on_key(e) {
                Picked::Continue => {
                    self.dialog = Some(Dialog::Picker(picker));
                    None
                }
                Picked::Cancel => None,
                Picked::Pick(a) => {
                    self.adopt(*a);
                    None
                }
            },
            Dialog::Confirm { question, op } => match e.code {
                KeyCode::Char('y' | 'Y') => Some(op),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                KeyCode::Char('c') if ctrl => None,
                _ => {
                    self.dialog = Some(Dialog::Confirm { question, op });
                    None
                }
            },
            Dialog::Input {
                prompt,
                mut value,
                purpose,
            } => {
                if input::cancels(e) {
                    return None;
                }
                if e.code == KeyCode::Enter {
                    return self.submit(value, purpose);
                }
                input::edit_line(&mut value, e);
                self.dialog = Some(Dialog::Input {
                    prompt,
                    value,
                    purpose,
                });
                None
            }
        }
    }

    pub(super) fn submit(&mut self, value: String, purpose: Purpose) -> Option<Op> {
        let value = value.trim().to_string();
        match purpose {
            Purpose::RestoreName { .. } if value.is_empty() => None,
            Purpose::RestoreName { id } => Some(Op::Restore {
                id,
                name: Some(value),
            }),
        }
    }

    pub(super) fn input(&mut self, prompt: &str, value: String, purpose: Purpose) {
        self.dialog = Some(Dialog::Input {
            prompt: prompt.to_string(),
            value,
            purpose,
        });
    }

    pub(super) fn confirm(&mut self, question: String, op: Op) {
        self.dialog = Some(Dialog::Confirm { question, op });
    }
}
