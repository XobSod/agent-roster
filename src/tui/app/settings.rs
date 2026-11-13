//! The settings view: every setting of config.toml, to see and change. Enter
//! changes the selected one (on/off, the next choice, a value typed, or for a
//! key the next one pressed), ←/→ go through its choices, `a` adds a key,
//! `d` puts its default back; `n` adds an agent, `x` takes an agent's settings
//! out; `e` opens config.toml in `$EDITOR`.

use super::*;
use crate::settings::{Kind, Setting};

/// A key setting waiting for a key: to be it, or (`add`) one more of its keys.
#[derive(Clone, Debug)]
pub struct Capture {
    pub setting: String,
    pub add: bool,
}

impl App {
    /// Shows `sections`, keeping the selected setting where it was: the CLI
    /// Proxy's with rows of the view's own, how it stands and its accounts.
    pub fn set_settings(&mut self, mut sections: Vec<crate::settings::Section>) {
        let kept = self.selected_setting().map(Setting::dotted);
        let proxy = ["cli_proxy".to_string()];
        if let Some(section) =
            (sections.iter_mut()).find(|s| s.table.as_deref() == Some(&proxy[..]))
        {
            section.settings.extend(self.proxy_rows());
        }
        self.settings = sections;
        let all = self.all_settings();
        self.setting_row = kept
            .and_then(|k| all.iter().position(|s| s.dotted() == k))
            .unwrap_or(self.setting_row)
            .min(all.len().saturating_sub(1));
    }

    /// Every setting, in the view's order.
    pub fn all_settings(&self) -> Vec<&Setting> {
        self.settings.iter().flat_map(|s| &s.settings).collect()
    }

    pub fn selected_setting(&self) -> Option<&Setting> {
        self.all_settings().get(self.setting_row).copied()
    }

    /// The view's own keys, before the keymap's: Some if `e` was one. Keys
    /// you bound to move or change (`keys.up = "e"`) stay yours.
    pub(super) fn on_settings_key(&mut self, e: &KeyEvent) -> Option<Option<Op>> {
        if !e.modifiers.is_empty() && e.modifiers != ratatui::crossterm::event::KeyModifiers::SHIFT
        {
            return None;
        }
        if matches!(
            self.keymap.top(e),
            Some(Action::Up | Action::Down | Action::Confirm | Action::Collapse | Action::Expand)
        ) {
            return None;
        }
        match e.code {
            KeyCode::Char('e') => Some(Some(Op::EditConfig)),
            KeyCode::Char('n') => {
                let prompt =
                    "A new agent: its command, and arguments if any (aider, gemini --yolo):";
                self.input(prompt, String::new(), dialog::Purpose::NewAgent);
                Some(None)
            }
            KeyCode::Char('x') => {
                let name = self.selected_agent()?;
                let question = crate::settings::agent_removal_question(&name);
                let setting = format!("agents.{name}");
                self.confirm(
                    question,
                    Op::ChangeSetting {
                        setting,
                        input: None,
                    },
                );
                Some(None)
            }
            KeyCode::Char('a') => {
                let s = self.selected_setting()?;
                if s.kind != Kind::Keys {
                    return None;
                }
                self.capturing = Some(Capture {
                    setting: s.dotted(),
                    add: true,
                });
                Some(None)
            }
            KeyCode::Char('d') => {
                let s = self.selected_setting()?;
                // A row of the view's own: a label to take out, or nothing.
                match &s.kind {
                    Kind::Status => return Some(None),
                    Kind::Label { provider, email } if s.set => {
                        return Some(Some(Op::SetProxyLabel {
                            provider: provider.clone(),
                            email: email.clone(),
                            label: None,
                        }));
                    }
                    Kind::Label { .. } => {
                        self.flash("It has no label");
                        return Some(None);
                    }
                    _ => {}
                }
                if !s.set {
                    self.flash(format!("{} is already the default", s.name()));
                    return Some(None);
                }
                Some(Some(Op::ChangeSetting {
                    setting: s.dotted(),
                    input: None,
                }))
            }
            _ => None,
        }
    }

    /// In the settings view: move, change the selected setting, or leave.
    pub(super) fn act_in_settings_view(&mut self, action: Action) -> Option<Op> {
        let count = self.all_settings().len();
        match action {
            Action::Up => self.setting_row = wrap(self.setting_row, count, false),
            Action::Down => self.setting_row = wrap(self.setting_row, count, true),
            Action::Confirm => return self.change_selected(true, true),
            Action::Collapse => return self.change_selected(false, false),
            Action::Expand => return self.change_selected(true, false),
            _ => self.fail("This is the settings view: Esc goes back to the sessions"),
        }
        None
    }

    /// How the CLI Proxy stands, then each account it listed last and each
    /// labelled that it didn't, by service then name: rows of the settings
    /// view's own, not settings.
    fn proxy_rows(&self) -> Vec<Setting> {
        use crate::usage::Via;
        let row = |path: &[&str], kind: Kind, value: &str, set: bool, doc: String| Setting {
            path: path.iter().map(|p| p.to_string()).collect(),
            kind,
            value: value.to_string(),
            set,
            input: String::new(),
            placeholder: None,
            doc,
        };
        let said: Vec<_> = (self.limits.agents.iter())
            .filter_map(|s| Some((s.account.as_ref()?, s)))
            .collect();
        let again = "; Enter asks it again";
        let (value, doc) = match (&self.proxy_url, &self.limits.via) {
            _ if !self.account_limits => (
                "not asked",
                "account_limits is off: neither it nor the agents are asked".to_string(),
            ),
            (None, _) => (
                "off",
                "enabled, at its url, with its key set, it's asked for every account it holds"
                    .to_string(),
            ),
            (Some(url), Some(Via::Proxy(_))) => {
                let n = said.len();
                let accounts = if n == 1 { "account" } else { "accounts" };
                ("connected", format!("{url}: {n} {accounts}{again}"))
            }
            (Some(url), Some(via)) if via.why().is_some() => (
                "can't be asked",
                format!("{url}: {}{again}", via.why().unwrap_or_default()),
            ),
            (Some(url), _) if self.limits.asking() => ("asking…", url.clone()),
            (Some(url), _) => ("not asked yet", format!("{url}{again}")),
        };
        let mut rows = vec![row(
            &["cli_proxy", "status"],
            Kind::Status,
            value,
            false,
            doc,
        )];
        let accounts = said.iter().map(|(a, _)| *a);
        for r in crate::proxy::rows(accounts, &self.cli_proxy.labels) {
            let mut doc = vec![
                r.email
                    .clone()
                    .unwrap_or_else(|| "no email: it can't be labelled".into()),
            ];
            let said = r.account.and_then(|a| said.iter().find(|(b, _)| a.same(b)));
            if let Some(plan) = said.and_then(|(_, s)| s.usage.as_ref()?.plan.clone()) {
                doc.push(plan);
            }
            match r.account {
                Some(a) if a.disabled => doc.push("disabled".into()),
                Some(a) if a.sign_in => doc.push("needs signing in again".into()),
                Some(_) => {}
                None => doc.push("not listed by the proxy now".into()),
            }
            let id = r.email.clone().or_else(|| r.account.map(|a| a.id.clone()));
            let kind = Kind::Label {
                provider: r.provider.clone(),
                email: r.email.clone().unwrap_or_default(),
            };
            rows.push(row(
                &["cli_proxy", "account", &id.unwrap_or_default(), &r.provider],
                kind,
                r.label.as_deref().unwrap_or("no label"),
                r.label.is_some(),
                doc.join(" · "),
            ));
        }
        rows
    }

    /// The agent whose settings the selected one is.
    pub fn selected_agent(&self) -> Option<String> {
        match self.selected_setting()?.path.as_slice() {
            [agents, name, _] if agents == "agents" => Some(name.clone()),
            _ => None,
        }
    }

    /// A key pressed for a key setting waiting for one: it, or one more; Esc
    /// leaves it as it was, Backspace leaves none (the back key, which can't
    /// be none, is left as it was by Backspace and C-c too).
    pub(super) fn on_captured_key(&mut self, capture: Capture, e: &KeyEvent) -> Option<Op> {
        let kind = self
            .all_settings()
            .into_iter()
            .find(|s| s.dotted() == capture.setting)
            .map(|s| (s.kind.clone(), s.input.clone()));
        let (kind, current) = kind?;
        let key = crate::keys::Key::from_event(e);
        let back_key = kind == Kind::TmuxKey;
        let ctrl_c = e.code == KeyCode::Char('c')
            && e.modifiers
                .contains(ratatui::crossterm::event::KeyModifiers::CONTROL);
        if e.code == KeyCode::Esc || (back_key && (e.code == KeyCode::Backspace || ctrl_c)) {
            return None;
        }
        let name = if back_key {
            key.tmux_name()
        } else {
            key.name()
        };
        let Some(name) = name else {
            self.capturing = Some(capture);
            self.fail("roster has no name for that key: press another, or Esc");
            return None;
        };
        let input = match e.code {
            KeyCode::Backspace if kind == Kind::Keys => String::new(),
            _ if capture.add && current.split_whitespace().any(|k| k == name) => {
                self.flash(format!("{name} is one of its keys already"));
                return None;
            }
            _ if capture.add && !current.is_empty() => format!("{current} {name}"),
            _ => name,
        };
        Some(Op::ChangeSetting {
            setting: capture.setting,
            input: Some(input),
        })
    }

    /// The models a model setting offers: the agent's own default, then
    /// those the agent has (as the session forms offer them).
    pub fn model_choices(&self, s: &Setting) -> Vec<String> {
        let agent = match s.path.as_slice() {
            [agents, name, _] if agents == "agents" => name.as_str(),
            _ => "",
        };
        let mut choices = vec!["default".to_string()];
        if let Some(models) = self.models.offered.get(agent) {
            choices.extend(models.iter().map(|m| m.id.clone()));
        }
        choices
    }

    /// The selected setting's next (or previous) choice; with `typing`, a
    /// line to type its value in, or for a key, a key to press.
    fn change_selected(&mut self, next: bool, typing: bool) -> Option<Op> {
        let s = self.selected_setting()?.clone();
        let choices: Vec<String> = match &s.kind {
            Kind::Bool => vec!["on".into(), "off".into()],
            Kind::Choice(options) => options.clone(),
            Kind::Model if typing => {
                let prompt = format!("{} ({}):", s.dotted(), typing_hint(&s.kind));
                let mut models = self.model_choices(&s);
                models.remove(0);
                self.input_with(
                    &prompt,
                    s.input.clone(),
                    dialog::Purpose::Setting {
                        setting: s.dotted(),
                    },
                    models,
                );
                return None;
            }
            Kind::Model => {
                let mut choices = self.model_choices(&s);
                // One typed that the agent doesn't list: where it is.
                if !s.input.is_empty() && !choices.contains(&s.input) {
                    choices.insert(1, s.input.clone());
                }
                choices
            }
            Kind::Keys | Kind::TmuxKey if typing => {
                self.capturing = Some(Capture {
                    setting: s.dotted(),
                    add: false,
                });
                return None;
            }
            // Asked again, as Enter in the usage view does.
            Kind::Status if typing => {
                if !self.account_limits {
                    self.fail(crate::usage::OFF);
                } else if self.proxy_url.is_none() {
                    self.fail("The CLI Proxy isn't on: enabled, at its url, with its key set");
                } else if self.limits.asking() {
                    self.flash("Asking it already");
                } else {
                    self.limits.want(true);
                }
                return None;
            }
            Kind::Label { email, .. } if typing && email.is_empty() => {
                self.fail("It has no email in the proxy's list, which labels are kept by");
                return None;
            }
            Kind::Label { provider, email } if typing => {
                let prompt = format!("{provider}'s {email}, called (empty for its email):");
                let label = if s.set {
                    s.value.clone()
                } else {
                    String::new()
                };
                self.input(
                    &prompt,
                    label,
                    dialog::Purpose::Label {
                        provider: provider.clone(),
                        email: email.clone(),
                    },
                );
                return None;
            }
            // Typed afresh, never shown.
            Kind::Secret if typing => {
                let prompt = format!("{} ({}):", s.dotted(), typing_hint(&s.kind));
                self.input(
                    &prompt,
                    String::new(),
                    dialog::Purpose::Secret {
                        setting: s.dotted(),
                    },
                );
                return None;
            }
            _ if typing => {
                let prompt = format!("{} ({}):", s.dotted(), typing_hint(&s.kind));
                self.input(
                    &prompt,
                    s.input.clone(),
                    dialog::Purpose::Setting {
                        setting: s.dotted(),
                    },
                );
                return None;
            }
            _ => return None,
        };
        let current = if s.input.is_empty() {
            "default"
        } else {
            &s.input
        };
        let i = choices.iter().position(|c| c == current).unwrap_or(0);
        let n = choices.len();
        let to = if next { (i + 1) % n } else { (i + n - 1) % n };
        Some(Op::ChangeSetting {
            setting: s.dotted(),
            input: Some(choices[to].clone()),
        })
    }
}

/// How a value of `kind` is typed, for the prompt.
fn typing_hint(kind: &Kind) -> &'static str {
    match kind {
        Kind::Text => "empty for the default",
        Kind::Model => "Tab completes from the agent's; empty for its default",
        Kind::Words => "words as in a shell; empty for none",
        Kind::Seconds => "seconds",
        Kind::Millis => "milliseconds",
        Kind::Lines => "a number of lines",
        Kind::Prompts => "a number of prompts",
        Kind::Keys => "key names, e.g. k Up or C-x; empty for none",
        Kind::TmuxKey => "a tmux key, e.g. C-q",
        Kind::Secret => "not shown as it's typed; empty for none",
        Kind::Label { .. } => "empty for none",
        Kind::Status => "",
        Kind::Bool | Kind::Choice(_) => "",
    }
}
