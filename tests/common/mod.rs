//! Shared helpers for the end-to-end tests: throwaway config and state
//! directories, a private tmux socket, a stand-in agent instead of Claude, and a
//! terminal (another tmux server) to drive the TUI in.

// Each test file uses a different subset.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Stand-in for `claude`: shows its arguments and environment, then echoes what
/// it's sent.
const FAKE_AGENT: &str = r#"#!/bin/sh
echo "fake agent: $*"
echo "TOKEN=${TOKEN-unset} GLOBAL=${GLOBAL-unset} SESSION=${ROSTER_SESSION-unset}"
exec cat
"#;

const TIMEOUT: Duration = Duration::from_secs(15);

pub struct TestEnv {
    dir: tempfile::TempDir,
    pub socket: String,
}

impl TestEnv {
    /// None if tmux isn't installed (the test is then skipped).
    pub fn new(name: &str) -> Option<TestEnv> {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("tmux isn't installed; skipping");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let env = TestEnv {
            socket: format!("roster-test-{}-{name}", std::process::id()),
            dir,
        };
        let agent = env.path("fake-agent");
        fs::write(&agent, FAKE_AGENT).unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(env.config_dir()).unwrap();
        fs::write(
            env.config_dir().join("config.toml"),
            format!(
                "shell = \"/bin/sh\"\n[agents.claude]\ncommand = \"{}\"\n",
                agent.display()
            ),
        )
        .unwrap();
        fs::create_dir_all(env.path("proj")).unwrap();
        Some(env)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn config_dir(&self) -> PathBuf {
        self.path("config/agent-roster")
    }

    pub fn db(&self) -> PathBuf {
        self.path("state/agent-roster/roster.db")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_roster"));
        cmd.args(args)
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("ROSTER_TMUX_SOCKET", &self.socket)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("ROSTER_SESSION")
            .env_remove("ROSTER_DB")
            .stdin(Stdio::null());
        cmd
    }

    /// The same as a shell command line, for running roster in a terminal.
    pub fn shell_command(&self) -> String {
        format!(
            "env -u TMUX -u TMUX_PANE XDG_CONFIG_HOME='{}' XDG_STATE_HOME='{}' ROSTER_TMUX_SOCKET='{}' '{}'",
            self.path("config").display(),
            self.path("state").display(),
            self.socket,
            env!("CARGO_BIN_EXE_roster"),
        )
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "roster {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Calls `roster report` like a hook of session `id` would.
    pub fn report(&self, id: &str, kind: &str, payload: &str) -> Output {
        let mut child = self
            .command(&["report", kind])
            .env("ROSTER_SESSION", id)
            .env("ROSTER_DB", self.db())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    pub fn session(&self, name: &str, deleted: bool) -> Option<Value> {
        let args: &[&str] = if deleted {
            &["ls", "--json", "--deleted"]
        } else {
            &["ls", "--json"]
        };
        let list: Value = serde_json::from_str(&self.ok(args)).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .cloned()
    }

    pub fn wait_for(&self, what: &str, done: impl Fn(&TestEnv) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        kill_server(&self.socket);
    }
}

/// A terminal to run the TUI in: a separate tmux server with one pane.
pub struct Terminal {
    socket: String,
}

impl Terminal {
    pub fn start(env: &TestEnv, command: &str, size: (u16, u16)) -> Terminal {
        let term = Terminal {
            socket: format!("{}-term", env.socket),
        };
        let out = term
            .tmux(&["-f", "/dev/null", "new-session", "-d", "-s", "term"])
            .args([
                "-x",
                &size.0.to_string(),
                "-y",
                &size.1.to_string(),
                command,
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        term
    }

    fn tmux(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.args(["-L", &self.socket])
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        cmd
    }

    /// Presses keys (tmux key names such as `Enter`, `C-q`, `j`).
    pub fn keys(&self, keys: &[&str]) {
        for key in keys {
            self.tmux(&["send-keys", "-t", "term", key])
                .output()
                .unwrap();
            sleep(Duration::from_millis(50));
        }
    }

    /// Types text literally.
    pub fn text(&self, text: &str) {
        self.tmux(&["send-keys", "-t", "term", "-l", text])
            .output()
            .unwrap();
        sleep(Duration::from_millis(50));
    }

    pub fn screen(&self) -> String {
        let out = self
            .tmux(&["capture-pane", "-p", "-t", "term"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn wait_for(&self, what: &str, needle: &str) {
        self.wait(what, |screen| screen.contains(needle));
    }

    pub fn wait_for_gone(&self, what: &str, needle: &str) {
        self.wait(what, |screen| !screen.contains(needle));
    }

    fn wait(&self, what: &str, done: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let screen = self.screen();
            if done(&screen) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; screen:\n{screen}"
            );
            sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        kill_server(&self.socket);
    }
}

fn kill_server(socket: &str) {
    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .env_remove("TMUX")
        .output();
    // tmux leaves its socket file behind.
    let tmpdir = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let _ = fs::remove_file(
        PathBuf::from(tmpdir)
            .join(format!("tmux-{uid}"))
            .join(socket),
    );
}
