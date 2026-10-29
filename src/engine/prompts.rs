//! First prompts: kept in a file till typed in, typed in once the agent is ready.

use super::*;

/// A first prompt `refresh` couldn't type into session `id`, and why;
/// `again`: this roster tries it again.
pub struct Undelivered {
    pub id: String,
    pub message: String,
    pub again: bool,
}

/// How often one roster tries a first prompt that never got into its agent;
/// after that it stays, for another roster or the agent's next start.
pub(super) const PROMPT_TRIES: u32 = 3;

/// How the files a first prompt waits in for the launch script are named,
/// before the session's id (`Roster::prompt_file`).
pub(super) const PROMPT_FILE: &str = "first-prompt-";

/// The longest argument Linux gives a program: under 128 KB
/// (`MAX_ARG_STRLEN`, its terminating NUL in it). A first prompt on the
/// agent's command line can't be longer.
pub(super) const LONGEST_ARGUMENT: usize = 128 * 1024 - 1;

/// Removes the first prompts' files in `dir` a launch script never got to
/// (its shell gone, its env file failing): session `id`'s, which is starting
/// again, left by this roster or by one that's gone (another starting it now
/// has its own, about to be read), and any an hour old.
pub(super) fn sweep_prompts(dir: &Path, id: &str) {
    let this_sessions = format!("{PROMPT_FILE}{id}.");
    let gone = |pid: &str| match pid.parse::<i32>() {
        Ok(pid) if pid as u32 == std::process::id() => true,
        Ok(pid) if pid > 0 => {
            // SAFETY: signal 0 only asks whether the process is there.
            let asked = unsafe { libc::kill(pid, 0) };
            asked != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        }
        _ => true,
    };
    let an_hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with(PROMPT_FILE)) else {
            continue;
        };
        let old = (entry.metadata().and_then(|m| m.modified())).is_ok_and(|t| t < an_hour_ago);
        let left = name.strip_prefix(&this_sessions).is_some_and(gone);
        if left || old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A first prompt that fits `agent`'s command line, or why not.
pub(super) fn check_prompt_fits(agent: &str, prompt: &str) -> Result<()> {
    if prompt.len() > LONGEST_ARGUMENT {
        bail!(
            "the first prompt is too long for {agent}'s command line: {} bytes, where it takes {LONGEST_ARGUMENT} at most",
            prompt.len()
        );
    }
    Ok(())
}

impl Roster {
    /// A first prompt that couldn't be typed into session `id`: one that never
    /// got into its agent stays (this roster tries it a few times, then
    /// leaves it to another roster or the agent's next start); one that did
    /// (only its Enter missing) doesn't, so it isn't typed twice.
    pub(super) fn untyped(&mut self, id: &str, prompt: &str, untyped: crate::tmux::Untyped) {
        let tries = self.prompt_tries.entry(id.to_string()).or_default();
        *tries += 1;
        let kept = !untyped.pasted && self.db.restore_pending_prompt(id, prompt).is_ok();
        let again = kept && *tries < PROMPT_TRIES;
        let label = self.db.session(id).map_or(id.to_string(), |s| s.label());
        let what = match (untyped.pasted, kept, again) {
            (true, ..) => "it's in, but Enter didn't get to it",
            (false, _, true) => "trying again",
            (false, true, false) => "it stays, for its next start",
            (false, false, _) => "and it's lost",
        };
        self.undelivered.push(Undelivered {
            id: id.to_string(),
            message: format!(
                "couldn't type the first prompt into {label} ({what}): {:#}",
                untyped.error
            ),
            again,
        });
    }

    /// First prompts `refresh` couldn't type in since this was last asked.
    pub fn take_undelivered(&mut self) -> Vec<Undelivered> {
        std::mem::take(&mut self.undelivered)
    }

    /// Types session `id`'s first prompt in once its agent is ready, in a
    /// roster process of its own (`roster deliver`): for while you're
    /// attached, when nothing else refreshes.
    pub fn deliver_in_background(&self, id: &str) {
        self.in_background(&["deliver", id]);
    }

    /// Where session `id`'s first prompt waits for the launch script to give
    /// it to the agent (and remove it).
    pub(super) fn prompt_file(&self, id: &str) -> PathBuf {
        let name = format!("{PROMPT_FILE}{id}.{}", std::process::id());
        self.paths.state_dir.join(name)
    }

    /// Removes the first prompts' files a launch script never got to
    /// (`sweep_prompts`).
    pub(super) fn sweep_prompts(&self, id: &str) {
        sweep_prompts(&self.paths.state_dir, id);
    }

    /// Writes `text` into `file`, only the user's to read; nothing is left
    /// of one that fails.
    pub(super) fn write_prompt(&self, file: &Path, text: &str) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let written = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        if let Err(e) = written {
            let _ = fs::remove_file(file);
            return Err(e).with_context(|| format!("can't write {}", file.display()));
        }
        Ok(())
    }

    /// Waits (refreshing) until a session's first prompt has been typed in, for
    /// one-shot commands. False if the agent wasn't ready within `timeout`, or
    /// stopped or was archived first; an error if typing it in failed for good,
    /// or failed and time ran out.
    pub fn wait_for_prompt(&mut self, id: &str, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut last_failure = None;
        loop {
            self.refresh()?;
            // Its own; others' stay for whoever asks.
            let failed = (self.undelivered.iter())
                .position(|u| u.id == id)
                .map(|i| self.undelivered.remove(i));
            let s = self.db.session(id)?;
            match failed {
                // Not typed in, and not to be tried again by this roster.
                Some(u) if !u.again => bail!("{}", u.message),
                Some(u) => last_failure = Some(u.message),
                None => {}
            }
            if s.pending_prompt.is_none() {
                return Ok(true);
            }
            // Its agent gone (exited, archived): nothing to type it into.
            if s.state == State::Stopped || s.deleted_at.is_some() {
                return Ok(false);
            }
            if Instant::now() > deadline {
                return match last_failure {
                    Some(message) => Err(anyhow::anyhow!(message)),
                    None => Ok(false),
                };
            }
            sleep(Duration::from_millis(500));
        }
    }

    /// Whether an agent is waiting for its first prompt: for one with hooks,
    /// after it has reported its session, which names its transcript (a new
    /// conversation has none before that); for others, idle after starting up.
    /// (An agent that takes its first prompt on its command line never waits.)
    pub(super) fn ready_for_prompt(&self, s: &Session, now: i64) -> bool {
        s.state == State::Idle
            && if agent::adapter(&s.agent).reports() {
                s.transcript_path.is_some()
            } else {
                s.baseline_at.is_some_and(|b| now > b)
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompts_file_is_swept_once_whoever_wrote_it_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mut done = std::process::Command::new("true").spawn().unwrap();
        done.wait().unwrap();
        let file = |id: &str, pid: u32| dir.path().join(format!("{PROMPT_FILE}{id}.{pid}"));
        let (mine, dead, live) = (
            file("s1", std::process::id()),
            file("s1", done.id()),
            file("s1", 1),
        );
        let other = file("s2", done.id());
        for f in [&mine, &dead, &live, &other] {
            fs::write(f, "fix it").unwrap();
        }
        sweep_prompts(dir.path(), "s1");
        // This roster's and a gone one's go; a roster starting it now keeps
        // its own, and another session's, not an hour old, stays.
        let left: Vec<bool> = [&mine, &dead, &live, &other].map(|f| f.exists()).into();
        assert_eq!(left, [false, false, true, true]);
    }

    #[test]
    fn a_first_prompt_fits_a_command_line_up_to_linuxs_longest_argument() {
        assert!(check_prompt_fits("codex", &"x".repeat(LONGEST_ARGUMENT)).is_ok());
        let e = check_prompt_fits("codex", &"x".repeat(200_000)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "the first prompt is too long for codex's command line: 200000 bytes, where it takes 131071 at most"
        );
        assert!(check_prompt_fits("codex", &"x".repeat(LONGEST_ARGUMENT + 1)).is_err());
    }
}
