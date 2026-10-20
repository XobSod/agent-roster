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
pub(crate) const LEAKY_ENV: &[&str] = &[
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
    /// The terminal title the program set (tmux's default is the host name).
    pub title: String,
}

pub struct Tmux {
    socket: String,
    conf: PathBuf,
}

/// The running server, for diagnostics.
#[derive(Clone, Debug)]
pub struct Server {
    pub pid: i64,
    pub version: String,
    /// Unix time in seconds.
    pub started: i64,
    pub socket_path: String,
}

/// One pane with everything diagnostics show about it.
#[derive(Clone, Debug)]
pub struct PaneDetail {
    pub session: String,
    pub clients: u32,
    pub dead: bool,
    pub dead_status: Option<i32>,
    /// The signal that killed it, if one did (tmux 3.3+).
    pub dead_signal: Option<i32>,
    pub pid: Option<i32>,
    pub command: String,
    pub width: u16,
    pub height: u16,
    pub history: u64,
}

impl Tmux {
    pub fn new(socket: String, conf: PathBuf) -> Tmux {
        Tmux { socket, conf }
    }

    /// Writes the generated config. tmux reads it when the server starts.
    /// Writes the generated config, which tmux reads when the server starts.
    /// Returns true if it changed (including edits to the user's tmux.conf).
    pub fn write_config(&self, detach_key: &str, user_conf: &Path) -> Result<bool> {
        let text = config_text(detach_key, user_conf);
        if fs::read_to_string(&self.conf).ok().as_deref() == Some(text.as_str()) {
            return Ok(false);
        }
        fs::write(&self.conf, text)
            .with_context(|| format!("can't write {}", self.conf.display()))?;
        Ok(true)
    }

    /// Makes a running server re-read the generated config.
    pub fn reload_config(&self) -> Result<()> {
        match self.run([OsStr::new("source-file"), self.conf.as_os_str()]) {
            Err(e) if is_missing(&e) => Ok(()),
            other => other.map(drop),
        }
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
                "#{session_name}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_pid}\t#{pane_title}",
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
            // The title comes last and may itself contain tabs.
            let fields: Vec<&str> = line.splitn(5, '\t').collect();
            if let [name, dead, status, pid, title] = fields[..] {
                panes.entry(name.to_string()).or_insert(Pane {
                    dead: dead == "1",
                    dead_status: status.parse().ok(),
                    pid: pid.parse().ok(),
                    title: title.to_string(),
                });
            }
        }
        Ok(panes)
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    pub fn conf_path(&self) -> &Path {
        &self.conf
    }

    /// Runs a query; None if the server isn't running.
    fn query(&self, args: &[&str]) -> Result<Option<String>> {
        match self.run(args) {
            Err(e) if is_missing(&e) => Ok(None),
            other => other.map(Some),
        }
    }

    pub fn server(&self) -> Result<Option<Server>> {
        let Some(out) = self.query(&[
            "list-sessions",
            "-F",
            "#{pid}\t#{version}\t#{start_time}\t#{socket_path}",
        ])?
        else {
            return Ok(None);
        };
        Ok(out.lines().next().and_then(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            match f[..] {
                [pid, version, started, path] => Some(Server {
                    pid: pid.parse().ok()?,
                    version: version.to_string(),
                    started: started.parse().unwrap_or(0),
                    socket_path: path.to_string(),
                }),
                _ => None,
            }
        }))
    }

    pub fn pane_details(&self) -> Result<Vec<PaneDetail>> {
        let format = "#{session_name}\t#{session_attached}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\t#{pane_pid}\t#{pane_current_command}\t#{pane_width}\t#{pane_height}\t#{history_size}";
        let Some(out) = self.query(&["list-panes", "-a", "-F", format])? else {
            return Ok(Vec::new());
        };
        Ok(out
            .lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.split('\t').collect();
                match f[..] {
                    [
                        session,
                        clients,
                        dead,
                        status,
                        signal,
                        pid,
                        command,
                        width,
                        height,
                        history,
                    ] => Some(PaneDetail {
                        session: session.to_string(),
                        clients: clients.parse().unwrap_or(0),
                        dead: dead == "1",
                        dead_status: status.parse().ok(),
                        dead_signal: signal.parse().ok(),
                        pid: pid.parse().ok(),
                        command: command.to_string(),
                        width: width.parse().unwrap_or(0),
                        height: height.parse().unwrap_or(0),
                        history: history.parse().unwrap_or(0),
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    /// Whether `key` is bound to detach-client in the root table.
    pub fn detach_bound(&self, key: &str) -> Result<Option<bool>> {
        let Some(out) = self.query(&["list-keys", "-T", "root"])? else {
            return Ok(None);
        };
        Ok(Some(out.lines().any(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.contains(&"detach-client") && words.get(3) == Some(&key)
        })))
    }

    /// The server's global environment variables (names), e.g. to spot TMUX leaks.
    pub fn global_env(&self) -> Result<Option<Vec<String>>> {
        let Some(out) = self.query(&["show-environment", "-g"])? else {
            return Ok(None);
        };
        // Removed variables show as "-NAME".
        Ok(Some(
            out.lines()
                .filter(|l| !l.starts_with('-'))
                .filter_map(|l| l.split('=').next().map(String::from))
                .collect(),
        ))
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

    /// Puts a line into the pane as a bracketed paste, then presses Enter. A plain
    /// carriage return right after a burst of typed text is taken by Claude as
    /// part of a paste (a newline, not a submit); the paste markers and a pause
    /// make the Enter a key press. Programs that don't use bracketed paste get the
    /// plain text.
    pub fn type_line(&self, name: &str, text: &str) -> Result<()> {
        let target = pane_target(name);
        let buffer = format!("roster-{name}");
        self.run(["set-buffer", "-b", buffer.as_str(), "--", text])?;
        self.run([
            "paste-buffer",
            "-p",
            "-d",
            "-b",
            buffer.as_str(),
            "-t",
            target.as_str(),
        ])?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        self.run(["send-keys", "-t", target.as_str(), "Enter"])
            .map(drop)
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
    /// The client draws through the terminal it opens itself; its stdout only
    /// gets the message it prints on leaving ("[detached (from session …)]"),
    /// which would pile up behind the TUI, one per attach. Errors still show.
    pub fn attach(&self, name: &str) -> Result<ExitStatus> {
        self.command()
            .args(["attach-session", "-t", &format!("={name}")])
            .stdout(std::process::Stdio::null())
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
    // Noted in the text so that editing the user's file changes it.
    let modified = fs::metadata(user_conf)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or("absent".to_string(), |d| d.as_nanos().to_string());
    let user_conf = user_conf
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$");
    format!(
        "# Generated by roster on every run; don't edit. Your own additions go in
# {user_conf} (see examples/tmux.conf; last modified: {modified}).

# No prefix and no default bindings: every key goes to the agent...
set -g prefix None
set -g prefix2 None
unbind -aq -T prefix
unbind -aq -T root
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
        assert!(text.contains("unbind -aq -T root"));
        assert!(text.contains("source-file -q \"/home/u/.config/agent-roster/tmux.conf\""));
    }

    #[test]
    fn config_escapes_the_user_path() {
        let text = config_text("C-q", Path::new("/we\"ird/$HOME/tmux.conf"));
        assert!(text.contains(r#"source-file -q "/we\"ird/\$HOME/tmux.conf""#));
    }
}
