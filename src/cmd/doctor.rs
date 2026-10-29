//! `roster doctor`: what's wrong with roster's tmux and database, and fixing it.

use crate::*;

/// `roster doctor`.
pub fn doctor(r: &mut Roster, fix: bool, yes: bool) -> Result<()> {
    let mut diagnosis = r.diagnose()?;
    if fix && !diagnosis.orphans.is_empty() {
        let question = format!("Kill {} orphaned tmux session(s)?", diagnosis.orphans.len());
        if confirm(&question, yes)? {
            let killed = r.kill_orphans()?;
            println!("Killed {killed} orphaned tmux session(s).\n");
            diagnosis = r.diagnose()?;
        }
    }
    let tty = io::stdout().is_terminal();
    for line in &diagnosis.lines {
        println!("{}", doctor_line(line, tty));
    }
    if diagnosis.problems > 0 {
        return Err(anyhow::anyhow!("{} problem(s)", diagnosis.problems));
    }
    Ok(())
}
