//! Titles made for a session: Haiku names what its recent prompts are about.

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::agent;
use crate::engine::Roster;

impl Roster {
    /// Asks Haiku for a title from the session's recent prompts and keeps it as
    /// the session's title (the agent's own stays the same otherwise).
    pub fn generate_title(&mut self, id: &str) -> Result<String> {
        let mut s = self.db.session(id)?;
        let adapter = agent::adapter(&s.agent);
        // (Asked without a path: whether it has a transcript roster reads at all.)
        if adapter.transcript(Path::new("")).is_none() {
            bail!(
                "{} keeps no transcript roster can read, so it can't get a generated title; set one with edit",
                s.agent
            );
        }
        let path = s.transcript_path.clone().filter(|p| Path::new(p).exists());
        let path =
            path.with_context(|| format!("{} has no conversation to title yet", s.label()))?;
        let mut transcript = adapter
            .transcript(Path::new(&path))
            .context("no transcript")?;
        transcript.update()?;
        let prompts = transcript.recent_prompts();
        if prompts.is_empty() {
            bail!("{} has no prompts to title yet", s.label());
        }
        // Haiku makes the title, whichever agent the session runs.
        let mut argv = agent::claude::oneoff(&self.config);
        argv.push(title_request(&prompts, s.display_title()));
        let argv = self.wrapped(&s, &argv)?;
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&s.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for var in crate::tmux::LEAKY_ENV {
            cmd.env_remove(var);
        }
        let output = run_with_timeout(cmd, "claude", TITLE_TIMEOUT)?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!(
                "claude couldn't make a title: {}",
                err.lines().next().unwrap_or("no output")
            );
        }
        let title = clean_title(&String::from_utf8_lossy(&output.stdout))
            .context("claude gave no title")?;
        s.title_override = Some(title.clone());
        self.db.save_session(&s)?;
        Ok(title)
    }
}

/// How long a title may take.
const TITLE_TIMEOUT: Duration = Duration::from_secs(60);

fn title_request(prompts: &[String], current: &str) -> String {
    let mut request =
        String::from("These are the latest requests in a coding session, oldest first:\n\n");
    for p in prompts {
        let p: String = p.chars().take(300).collect();
        request.push_str(&format!("- {}\n", p.replace('\n', " ")));
    }
    request.push_str(&format!(
        "\nIts title so far: {current}\n\nGive a short title (3 to 7 words) for what the session is working on now. Reply with the title only."
    ));
    request
}

/// The first line of the reply, without quotes or markdown, at most 80 characters.
fn clean_title(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '#' | ' '));
    let title: String = line.chars().take(80).collect();
    (!title.is_empty()).then_some(title)
}

/// Runs `cmd` (`what`, in messages) to its end, for at most `timeout`. Its
/// output is read meanwhile, so a full pipe can't hold it up.
fn run_with_timeout(
    mut cmd: std::process::Command,
    what: &str,
    timeout: Duration,
) -> Result<std::process::Output> {
    use std::io::Read;
    let mut child = cmd.spawn().with_context(|| format!("can't run {what}"))?;
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{what} took longer than {}s", timeout.as_secs());
        }
        sleep(Duration::from_millis(100));
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_one_clean_line() {
        assert_eq!(
            clean_title("\n  \"Fix Login Tests\"  \nmore").as_deref(),
            Some("Fix Login Tests")
        );
        assert_eq!(
            clean_title("# **Retry uploads**").as_deref(),
            Some("Retry uploads")
        );
        assert_eq!(clean_title("  \n "), None);
        let request = title_request(&["fix\nthe tests".into()], "Old");
        assert!(request.contains("- fix the tests") && request.contains("Its title so far: Old"));
    }
}
