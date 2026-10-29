//! `roster config`: the settings, printed, changed or edited.

use crate::*;

/// `roster config`.
pub fn config(r: &mut Roster, change: Option<ConfigCmd>, defaults: bool, edit: bool) -> Result<()> {
    match change {
        Some(ConfigCmd::Set { setting, value }) => {
            r.change_setting(&setting, Some(&value))?;
            println!("Set {setting}");
        }
        Some(ConfigCmd::AddAgent { command }) => {
            // Each word as it was given, the settings view's line.
            let line = command
                .iter()
                .map(|w| util::sh_quote(w))
                .collect::<Vec<_>>();
            let name = r.add_agent(&line.join(" "))?;
            println!("Added the agent {name}: new sessions can run it");
        }
        Some(ConfigCmd::Unset { setting, yes }) => {
            let question = r.unset_question(&setting);
            if let Some(question) = &question
                && !confirm(question, yes)?
            {
                return Ok(());
            }
            r.change_setting(&setting, None)?;
            match question {
                Some(_) => println!("Took [{setting}] out of config.toml"),
                None => println!("{setting} is back to its default"),
            }
        }
        None if defaults => print!("{}", settings::template(&r.settings())),
        None if edit => {
            let (file, made) = r.config_file_to_edit()?;
            if !util::run_editor(&file)? {
                bail!("the editor exited with an error; config.toml is as it left it");
            }
            match r.reload_config_now()? {
                settings::Reloaded::Same if made => {
                    println!("config.toml made, every setting commented out at its default")
                }
                settings::Reloaded::Same => println!("config.toml unchanged"),
                settings::Reloaded::InUse(None) => println!("config.toml saved"),
                settings::Reloaded::InUse(Some(e)) => {
                    return Err(e.context("config.toml is saved, but roster's tmux didn't take it"));
                }
            }
        }
        None => print::settings(r),
    }
    Ok(())
}

/// `roster config --edit` when roster couldn't open (`why`): if config.toml
/// is why, the file in the editor, then roster opened with it, which puts it
/// in use in roster's tmux too; anything else is `why` as it is.
pub fn edit_unread_config(why: anyhow::Error) -> Result<()> {
    let file = paths::Paths::from_env()?.config_file();
    if config::Config::load_text(&file).is_ok() {
        return Err(why);
    }
    eprintln!("roster: {why:#}");
    if !util::run_editor(&file)? {
        bail!("the editor exited with an error; config.toml is as it left it");
    }
    config::Config::load_text(&file).context("config.toml still has an error")?;
    let mut r = Roster::open()?;
    // What it sets that roster no longer has is only said; roster's tmux
    // refusing what it sets is a failure.
    let gone = r.config.gone();
    let (said, refused): (Vec<String>, Vec<String>) =
        r.take_notices().into_iter().partition(|n| gone.contains(n));
    for notice in said {
        eprintln!("roster: {notice}");
    }
    if !refused.is_empty() {
        bail!("config.toml is saved, but {}", refused.join("; "));
    }
    println!("config.toml saved");
    Ok(())
}
