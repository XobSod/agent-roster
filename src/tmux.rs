//! roster's private tmux server: one tmux session per agent
//! session, a generated config with no prefix and only the back key bound, and a
//! clean environment.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

/// Variables never passed into roster's tmux: the ones that make tmux think it's
/// nested, and Claude Code's per-session ones, which leak in
/// when roster itself is run from inside Claude Code.
const LEAKY_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
];

/// State of the pane that runs an agent.
#[derive(Clone, Debug)]
pub struct Pane {
    pub dead: bool,
    pub dead_status: Option<i32>,
    pub pid: Option<i32>,
}

pub struct Tmux {
    socket: String,
    conf: PathBuf,
}

impl Tmux {
    pub fn new(socket: String, conf: PathBuf) -> Tmux {
        Tmux { socket, conf }
    }

    /// Writes the generated config. tmux reads it when the server starts.
    pub fn write_config(&self, detach_key: &str, user_conf: &Path) -> Result<()> {
        let text = config_text(detach_key, user_conf);
        if fs::read_to_string(&self.conf).ok().as_deref() != Some(text.as_str()) {
            fs::write(&self.conf, text)
                .with_context(|| format!("can't write {}", self.conf.display()))?;
        }
        Ok(())
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.arg("-L").arg(&self.socket).arg("-f").arg(&self.conf);
        for var in LEAKY_ENV {
            cmd.env_remove(var);
        }
        cmd
    }

    fn run<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = self.command();
        cmd.args(args);
        let out = cmd.output().context("can't run tmux (is it installed?)")?;
        if !out.status.success() {
            bail!("tmux: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Panes of roster's sessions by tmux session name; empty if the server isn't running.
    pub fn panes(&self) -> Result<HashMap<String, Pane>> {
        let out = self
            .command()
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_pid}",
            ])
            .output()
            .context("can't run tmux (is it installed?)")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("no server running") || err.contains("error connecting") {
                return Ok(HashMap::new());
            }
            bail!("tmux: {}", err.trim());
        }
        let mut panes = HashMap::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            if let [name, dead, status, pid] = fields[..] {
                panes.entry(name.to_string()).or_insert(Pane {
                    dead: dead == "1",
                    dead_status: status.parse().ok(),
                    pid: pid.parse().ok(),
                });
            }
        }
        Ok(panes)
    }

    /// Starts a detached session running `argv` (no shell in between, so arguments
    /// such as JSON need no quoting).
    pub fn new_session(
        &self,
        name: &str,
        workdir: &Path,
        size: (u16, u16),
        env: &[(&str, String)],
        argv: &[String],
    ) -> Result<()> {
        let mut args: Vec<&OsStr> = Vec::new();
        let (w, h) = (size.0.to_string(), size.1.to_string());
        args.extend(
            [
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                w.as_str(),
                "-y",
                h.as_str(),
                "-c",
            ]
            .map(OsStr::new),
        );
        args.push(workdir.as_os_str());
        let env: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for kv in &env {
            args.push(OsStr::new("-e"));
            args.push(OsStr::new(kv));
        }
        args.push(OsStr::new("--"));
        args.extend(argv.iter().map(OsStr::new));
        self.run(args).map(drop)
    }

    /// Replaces the (dead) pane's process with `argv`.
    pub fn respawn(
        &self,
        name: &str,
        workdir: &Path,
        env: &[(&str, String)],
        argv: &[String],
    ) -> Result<()> {
        let target = pane_target(name);
        let mut args: Vec<&OsStr> = ["respawn-pane", "-k", "-t", target.as_str(), "-c"]
            .map(OsStr::new)
            .to_vec();
        args.push(workdir.as_os_str());
        let env: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for kv in &env {
            args.push(OsStr::new("-e"));
            args.push(OsStr::new(kv));
        }
        args.push(OsStr::new("--"));
        args.extend(argv.iter().map(OsStr::new));
        self.run(args).map(drop)
    }

    pub fn kill_session(&self, name: &str) -> Result<()> {
        match self.run(["kill-session", "-t", &format!("={name}")]) {
            Err(e) if is_missing(&e) => Ok(()),
            other => other.map(drop),
        }
    }

    /// The visible screen plus `history` lines above it, with colours as escape
    /// sequences if `ansi`.
    pub fn capture(&self, name: &str, ansi: bool, history: u32) -> Result<String> {
        let target = pane_target(name);
        let start = format!("-{history}");
        let mut args = vec!["capture-pane", "-p", "-t", target.as_str()];
        if history > 0 {
            args.extend(["-S", start.as_str()]);
        }
        if ansi {
            args.push("-e");
        }
        self.run(args)
    }

    /// Attaches the current terminal until the user detaches (or the session ends).
    pub fn attach(&self, name: &str) -> Result<ExitStatus> {
        self.command()
            .args(["attach-session", "-t", &format!("={name}")])
            .status()
            .context("can't run tmux (is it installed?)")
    }
}

fn pane_target(name: &str) -> String {
    format!("={name}:")
}

fn is_missing(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("can't find session")
        || msg.contains("no server running")
        || msg.contains("error connecting")
}

fn config_text(detach_key: &str, user_conf: &Path) -> String {
    let user_conf = user_conf
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$");
    format!(
        "# Generated by roster on every run; don't edit. Your own additions go in
# {user_conf} (see examples/tmux.conf).

# No prefix and no default bindings: every key goes to the agent...
set -g prefix None
set -g prefix2 None
unbind -a -T prefix
unbind -a -T root
# ...except the back key.
bind -n {detach_key} detach-client

set -g status off
set -g mouse off
set -s escape-time 10
set -s extended-keys on
set -s set-clipboard on
set -g history-limit 50000
set -g remain-on-exit on

source-file -q \"{user_conf}\"
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_binds_only_the_detach_key_and_sources_user_file() {
        let text = config_text("C-q", Path::new("/home/u/.config/agent-roster/tmux.conf"));
        assert!(text.contains("bind -n C-q detach-client"));
        assert!(text.contains("unbind -a -T root"));
        assert!(text.contains("source-file -q \"/home/u/.config/agent-roster/tmux.conf\""));
    }

    #[test]
    fn config_escapes_the_user_path() {
        let text = config_text("C-q", Path::new("/we\"ird/$HOME/tmux.conf"));
        assert!(text.contains(r#"source-file -q "/we\"ird/\$HOME/tmux.conf""#));
    }
}
