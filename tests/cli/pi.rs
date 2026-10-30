//! pi: reporting through roster's extension, and its conversations adopted.

use std::fs;
use std::process::Command;

use serde_json::Value;

use crate::common::TestEnv;

/// Stand-in for pi: reports to roster the way roster's extension does, and
/// keeps a session file like pi's. A line `name …` names its session.
const FAKE_PI: &str = include_str!("../fakes/pi.sh");

#[test]
fn pi_reports_through_rosters_extension() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("pi") else {
        return;
    };
    let pi = t.path("fake-pi");
    fs::write(&pi, FAKE_PI).unwrap();
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!("[agents.pi]\ncommand = \"{}\"\n", pi.display()));
    // pi is what new sessions get unless they ask for another.
    fs::write(&config, format!("default_agent = \"pi\"\n{text}")).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    assert!(
        !t.run(&["new", "demo", "--agent", "pi", "--effort", "extreme"])
            .status
            .success()
    );

    // Started with its model, thinking level, conversation and roster's
    // extension; it reports its session, then gets the first prompt.
    t.ok(&[
        "new",
        "demo",
        "--name",
        "p",
        "--model",
        "fake-model-1",
        "--effort",
        "high",
        "--prompt",
        "fix the tests",
    ]);
    t.wait_for("the first prompt's turn", |t| {
        let s = t.session("p", false).unwrap();
        s["state"] == "idle" && s["unread"] == true
    });
    let s = t.session("p", false).unwrap();
    let conversation = s["conversation_id"].as_str().unwrap().to_string();
    let screen = t.ok(&["show", "p"]);
    let extension = t.path("state/agent-roster/pi/agent-roster.ts");
    assert!(
        screen.contains(&format!(
            "--model fake-model-1 --thinking high --session-id {conversation} -e {}",
            extension.display()
        )) && screen.contains("with the extension")
            && screen.contains("got: fix the tests"),
        "{screen}"
    );
    assert!(
        fs::read_to_string(&extension)
            .unwrap()
            .contains("report(\"session\"")
    );
    assert_eq!(
        (&s["agent"], &s["running_model"], &s["effort"]),
        (
            &Value::from("pi"),
            &Value::from("fake-model-1"),
            &Value::from("high")
        )
    );

    // Statistics from its session file, with the context window it reported.
    let info = t.ok(&["info", "p"]);
    assert!(
        info.contains("1 prompt") && info.contains("context 115/200k") && info.contains("$0.50"),
        "{info}"
    );

    // pi names no task itself: after its first turn roster has it make a
    // title (the stand-in answers with its arguments).
    t.wait_for("the automatic title", |t| {
        t.session("p", false).unwrap()["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("fake pi: -p --no-session"))
    });

    // Naming its session (/name) titles it instead.
    let target = format!("=r-{}:", s["id"].as_str().unwrap());
    Command::new("tmux")
        .args([
            "-L",
            &t.socket,
            "send-keys",
            "-t",
            &target,
            "-l",
            "name Fix the parser",
        ])
        .output()
        .unwrap();
    Command::new("tmux")
        .args(["-L", &t.socket, "send-keys", "-t", &target, "Enter"])
        .output()
        .unwrap();
    t.wait_for("the title, and the turn's end", |t| {
        let s = t.session("p", false).unwrap();
        s["title"] == "Fix the parser" && s["state"] == "idle"
    });

    // It resumes: a restart opens the same conversation, without asking.
    t.ok(&["restart", "p"]);
    t.wait_for("the restart", |t| {
        let screen = t.ok(&["show", "p"]);
        screen.contains("fake pi:") && !screen.contains("got:")
    });
    assert!(
        t.ok(&["show", "p"])
            .contains(&format!("--session-id {conversation}"))
    );
    assert_eq!(
        t.session("p", false).unwrap()["conversation_id"],
        conversation.as_str()
    );

    // pi makes its title itself (the stand-in echoes its arguments).
    let title = t.ok(&["retitle", "p"]);
    assert!(
        title.starts_with("fake pi: -p --no-session --no-tools"),
        "{title}"
    );

    // Its session file gone after its turns, pi would begin a new one on its
    // id: starting it asks first.
    t.ok(&["stop", "p"]);
    fs::remove_file(t.path(&format!("proj/.pi-{conversation}.jsonl"))).unwrap();
    let out = t.run(&["start", "p"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("demo/p (pi) can't resume its conversation: Pi no longer has it"),
        "{err}"
    );
}

#[test]
fn pi_conversations_can_be_adopted() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("pi-adopt") else {
        return;
    };
    let pi = t.path("fake-pi");
    fs::write(&pi, FAKE_PI).unwrap();
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!("[agents.pi]\ncommand = \"{}\"\n", pi.display()));
    fs::write(&config, text).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    // Two conversations pi kept, started outside roster in the project's directory.
    let folder = t.pi_dir().join("sessions").join("--proj--");
    fs::create_dir_all(&folder).unwrap();
    for (id, name) in [("pc-one", "Parser work"), ("pc-two", "Docs")] {
        let lines = [
            serde_json::json!({"type": "session", "version": 3, "id": id, "cwd": t.path("proj")}),
            serde_json::json!({"type": "message", "id": "a", "message": {"role": "user", "content": [{"type": "text", "text": "fix it"}]}}),
            serde_json::json!({"type": "session_info", "id": "b", "name": name}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(
            folder.join(format!("2026-10-22T06-00-00-000Z_{id}.jsonl")),
            text,
        )
        .unwrap();
    }
    let list = t.ok(&["adopt", "--list"]);
    assert!(
        list.contains("pc-one") && list.contains(" pi ") && list.contains("Parser work"),
        "{list}"
    );

    // Adopted, it's a pi session in its project that opens that conversation.
    t.ok(&["adopt", "pc-one", "--name", "a"]);
    let s = t.session("a", false).unwrap();
    assert_eq!(
        (&s["agent"], &s["conversation_id"]),
        (&Value::from("pi"), &Value::from("pc-one"))
    );
    t.wait_for("pi started", |t| t.ok(&["show", "a"]).contains("fake pi:"));
    let screen = t.ok(&["show", "a"]);
    assert!(
        screen.contains("--session-id pc-one") && !screen.contains("--fork"),
        "{screen}"
    );
    assert!(!t.ok(&["adopt", "--list"]).contains("pc-one"));

    // A copy forks the original into a conversation of its own.
    t.ok(&["adopt", "pc-two", "--fork", "--name", "b"]);
    let copy = t.session("b", false).unwrap()["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(copy, "pc-two");
    t.wait_for("the copy started", |t| {
        t.ok(&["show", "b"]).contains("fake pi:")
    });
    let screen = t.ok(&["show", "b"]);
    assert!(
        screen.contains(&format!("--fork pc-two --session-id {copy}")),
        "{screen}"
    );

    // Outside any project: the project made for it runs pi, not the default.
    let elsewhere = t.path("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let folder = t.pi_dir().join("sessions").join("--elsewhere--");
    fs::create_dir_all(&folder).unwrap();
    let lines = [
        serde_json::json!({"type": "session", "version": 3, "id": "pc-three", "cwd": elsewhere}),
        serde_json::json!({"type": "message", "id": "a", "message": {"role": "user", "content": [{"type": "text", "text": "plan it"}]}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(folder.join("2026-10-22T07-00-00-000Z_pc-three.jsonl"), text).unwrap();
    t.ok(&["adopt", "pc-three", "--name", "c"]);
    let projects = t.ok(&["project", "ls"]);
    let made = projects
        .lines()
        .find(|l| l.starts_with("elsewhere"))
        .unwrap();
    assert!(made.trim_end().ends_with("pi"), "{projects}");
}
