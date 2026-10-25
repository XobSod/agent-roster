//! An agent without an adapter, tracked by its screen alone.

use std::fs;
use std::process::Command;

use serde_json::Value;

use crate::common::TestEnv;

/// Stand-in for an agent without hooks: replies to each line half a second later.
const GENERIC_AGENT: &str = include_str!("../fakes/generic.sh");

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
    // (pi among them if it's installed here.)
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown agent \"nope\" (available: claude, ") && err.contains(" echo)"),
        "{err}"
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
    // Just started, it's working: stopping it asks, as restarting does.
    let out = t.run(&["stop", "g"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("is working. Stop it?") && err.contains("--yes"),
        "{err}"
    );
    t.ok(&["stop", "--yes", "g"]);
    assert!(!t.run(&["start", "g"]).status.success());
    assert!(!t.run(&["attach", "g"]).status.success());
    t.ok(&["start", "--yes", "g"]);
    t.wait_for("the new conversation", |t| {
        t.ok(&["show", "g"]).contains("generic agent ready")
    });
}

#[test]
fn an_agents_arguments_reach_it_as_given_semicolons_too() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("args") else {
        return;
    };
    // Shows each argument it got, bracketed, then waits.
    let agent = t.path("args-agent");
    fs::write(
        &agent,
        "#!/bin/sh\nprintf 'got:'; printf ' [%s]' \"$@\"; echo; exec cat\n",
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[agents.args]\ncommand = \"{}\"\nargs = [\";\", \"a;\", \"b\"]\n",
        agent.display()
    ));
    fs::write(&config, text).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--agent", "args", "--name", "a"]);
    let shows = |t: &TestEnv| t.ok(&["show", "a"]).contains("got: [;] [a;] [b]");
    t.wait_for("its arguments", shows);
    // Started again, by respawning its pane, likewise.
    t.ok(&["restart", "--yes", "a"]);
    t.wait_for("its arguments again", |t| {
        t.ok(&["show", "a"]).matches("got: [;] [a;] [b]").count() == 1 && shows(t)
    });
}
