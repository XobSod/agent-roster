//! Settings: one changed, an agent added, or config.toml in `$EDITOR`.

use super::*;

pub(super) fn change_setting(
    roster: &mut Roster,
    app: &mut App,
    setting: String,
    input: Option<String>,
) -> Result<String> {
    let before = roster.config_text.clone();
    let changed = roster.change_setting(&setting, input.as_deref());
    if roster.config_text != before {
        use_config(roster, app);
    }
    changed.map(|()| said_changed(&setting, input.as_deref()))
}

/// What changing `setting` to `input` did, as the CLI says it: None is its
/// default again; for the CLI Proxy's key, none typed takes it out.
fn said_changed(setting: &str, input: Option<&str>) -> String {
    let secret = setting == crate::secrets::PROXY_KEY;
    match input {
        None if !secret => format!("{setting} is back to its default"),
        Some(key) if secret && !key.trim().is_empty() => format!("Set {setting}"),
        _ if secret => format!("{setting} is taken out"),
        _ => format!("Set {setting}"),
    }
}

/// Hands the terminal to `$VISUAL` or `$EDITOR` (else vi) on config.toml, made
/// first from the template of every setting if there's none, then puts what
/// was saved in use. The outer result is the terminal's.
pub(super) fn edit_config(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
) -> Result<Result<String>> {
    let file = match roster.config_file_to_edit() {
        Ok((file, _)) => file,
        Err(e) => return Ok(Err(e)),
    };
    Ok(
        match in_terminal(terminal, || crate::util::run_editor(&file))? {
            Err(e) => Err(e),
            Ok(_) => match roster.reload_config_now() {
                Ok(Reloaded::InUse(problem)) => {
                    use_config(roster, app);
                    match problem {
                        None => Ok("config.toml saved: its settings are in use".into()),
                        Some(e) => Err(e.context(
                            "config.toml's settings are in use, but roster's tmux didn't take them",
                        )),
                    }
                }
                Ok(Reloaded::Same) => Ok("config.toml unchanged".into()),
                Err(e) => Err(e.context("config.toml has an error, the settings in use stay")),
            },
        },
    )
}

pub(super) fn set_proxy_label(
    roster: &mut Roster,
    app: &mut App,
    provider: String,
    email: String,
    label: Option<String>,
) -> Result<String> {
    let before = roster.config_text.clone();
    let done = roster.set_proxy_label(&provider, &email, label.as_deref());
    if roster.config_text != before {
        use_config(roster, app);
    }
    done.map(|()| match label {
        Some(label) => format!("{email} is labelled {label}"),
        None => format!("{email}'s label is taken out"),
    })
}

pub(super) fn add_agent(roster: &mut Roster, app: &mut App, line: String) -> Result<String> {
    let before = roster.config_text.clone();
    let added = roster.add_agent(&line);
    if roster.config_text != before {
        use_config(roster, app);
    }
    added.map(|name| format!("Added the agent {name}: new sessions can run it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_proxys_key_emptied_or_taken_out_is_said_taken_out() {
        let key = crate::secrets::PROXY_KEY;
        assert_eq!(said_changed(key, Some("made-up")), "Set cli_proxy.key");
        assert_eq!(said_changed(key, Some(" ")), "cli_proxy.key is taken out");
        assert_eq!(said_changed(key, None), "cli_proxy.key is taken out");
        // Any other setting: empty is a value too.
        assert_eq!(said_changed("shell", Some("")), "Set shell");
        assert_eq!(said_changed("shell", None), "shell is back to its default");
    }
}
