//! End-to-end tests of the CLI against a real tmux server on a private socket,
//! with a stand-in script as the agent and `roster report` called the way Claude's
//! hooks would call it.

mod common;

use std::fs;
use std::process::Command;

use common::TestEnv;
use serde_json::Value;

#[test]
fn session_lifecycle() {
    let Some(t) = TestEnv::new("life") else {
        return;
    };
    fs::write(
        t.config_dir().join("env"),
        "GLOBAL=yes\nTOKEN=from-global\n",
    )
    .unwrap();
    fs::write(t.path("proj/.agent-roster.env"), "TOKEN=from-project\n").unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);

    let out = t.ok(&["new", "demo", "--name", "one"]);
    assert!(out.starts_with("Started demo/one"), "{out}");
    let id = t.session("one", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Both env files sourced, project file last; a new conversation with hooks.
    t.wait_for("the agent's output", |t| {
        t.ok(&["show", "one"])
            .contains("TOKEN=from-project GLOBAL=yes")
    });
    let screen = t.ok(&["show", "demo/one"]);
    assert!(screen.contains(&format!("SESSION={id}")), "{screen}");
    assert!(screen.contains("fake agent: --session-id"), "{screen}");

    // No hook events and a still screen: "working" settles to idle, not unread.
    t.wait_for("idle", |t| {
        t.session("one", false).unwrap()["state"] == "idle"
    });
    assert_eq!(t.session("one", false).unwrap()["unread"], false);

    // Hook events.
    let transcript = t.path("conv-1.jsonl");
    let payload = format!(
        r#"{{"session_id":"conv-1","transcript_path":"{}"}}"#,
        transcript.display()
    );
    let out = t.report(&id, "needs-input", &payload);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    let s = t.session("one", false).unwrap();
    assert_eq!(
        (s["state"].as_str(), s["unread"].as_bool()),
        (Some("needs-input"), Some(true))
    );
    assert_eq!(s["conversation_id"], "conv-1");
    t.report(&id, "idle", &payload);
    t.ok(&["read", "one"]);
    let s = t.session("one", false).unwrap();
    assert_eq!(
        (s["state"].as_str(), s["unread"].as_bool()),
        (Some("idle"), Some(false))
    );

    // Stop keeps the session; start resumes the conversation once its transcript exists.
    fs::write(&transcript, "{}").unwrap();
    assert!(t.ok(&["stop", "one"]).starts_with("Stopped"));
    let s = t.session("one", false).unwrap();
    assert_eq!(s["state"], "stopped");
    // A requested stop isn't reported as an exit code (143 after SIGTERM).
    assert_eq!(s["exit_status"], Value::Null);
    t.ok(&["start", "one"]);
    t.wait_for("the resumed agent", |t| {
        t.ok(&["show", "one"])
            .contains("fake agent: --resume conv-1")
    });

    // Delete needs confirmation; deleted sessions can be restored.
    let out = t.run(&["rm", "one"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    t.ok(&["rm", "--yes", "one"]);
    assert!(t.session("one", false).is_none());
    assert_eq!(t.session("one", true).unwrap()["conversation_id"], "conv-1");
    t.ok(&["restore", "one"]);
    let s = t.session("one", false).unwrap();
    assert_eq!(
        (s["state"].as_str(), s["conversation_id"].as_str()),
        (Some("stopped"), Some("conv-1"))
    );
    assert!(
        !t.run(&["project", "rm", "demo"]).status.success(),
        "project with a live session was removed"
    );
}

#[test]
fn a_failing_env_file_leaves_a_stopped_session_with_the_error() {
    let Some(t) = TestEnv::new("envfail") else {
        return;
    };
    fs::write(t.path("proj/.agent-roster.env"), "TOKEN=\"$(false)\"\n").unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "bad"]);
    t.wait_for("stopped", |t| {
        t.session("bad", false).unwrap()["state"] == "stopped"
    });
    assert_eq!(t.session("bad", false).unwrap()["exit_status"], 1);
    let screen = t.ok(&["show", "bad"]);
    assert!(
        screen.contains("roster: preparing the environment failed"),
        "{screen}"
    );
    assert!(!screen.contains("fake agent"), "{screen}");
}

#[test]
fn sessions_survive_a_lost_tmux_server_as_stopped() {
    let Some(t) = TestEnv::new("reboot") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo"]);
    let name = t.session("claude-1", false).unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();
    Command::new("tmux")
        .args(["-L", &t.socket, "kill-server"])
        .output()
        .unwrap();
    assert_eq!(t.session(&name, false).unwrap()["state"], "stopped");
    t.ok(&["start", &name]);
    t.wait_for("the restarted agent", |t| {
        t.ok(&["show", &name]).contains("fake agent")
    });
}

#[test]
fn report_is_silent_and_always_succeeds() {
    let Some(t) = TestEnv::new("report") else {
        return;
    };
    for out in [
        t.run(&["report"]),
        t.run(&["report", "idle"]),
        t.report("nope", "idle", "not json"),
        t.report("nope", "--bogus-flag", ""),
    ] {
        assert!(out.status.success());
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    }
    // A hook never creates the database.
    assert!(!t.db().exists());
}

#[test]
fn addressing_and_validation() {
    let Some(t) = TestEnv::new("addr") else {
        return;
    };
    t.ok(&["project", "add", "a", t.path("proj").to_str().unwrap()]);
    fs::create_dir_all(t.path("other")).unwrap();
    t.ok(&["project", "add", "b", t.path("other").to_str().unwrap()]);
    assert!(
        !t.run(&["project", "add", "a", t.path("proj").to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        !t.run(&[
            "project",
            "add",
            "bad/name",
            t.path("proj").to_str().unwrap()
        ])
        .status
        .success()
    );
    t.ok(&["new", "a", "--name", "same"]);
    t.ok(&["new", "b", "--name", "same"]);
    assert!(!t.run(&["new", "a", "--name", "same"]).status.success());

    let out = t.run(&["show", "same"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("matches several sessions"));
    t.ok(&["read", "a/same"]);
    let id = t.session("same", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    t.ok(&["unread", &id[..5]]);
    assert!(!t.run(&["show", "nothing"]).status.success());
}

/// Stand-in for an agent without hooks: replies to each line half a second later.
const GENERIC_AGENT: &str = r#"#!/bin/sh
echo "generic agent ready"
while read -r line; do
  sleep 0.5
  echo "reply: $line"
done
"#;

#[test]
fn generic_agents_are_tracked_by_their_screen() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("generic") else {
        return;
    };
    let agent = t.path("generic-agent");
    fs::write(&agent, GENERIC_AGENT).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[agents.echo]\ncommand = \"{}\"\nidle_after = 1\n",
        agent.display()
    ));
    fs::write(&config, text).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);

    let out = t.run(&["new", "demo", "--agent", "nope"]);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("unknown agent \"nope\" (available: claude, echo)")
    );

    t.ok(&["new", "demo", "--agent", "echo", "--name", "g"]);
    let s = t.session("g", false).unwrap();
    assert_eq!(s["conversation_id"], Value::Null);
    let id = s["id"].as_str().unwrap().to_string();

    // Starting up ends idle, and isn't a turn to read.
    t.wait_for("idle after startup", |t| {
        t.session("g", false).unwrap()["state"] == "idle"
    });
    assert_eq!(t.session("g", false).unwrap()["unread"], false);

    // Typing to it (as if attached elsewhere) after the startup grace: working, then
    // idle with a turn to read once the reply has been still for idle_after.
    std::thread::sleep(std::time::Duration::from_secs(4));
    let target = format!("=r-{id}:");
    Command::new("tmux")
        .args([
            "-L",
            &t.socket,
            "send-keys",
            "-t",
            &target,
            "hello",
            "Enter",
        ])
        .output()
        .unwrap();
    t.wait_for("the reply", |t| {
        t.ok(&["show", "g"]).contains("reply: hello")
    });
    t.wait_for("idle and unread", |t| {
        let s = t.session("g", false).unwrap();
        s["state"] == "idle" && s["unread"] == true
    });
    t.ok(&["read", "g"]);
    assert_eq!(t.session("g", false).unwrap()["unread"], false);

    // It can't resume, so restarting or starting it again asks first.
    let out = t.run(&["restart", "g"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("can't resume its conversation") && err.contains("--yes"),
        "{err}"
    );
    t.ok(&["restart", "--yes", "g"]);
    t.ok(&["stop", "g"]);
    assert!(!t.run(&["start", "g"]).status.success());
    assert!(!t.run(&["attach", "g"]).status.success());
    t.ok(&["start", "--yes", "g"]);
    t.wait_for("the new conversation", |t| {
        t.ok(&["show", "g"]).contains("generic agent ready")
    });
}

#[test]
fn config_changes_reach_a_running_server() {
    let Some(t) = TestEnv::new("reload") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo"]);
    let root_keys = || {
        let out = Command::new("tmux")
            .args(["-L", &t.socket, "list-keys", "-T", "root"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(root_keys().contains("C-q"));

    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{text}[keys]\ndetach = \"C-x\"\n")).unwrap();
    t.ok(&["ls"]);
    let keys = root_keys();
    assert!(keys.contains("C-x") && !keys.contains("C-q"), "{keys}");

    // An edit to the user's own tmux.conf is applied too.
    fs::write(
        t.config_dir().join("tmux.conf"),
        "bind -n F9 display-message hi\n",
    )
    .unwrap();
    t.ok(&["ls"]);
    assert!(root_keys().contains("F9"));
}
