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
    /// stays the same otherwise), unless the session changed meanwhile
    /// (`keep_title`), which is an error.
    pub fn generate_title(&mut self, id: &str) -> Result<String> {
        let job = self.title_job(id)?;
        let asked_of = job.asked_of.clone();
        let label = job.label.clone();
        let title = job.run().with_context(|| label)?;
        match self.keep_title(id, &title, &asked_of)? {
            Kept::Yes => Ok(title),
            Kept::Retitled => bail!("its title changed meanwhile, so the new one isn't kept"),
            Kept::Moved => bail!("it's on another conversation now, so the new title isn't kept"),
            Kept::Gone => bail!("no session with id {id}"),
        }
    }

    /// Keeps `title`, made by a `TitleJob`, as session `id`'s title, unless
    /// the session changed since it was asked (`asked_of`): a title given (or
    /// taken off) meanwhile stays as it is, and one for a conversation it's
    /// no longer on isn't kept.
    pub fn keep_title(&mut self, id: &str, title: &str, asked_of: &AskedOf) -> Result<Kept> {
        // What its agent reported meanwhile (a `/clear`) counts.
        self.db.fold_events(crate::util::now_ms())?;
        let kept = self.db.set_title_if(
            id,
            title,
            asked_of.title.as_deref(),
            asked_of.conversation.as_deref(),
        )?;
        Ok(match kept {
            Some(true) => Kept::Yes,
            Some(false) if self.db.session(id)?.conversation_id != asked_of.conversation => {
                Kept::Moved
            }
            Some(false) => Kept::Retitled,
            None => Kept::Gone,
        })
    }

    /// `generate_title` for an agent that doesn't name its task itself,
    /// after its first turn: kept as the agent's title, so a name given in
    /// the agent (pi's `/name`) replaces it; unless it got one meanwhile or
    /// is on another conversation by then (the model takes a while).
    pub fn auto_title(&mut self, id: &str) -> Result<String> {
        // The conversation it's made of, as the job found it.
        let job = self.title_job(id)?;
        let conversation = job.asked_of.conversation.clone();
        let label = job.label.clone();
        let title = job.run().with_context(|| label)?;
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
            && adapter.runs_oneoff()
            && adapter.keeps_transcript()
            && self.config.agent(&s.agent).auto_title != Some(false)
    }

    /// What asks `id`'s agent for a title, ready to run anywhere (a thread:
    /// the TUI doesn't wait for the model, nor for its transcript to be
    /// read, which takes a while when it's big).
    pub fn title_job(&mut self, id: &str) -> Result<TitleJob> {
        // Its conversation as it is now, events reported so far applied.
        self.db.fold_events(crate::util::now_ms())?;
        let s = self.db.session(id)?;
        let adapter = agent::adapter(&s.agent);
        if !adapter.keeps_transcript() {
            bail!(
                "{} keeps no transcript roster can read, so it can't get a generated title; set one with edit",
                s.agent
            );
        }
        let path = s.transcript_path.clone().filter(|p| Path::new(p).exists());
        let path =
            path.with_context(|| format!("{} has no conversation to title yet", s.label()))?;
        let transcript =
            crate::agent::transcript_of(&s, Path::new(&path)).context("no transcript")?;
        let Some(oneoff) = adapter.oneoff(&self.config) else {
            bail!("{} can't make a title; set one with edit", s.agent);
        };
        Ok(TitleJob {
            transcript,
            argv: self.wrapped(&s, None, &oneoff)?,
            workdir: s.workdir.clone(),
            current: s.display_title().to_string(),
            label: s.label(),
            agent: s.agent,
            asked_of: AskedOf {
                title: s.title_override,
                conversation: s.conversation_id,
            },
        })
    }
}

/// A title being asked for: what's needed to ask the session's agent (its
/// transcript, read when it runs; its one-off command, through the env
/// files), which agent it is, and what the session was when asked.
pub struct TitleJob {
    transcript: Box<dyn crate::stats::Transcript>,
    /// Its one-off command run by roster's shell, the request to come last.
    argv: Vec<String>,
    workdir: std::path::PathBuf,
    /// Its title so far, for the model to improve on.
    current: String,
    pub label: String,
    pub agent: String,
    pub asked_of: AskedOf,
}

/// What a session was when its title was asked for: its title of your own
/// and its conversation, which a title made for it expects still.
#[derive(Clone, Debug)]
pub struct AskedOf {
    title: Option<String>,
    conversation: Option<String>,
}

/// Whether a title made was kept (`Roster::keep_title`).
#[derive(Debug, PartialEq)]
pub enum Kept {
    Yes,
    /// Its title of your own was given, changed or taken off meanwhile.
    Retitled,
    /// It's on another conversation now (a `/clear`, say).
    Moved,
    /// It's been deleted.
    Gone,
}

impl TitleJob {
    /// Reads its transcript's recent prompts and runs the agent on them, to
    /// the title (the run at most `TITLE_TIMEOUT`).
    pub fn run(mut self) -> Result<String> {
        self.transcript.update()?;
        let prompts = self.transcript.recent_prompts();
        if prompts.is_empty() {
            bail!("no prompts to title yet");
        }
        let mut argv = std::mem::take(&mut self.argv);
        argv.push(title_request(&prompts, &self.current));
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&self.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for var in crate::tmux::LEAKY_ENV {
            cmd.env_remove(var);
        }
        let agent = &self.agent;
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
