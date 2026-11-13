//! `roster config`: the settings, printed, changed or edited.

use crate::*;

/// `roster config`.
pub fn config(r: &mut Roster, change: Option<ConfigCmd>, defaults: bool, edit: bool) -> Result<()> {
    match change {
        Some(ConfigCmd::Set { setting, value }) => {
            let secret = setting == secrets::PROXY_KEY;
            let value = match value {
                Some(value) => value,
                // Not on the command line, so not in the shell's history.
                None if secret => read_secret("The CLI Proxy's management key: ")?,
                None => bail!("{setting}: what to set it to?"),
            };
            if secret && value.trim().is_empty() {
                bail!("no key given; roster config unset {setting} takes it out");
            }
            r.change_setting(&setting, Some(&value))?;
            match secret {
                true => println!(
                    "Set {setting}, kept in {}",
                    r.paths.secrets_file().display()
                ),
                false => println!("Set {setting}"),
            }
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
                None if setting == secrets::PROXY_KEY => println!("{setting} is taken out"),
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

/// A line read from stdin, its end taken off: on a terminal, after `prompt`,
/// not shown as it's typed (Esc or C-c leaving it unset).
fn read_secret(prompt: &str) -> Result<String> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::crossterm::terminal;
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\n', '\r']).to_string());
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    // Keys one by one, none shown: C-c among them, so the terminal is
    // always put back.
    terminal::enable_raw_mode()?;
    let typed = (|| -> Result<Option<String>> {
        let mut line = String::new();
        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Enter => return Ok(Some(line)),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c' | 'd') if ctrl => return Ok(None),
                KeyCode::Char('u') if ctrl => line.clear(),
                KeyCode::Backspace => {
                    line.pop();
                }
                KeyCode::Char(c) if !ctrl => line.push(c),
                _ => {}
            }
        }
    })();
    terminal::disable_raw_mode().ok();
    eprintln!();
    typed?.context("not set")
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
