//! Titles made for a session: its agent, in a one-off run, names what its
//! recent prompts are about.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::engine::Roster;
use crate::{agent, launch};

impl Roster {
    /// Asks the session's agent (`Adapter::oneoff`) for a title from its
    /// recent prompts and keeps it as the session's title (the agent's own
    /// stays the same otherwise).
    pub fn generate_title(&mut self, id: &str) -> Result<String> {
        let title = self.make_title(id)?;
        let mut s = self.db.session(id)?;
        let before = s.clone();
        s.title_override = Some(title.clone());
        self.db.save_changes(&before, &s)?;
        Ok(title)
    }

    /// `generate_title` for an agent that doesn't name its task itself,
    /// after its first turn: kept as the agent's title, so a name given in
    /// the agent (pi's `/name`) replaces it; unless it got one meanwhile or
    /// is on another conversation by then (the model takes a while).
    pub fn auto_title(&mut self, id: &str) -> Result<String> {
        let conversation = self.db.session(id)?.conversation_id;
        let title = self.make_title(id)?;
        let made = serde_json::json!({ "conversation_id": conversation, "title": title });
        self.db.insert_event(
            id,
            crate::model::AUTO_TITLE,
            crate::util::now_ms(),
            Some(&made.to_string()),
        )?;
        Ok(title)
    }

    /// Whether `s` is due an automatic title: its agent doesn't name its
    /// task but can make a title, it has finished a turn and has no title,
    /// and `auto_title` isn't off.
    pub(crate) fn wants_auto_title(&self, s: &crate::model::Session) -> bool {
        let adapter = agent::adapter(&s.agent);
        s.state == crate::model::State::Idle
            && s.turns > 0
            && s.title().is_none()
            && s.conversation_id.is_some()
            && s.transcript_path
                .as_deref()
                .is_some_and(|p| Path::new(p).exists())
            && !adapter.names_its_task()
            && adapter.oneoff(&self.config).is_some()
            && adapter.transcript(Path::new("")).is_some()
            && self.config.agent(&s.agent).auto_title != Some(false)
    }

    /// The title `id`'s agent makes of its recent prompts.
    fn make_title(&mut self, id: &str) -> Result<String> {
        let s = self.db.session(id)?;
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
        let Some(mut argv) = adapter.oneoff(&self.config) else {
            bail!("{} can't make a title; set one with edit", s.agent);
        };
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
        let agent = &s.agent;
        let output = launch::run_with_timeout(cmd, agent, TITLE_TIMEOUT)?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output");
            // Its model may be one you can't use.
            bail!(
                "{agent} couldn't make a title: {err} (title_model under [agents.{agent}] in config.toml picks its model)"
            );
        }
        clean_title(&String::from_utf8_lossy(&output.stdout))
            .with_context(|| format!("{agent} gave no title"))
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
