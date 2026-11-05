//! Settings: one changed, an agent added, or config.toml in `$EDITOR`.

use super::*;

pub(super) fn change_setting(
    roster: &mut Roster,
    app: &mut App,
    setting: String,
    input: Option<String>,
) -> Result<String> {
    let back = input.is_none();
    let before = roster.config_text.clone();
    let changed = roster.change_setting(&setting, input.as_deref());
    if roster.config_text != before {
        use_config(roster, app);
    }
    changed.map(|()| {
        if back {
            format!("{setting} is back to its default")
        } else {
            format!("Set {setting}")
        }
    })
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

pub(super) fn add_agent(roster: &mut Roster, app: &mut App, line: String) -> Result<String> {
    let before = roster.config_text.clone();
    let added = roster.add_agent(&line);
    if roster.config_text != before {
        use_config(roster, app);
    }
    added.map(|name| format!("Added the agent {name}: new sessions can run it"))
}
