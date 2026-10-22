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
        /// Values Tab completes to (those holding what was typed), in order.
        choices: Vec<String>,
        /// What was typed before Tab went through the choices.
        typed: Option<String>,
    },
    Form(Box<Form>),
    /// Choosing a conversation to adopt.
    Picker(Picker),
}

#[derive(Clone, Debug)]
pub enum Purpose {
    /// The setting with this dotted name.
    Setting {
        setting: String,
    },
    /// A new agent's command line.
    NewAgent,
    RestoreName {
        id: String,
    },
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
                choices,
                mut typed,
            } => {
                if input::cancels(e) {
                    return None;
                }
                if e.code == KeyCode::Enter {
                    return self.submit(value, purpose);
                }
                if matches!(e.code, KeyCode::Tab | KeyCode::BackTab) {
                    // The next (or previous) of those holding what was typed.
                    let first = typed.is_none();
                    let stem = typed.get_or_insert_with(|| value.clone()).clone();
                    let matching = completions(&choices, &stem);
                    if !matching.is_empty() {
                        let at = if first {
                            None
                        } else {
                            matching.iter().position(|c| **c == value)
                        };
                        let n = matching.len();
                        let to = match (at, e.code == KeyCode::Tab) {
                            (None, true) => 0,
                            (None, false) => n - 1,
                            (Some(i), true) => (i + 1) % n,
                            (Some(i), false) => (i + n - 1) % n,
                        };
                        value = matching[to].clone();
                    }
                } else {
                    input::edit_line(&mut value, e);
                    typed = None;
                }
                self.dialog = Some(Dialog::Input {
                    prompt,
                    value,
                    purpose,
                    choices,
                    typed,
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
            Purpose::NewAgent if value.is_empty() => None,
            Purpose::NewAgent => Some(Op::AddAgent(value)),
            // Empty is a value too: text back to its default, no keys.
            Purpose::Setting { setting } => Some(Op::ChangeSetting {
                setting,
                input: Some(value),
            }),
        }
    }

    pub(super) fn input(&mut self, prompt: &str, value: String, purpose: Purpose) {
        self.input_with(prompt, value, purpose, Vec::new());
    }

    /// `input`, Tab completing to one of `choices`.
    pub(super) fn input_with(
        &mut self,
        prompt: &str,
        value: String,
        purpose: Purpose,
        choices: Vec<String>,
    ) {
        self.dialog = Some(Dialog::Input {
            prompt: prompt.to_string(),
            value,
            purpose,
            choices,
            typed: None,
        });
    }

    pub(super) fn confirm(&mut self, question: String, op: Op) {
        self.dialog = Some(Dialog::Confirm { question, op });
    }
}

/// Those of `choices` holding `typed` (in any case), in order.
pub fn completions<'a>(choices: &'a [String], typed: &str) -> Vec<&'a String> {
    let typed = typed.trim().to_lowercase();
    choices
        .iter()
        .filter(|c| c.to_lowercase().contains(&typed))
        .collect()
}
