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
/// it's sent. In a directory with a `.fullscreen` file it draws full screen
/// instead (the alternate screen), redrawing its size whenever it changes,
/// each size it's told kept in `.sizes` there, rows and columns.
const FAKE_AGENT: &str = include_str!("../fakes/claude.sh");

const TIMEOUT: Duration = Duration::from_secs(15);

pub struct TestEnv {
    dir: tempfile::TempDir,
    pub socket: String,
    /// The `PATH` roster runs with (`without_agents`).
    search_path: String,
}

/// The agents roster finds installed on `PATH` (as `agent::BUILT_IN`).
const AGENTS: &[&str] = &["claude", "pi", "codex"];

/// The inherited `PATH` with no agent on it, so one installed on the machine
/// running the tests isn't found and run: each directory holding one is
/// replaced by a copy in `shadows` without it (links to everything else).
fn without_agents(shadows: &std::path::Path) -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).enumerate().map(|(i, dir)| {
        if !AGENTS.iter().any(|a| dir.join(a).exists()) {
            return dir;
        }
        let shadow = shadows.join(i.to_string());
        fs::create_dir_all(&shadow).unwrap();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            if !AGENTS.iter().any(|a| name == *a) {
                let _ = std::os::unix::fs::symlink(entry.path(), shadow.join(&name));
            }
        }
        shadow
    });
    std::env::join_paths(dirs.collect::<Vec<_>>())
        .unwrap()
        .into_string()
        .unwrap()
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
            search_path: without_agents(&dir.path().join("path")),
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

    /// Claude's own directory, for conversations started outside roster.
    pub fn claude_dir(&self) -> PathBuf {
        self.path("claude")
    }

    /// pi's own directory (`PI_CODING_AGENT_DIR`), likewise.
    pub fn pi_dir(&self) -> PathBuf {
        self.path("pi")
    }

    /// codex's own directory (`CODEX_HOME`), likewise.
    pub fn codex_dir(&self) -> PathBuf {
        self.path("codex")
    }

    /// A conversation Claude kept, as if run in `cwd`, with Claude's title.
    pub fn conversation(&self, id: &str, cwd: &std::path::Path, title: &str) -> PathBuf {
        let folder = self.claude_dir().join("projects").join("-somewhere");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{id}.jsonl"));
        let lines = [
            serde_json::json!({"type": "user", "cwd": cwd, "message": {"content": "hello"}}),
            serde_json::json!({"type": "assistant", "cwd": cwd, "message": {"id": "m1", "model": "claude-sonnet-5-5"}}),
            serde_json::json!({"type": "ai-title", "aiTitle": title}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(&path, text).unwrap();
        path
    }

    /// Registers conversation `id` as open in a running Claude (a process whose
    /// command line says claude), the way Claude does. Kill it when done.
    pub fn open_in_claude(&self, id: &str) -> std::process::Child {
        let bin = self.path("claude-bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new("sh").arg(&bin).spawn().unwrap();
        let sessions = self.claude_dir().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let pid = child.id();
        // Until it has exec'd, its command line is still the test's.
        let deadline = Instant::now() + TIMEOUT;
        while !fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .is_ok_and(|c| c.contains("claude"))
        {
            assert!(
                Instant::now() < deadline,
                "the stand-in Claude didn't start"
            );
            sleep(Duration::from_millis(10));
        }
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"{id}"}}"#),
        )
        .unwrap();
        child
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
            .env("PATH", &self.search_path)
            .env("CLAUDE_CONFIG_DIR", self.claude_dir())
            .env("PI_CODING_AGENT_DIR", self.pi_dir())
            .env("CODEX_HOME", self.codex_dir())
            .env_remove("PI_CODING_AGENT_SESSION_DIR")
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
            "env -u TMUX -u TMUX_PANE -u PI_CODING_AGENT_SESSION_DIR XDG_CONFIG_HOME='{}' XDG_STATE_HOME='{}' ROSTER_TMUX_SOCKET='{}' PATH='{}' CLAUDE_CONFIG_DIR='{}' PI_CODING_AGENT_DIR='{}' CODEX_HOME='{}' '{}'",
            self.path("config").display(),
            self.path("state").display(),
            self.socket,
            self.search_path,
            self.claude_dir().display(),
            self.pi_dir().display(),
            self.codex_dir().display(),
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
            &["ls", "--json", "--archived"]
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
        Terminal::start_named(env, "term", command, size)
    }

    /// Another terminal next to the first, on a server of its own (`name`).
    pub fn start_named(env: &TestEnv, name: &str, command: &str, size: (u16, u16)) -> Terminal {
        let term = Terminal {
            socket: format!("{}-{name}", env.socket),
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
        self.tmux(&["send-keys", "-t", "term", "-l", "--", text])
            .output()
            .unwrap();
        sleep(Duration::from_millis(50));
    }

    pub fn resize(&self, size: (u16, u16)) {
        let (w, h) = (size.0.to_string(), size.1.to_string());
        self.tmux(&["resize-window", "-t", "term", "-x", &w, "-y", &h])
            .output()
            .unwrap();
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
