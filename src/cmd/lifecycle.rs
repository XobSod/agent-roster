//! Sessions' lives: attaching, starting, stopping, killing, restarting, archiving, deleting and restoring them.

use crate::*;
use engine::{StartCheck, Starting};

/// `roster attach`.
pub fn attach(r: &mut Roster, session: String, yes: bool, again: PromptAgain) -> Result<()> {
    r.refresh()?;
    let s = r.resolve(&session, false)?;
    let Some(give) = asked(&s, Starting::Attach, yes, &again)? else {
        return Ok(());
    };
    if s.state == State::Stopped {
        // Here, so what starting says comes before tmux takes the
        // terminal; unless another roster started it while asking.
        r.refresh()?;
        if r.db.session(&s.id)?.state == State::Stopped {
            if give {
                r.give_prompt_again(&s.id)?;
            } else {
                r.start(&s.id)?;
            }
            say_notices(r);
        }
    }
    r.attach(&s.id)?;
    Ok(())
}

/// `roster start`.
pub fn start(r: &mut Roster, session: String, yes: bool, again: PromptAgain) -> Result<()> {
    r.refresh()?;
    let s = r.resolve(&session, false)?;
    // What can't be done isn't asked about first.
    if again.prompt_again
        && let Some(refused) = s.prompt_again_refused()
    {
        bail!(refused);
    }
    let Some(give) = asked(&s, Starting::Start, yes, &again)? else {
        return Ok(());
    };
    if give {
        r.give_prompt_again(&s.id)?;
        println!("Started {}, its first prompt given again", s.label());
        return Ok(());
    }
    r.start(&s.id)?;
    println!("Started {}", s.label());
    if s.given_prompt.is_some() {
        println!("Its first prompt, not seen taken, is kept");
    }
    Ok(())
}

/// `roster stop --kill`: the session's processes, or (`pid`) one of them
/// and those under it.
pub fn kill(r: &mut Roster, session: String, yes: bool, pid: Option<i32>) -> Result<()> {
    r.refresh()?;
    // A deleted session's id, still carried by what it left running.
    let (id, label) = match r.resolve_either(&session) {
        Ok(s) => (s.id.clone(), s.label()),
        Err(e) => match r.kill_targets(&session, None) {
            Ok(left) if !left.is_empty() => (session.clone(), format!("{session} (deleted)")),
            _ => return Err(e),
        },
    };
    // One of its processes as it is now, so a pid given to another since
    // isn't taken for it; named by what it runs, the agent said to be.
    let (process, label) = match pid {
        None => (None, label),
        Some(pid) => {
            let tree = r.kill_targets(&id, None)?;
            let p = (tree.iter().find(|p| p.pid == pid))
                .with_context(|| format!("process {pid} isn't one of {label}'s, or has ended"))?;
            let agent = if p.agent { ", its agent" } else { "" };
            let label = format!("process {pid} ({}{agent}) of {label}", p.command);
            (Some((p.pid, p.started)), label)
        }
    };
    let targets = r.kill_targets(&id, process)?;
    if targets.is_empty() {
        println!("{label}: nothing running");
        return Ok(());
    }
    // Those not under its agent may serve others too (a build daemon
    // started in it): named. With no agent, all of them.
    let outside: Vec<&str> = (targets.iter())
        .filter(|p| p.session.is_some())
        .map(|p| p.command.as_str())
        .collect();
    let mut names = outside.clone();
    names.sort();
    names.dedup();
    let names = names.join(", ");
    let count = match (targets.len(), outside.len()) {
        (1, 1) => format!("1 process left running ({names})"),
        (n, k) if k == n => format!("{n} processes left running ({names})"),
        (1, _) => "1 process".to_string(),
        (n, 0) => format!("{n} processes"),
        (n, k) => format!("{n} processes, {k} not under its agent ({names})"),
    };
    if !confirm(&format!("Kill {label} at once: {count}?"), yes)? {
        return Ok(());
    }
    // Not cut short, which would leave its processes stopped: nor by
    // its terminal hanging up as its agent ends, when run from inside
    // it (by its agent, say).
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        // SAFETY: setting a signal's disposition to ignore it.
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    let said = r.kill(&id, process)?.said(&label);
    // Its terminal may be gone by now.
    let _ = writeln!(io::stdout(), "{said}");
    Ok(())
}

/// `roster stop`.
pub fn stop(r: &mut Roster, session: String, yes: bool) -> Result<()> {
    r.refresh()?;
    let s = r.resolve(&session, false)?;
    if s.state == State::Working && !confirm(&format!("{} is working. Stop it?", s.label()), yes)? {
        return Ok(());
    }
    if r.stop(&s.id)? {
        println!("Stopped {}", s.label());
    } else {
        println!("{} wasn't running", s.label());
    }
    Ok(())
}

/// `roster restart`.
pub fn restart(r: &mut Roster, session: String, yes: bool, again: PromptAgain) -> Result<()> {
    r.refresh()?;
    let s = r.resolve(&session, false)?;
    let Some(give) = asked(&s, Starting::Restart, yes, &again)? else {
        return Ok(());
    };
    if give {
        r.restart_giving_prompt(&s.id)?;
        println!("Restarted {}, its first prompt given again", s.label());
    } else {
        r.restart(&s.id)?;
        println!("Restarted {}", s.label());
    }
    Ok(())
}

/// `roster archive`.
pub fn archive(r: &mut Roster, session: String, yes: bool) -> Result<()> {
    // Whether its agent still runs, for the question.
    r.refresh()?;
    let s = r.resolve(&session, false)?;
    if !confirm(&agent::archive_question(&s), yes)? {
        return Ok(());
    }
    r.archive(&s.id)?;
    println!("Archived {} ({})", s.label(), s.id);
    Ok(())
}

/// `roster rm`.
pub fn delete(r: &mut Roster, session: String, yes: bool) -> Result<()> {
    // What its hooks said since, in the question: a turn or a subagent
    // it would cut short.
    r.refresh()?;
    let s = r.resolve_either(&session)?;
    if !confirm(&agent::delete_question(&s), yes)? {
        return Ok(());
    }
    r.delete(&s.id)?;
    println!("Deleted {} ({}) for good", s.label(), s.id);
    Ok(())
}

/// `roster restore`.
pub fn restore(r: &mut Roster, session: String, name: Option<String>) -> Result<()> {
    let s = r.resolve(&session, true)?;
    let s = r.restore(&s.id, name.as_deref())?;
    println!("Restored {} (stopped)", s.label());
    Ok(())
}

/// Whether to go on starting `s` (`how`), and if so whether its first prompt
/// is given again: what starting it asks first (`engine::start_check`),
/// answered by `yes` and `again` or else asked; None for no start at all.
fn asked(s: &Session, how: Starting, yes: bool, again: &PromptAgain) -> Result<Option<bool>> {
    Ok(match engine::start_check(s, how) {
        StartCheck::Go => Some(false),
        StartCheck::Confirm(question) => confirm(&question, yes)?.then_some(false),
        StartCheck::PromptAgain { question, unasked } => {
            let answered = again.prompt_again || again.no_prompt_again;
            match unasked {
                Some(unasked) if answered && !confirm(&unasked, yes)? => None,
                _ => give_prompt_again(&question, again)?,
            }
        }
        StartCheck::Refused(why) => bail!(why),
    })
}

/// Whether a start whose first prompt its agent hasn't said it took gives
/// it again: as `again` says, else `question` asked (with a terminal to ask
/// in); None for no start at all.
fn give_prompt_again(question: &str, again: &PromptAgain) -> Result<Option<bool>> {
    if again.prompt_again || again.no_prompt_again {
        return Ok(Some(again.prompt_again));
    }
    if !io::stdin().is_terminal() {
        bail!("{question} Pass --prompt-again to give it, or --no-prompt-again to start without.");
    }
    eprint!("{question} [y/n, anything else cancels] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(match answer.trim().to_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    })
}
