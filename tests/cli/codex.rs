//! Codex: reporting through the hooks it's given, and its conversations adopted.

use std::fs;
use std::process::Command;

use serde_json::{Value, json};

use crate::common::TestEnv;

/// Stand-in for codex, as codex's source says it behaves: its hooks report
/// nothing before its first turn, which reports `SessionStart` first; a
/// prompt after `--` starts that turn at once; `resume` keeps the id given,
/// `fork` and a new conversation get one of their own; a `clear` line starts
/// a new conversation, reported with its first turn. It keeps a rollout like
/// codex's.
const FAKE_CODEX: &str = include_str!("../fakes/codex.sh");

/// A codex stand-in in `t`, as `[agents.codex]`'s command, its agent the default.
pub fn with_fake_codex(t: &TestEnv) {
    use std::os::unix::fs::PermissionsExt;
    let codex = t.path("fake-codex");
    fs::write(&codex, FAKE_CODEX).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "default_agent = \"codex\"\n{text}\n[agents.codex]\ncommand = \"{}\"\n",
            codex.display()
        ),
    )
    .unwrap();
}

/// Types `text` into session `name`'s agent and presses Enter, as you would.
fn type_into(t: &TestEnv, name: &str, text: &str) {
    let id = t.session(name, false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let target = format!("=r-{id}:");
    for keys in [vec!["-l", text], vec!["Enter"]] {
        Command::new("tmux")
            .args(["-L", &t.socket, "send-keys", "-t", &target])
            .args(keys)
            .output()
            .unwrap();
    }
}

#[test]
fn codex_reports_through_the_hooks_it_is_given() {
    let Some(t) = TestEnv::new("codex") else {
        return;
    };
    with_fake_codex(&t);
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    // An effort codex has no word for.
    assert!(
        !t.run(&["new", "demo", "--agent", "codex", "--effort", "extreme"])
            .status
            .success()
    );

    // The first prompt goes on its command line, never typed in.
    let out = t.ok(&[
        "new",
        "demo",
        "--name",
        "c",
        "--model",
        "fake-model-1",
        "--effort",
        "ultra",
        "--prompt",
        "fix the tests",
    ]);
    assert!(out.contains("given on its command line"), "{out}");
    t.wait_for("the first prompt's turn", |t| {
        let s = t.session("c", false).unwrap();
        s["state"] == "idle" && s["unread"] == true
    });
    let s = t.session("c", false).unwrap();
    assert_eq!(s["pending_prompt"], false);
    let conversation = s["conversation_id"].as_str().unwrap().to_string();
    // Codex is told no id: it names its conversation and reports it back.
    assert_eq!(conversation.len(), 36, "{conversation}");
    // Its arguments as the stand-in echoes them, however the screen wraps
    // them (roster's path is in them, as long as it is).
    let screen = t.ok(&["show", "c"]).replace('\n', "");
    assert!(
        screen.contains("--no-daemon")
            && screen.contains(r#"-c model="fake-model-1""#)
            && screen.contains(r#"-c model_reasoning_effort="ultra""#)
            && screen.contains("hooks.SessionStart=")
            && screen.contains("-- fix the tests")
            && screen.contains("turn: fix the tests"),
        "{screen}"
    );
    assert!(
        !screen.contains("got: fix the tests") && !screen.contains("dangerously"),
        "{screen}"
    );
    assert_eq!(
        (&s["agent"], &s["running_model"], &s["effort"]),
        (
            &Value::from("codex"),
            &Value::from("fake-model-1"),
            &Value::from("ultra")
        )
    );

    // Statistics from its rollout; what a subscription costs it doesn't say.
    let info = t.ok(&["info", "c"]);
    assert!(
        info.contains("1 prompt") && info.contains("context 115/400k") && info.contains("5 out"),
        "{info}"
    );

    // Codex names its conversation itself, from its first prompt: that's its
    // title, and roster makes none.
    t.wait_for("codex's name for it", |t| {
        t.session("c", false).unwrap()["title"] == "named: fix the tests"
    });

    // It resumes: a restart opens the same conversation, without asking, and
    // by its id even when its file isn't where it was (codex finds it, or
    // says it can't): never a new conversation instead.
    t.ok(&["restart", "--yes", "c"]);
    t.wait_for("the restart", |t| {
        t.ok(&["show", "c"])
            .contains(&format!("resume {conversation}"))
    });
    fs::remove_file(s["transcript_path"].as_str().unwrap()).unwrap();
    t.ok(&["restart", "--yes", "c"]);
    t.wait_for("the restart without its file", |t| {
        let screen = t.ok(&["show", "c"]);
        screen.contains(&format!("resume {conversation}")) && !screen.contains("turn:")
    });

    // Cleared, codex starts a conversation of its own, reported with its
    // first turn: the session follows.
    type_into(&t, "c", "clear");
    type_into(&t, "c", "after clearing");
    t.wait_for("the new conversation", |t| {
        let s = t.session("c", false).unwrap();
        s["conversation_id"] != conversation.as_str() && s["state"] == "idle"
    });
    // Named anew, from its own first prompt.
    t.wait_for("codex's name for the new one", |t| {
        t.session("c", false).unwrap()["title"] == "named: after clearing"
    });

    // Without a first prompt: nothing typed in, nothing known until you give
    // it one; it settles to idle from its screen.
    t.ok(&["new", "demo", "--name", "quiet"]);
    t.wait_for("idle, untouched", |t| {
        t.session("quiet", false).unwrap()["state"] == "idle"
    });
    let s = t.session("quiet", false).unwrap();
    assert_eq!(s["conversation_id"], Value::Null);
    assert!(!t.ok(&["show", "quiet"]).contains("turn:"));
    // Not knowing its conversation, a restart would begin a new one: it asks.
    let out = t.run(&["restart", "quiet"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("hasn't said which conversation")
            && err.contains("--yes"),
        "{err}"
    );
    type_into(&t, "quiet", "now a task");
    // Its id comes with the turn's first report; the turn's end, which marks
    // it unread, comes after (its start, codex's SessionStart, doesn't).
    t.wait_for("its conversation, from its first turn", |t| {
        let s = t.session("quiet", false).unwrap();
        s["conversation_id"].is_string() && s["state"] == "idle" && s["unread"] == true
    });
    // Started on codex's own effort, it shows what codex says it ran with.
    assert_eq!(
        t.session("quiet", false).unwrap()["running_effort"],
        "medium"
    );
    // Known, it resumes without asking (idle: a working one would ask).
    t.ok(&["restart", "quiet"]);
}

#[test]
fn a_first_prompt_longer_than_a_tmux_command_line_reaches_codex() {
    let Some(t) = TestEnv::new("codex-long") else {
        return;
    };
    with_fake_codex(&t);
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    // About 40 KB (tmux takes a command line of about 16 KB), a `;` last.
    let prompt = format!("{}fix it;", "the tests ".repeat(4_000));
    t.ok(&["new", "demo", "--name", "l", "--prompt", &prompt]);
    t.wait_for("its turn, from that prompt", |t| {
        let s = t.session("l", false).unwrap();
        s["transcript_path"]
            .as_str()
            .and_then(|p| fs::read_to_string(p).ok())
            .is_some_and(|rollout| rollout.contains(&format!("\"text\":\"{prompt}\"")))
    });
    // Its file, read, is gone.
    let left: Vec<_> = fs::read_dir(t.path("state/agent-roster"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("first-prompt-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_first_prompts_file_no_launch_read_goes_at_the_next_start() {
    let Some(t) = TestEnv::new("codex-sweep") else {
        return;
    };
    with_fake_codex(&t);
    let files = |t: &TestEnv| {
        fs::read_dir(t.path("state/agent-roster"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("first-prompt-"))
            .count()
    };
    // A shell that isn't there: no launch script runs to read the prompt.
    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    let (shell, none) = ("shell = \"/bin/sh\"", "shell = \"/nonexistent/sh\"");
    assert!(text.contains(shell), "{text}");
    fs::write(&config, text.replace(shell, none)).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "s", "--prompt", "fix it"]);
    assert_eq!(files(&t), 1);
    t.wait_for("its agent gone", |t| {
        t.session("s", false).unwrap()["state"] == "stopped"
    });
    // With a shell again, starting it once more takes it away (the prompt
    // kept, never seen taken).
    fs::write(&config, text).unwrap();
    t.ok(&["start", "--no-prompt-again", "s"]);
    assert_eq!(files(&t), 0);
}

#[test]
fn codex_conversations_can_be_adopted() {
    let Some(t) = TestEnv::new("codex-adopt") else {
        return;
    };
    with_fake_codex(&t);
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);

    // Two conversations codex kept, started outside roster in the project's
    // directory, dated the way it files them.
    let folder = t
        .codex_dir()
        .join("sessions")
        .join("2026")
        .join("10")
        .join("21");
    fs::create_dir_all(&folder).unwrap();
    let (one, two) = (
        "01a10001-0000-4000-8000-000000000001",
        "01a10002-0000-4000-8000-000000000002",
    );
    for id in [one, two] {
        let lines = [
            serde_json::json!({"type": "session_meta", "payload": {"id": id, "cwd": t.path("proj"), "git": {"branch": "main"}}}),
            serde_json::json!({"type": "turn_context", "payload": {"model": "gpt-6.1-sol", "effort": "high"}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>not a prompt</environment_context>"}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["environments.environment_context"]}}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "fix it"}], "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["user.text"]}}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(
            folder.join(format!("rollout-2026-10-21T06-00-00-{id}.jsonl")),
            text,
        )
        .unwrap();
    }
    fs::write(
        t.codex_dir().join("session_index.jsonl"),
        format!(
            r#"{{"id":"{one}","thread_name":"Fixing it","updated_at":"2026-10-21T06:05:00Z"}}"#,
        ) + "\n",
    )
    .unwrap();

    let list = t.ok(&["adopt", "--list"]);
    // Listed by the short id `roster adopt` takes.
    assert!(
        list.contains(&one[..8]) && list.contains(" codex ") && list.contains("Fixing it"),
        "{list}"
    );

    // Adopted, it's a codex session in its project that resumes it.
    t.ok(&["adopt", one, "--name", "a"]);
    let s = t.session("a", false).unwrap();
    assert_eq!(
        (&s["agent"], &s["conversation_id"]),
        (&Value::from("codex"), &Value::from(one))
    );
    t.wait_for("codex started", |t| {
        t.ok(&["show", "a"]).contains(&format!("resume {one}"))
    });
    assert!(!t.ok(&["show", "a"]).contains("fork"));
    assert!(!t.ok(&["adopt", "--list"]).contains(&one[..8]));

    // A copy forks the original, and does again until codex has reported the
    // copy's own conversation, which it resumes from then on.
    t.ok(&["adopt", two, "--fork", "--name", "b"]);
    // No id for it until codex reports the copy's: codex names it.
    assert_eq!(
        t.session("b", false).unwrap()["conversation_id"],
        Value::Null
    );
    t.wait_for("the fork", |t| {
        t.ok(&["show", "b"]).contains(&format!("fork {two}"))
    });
    t.ok(&["restart", "--yes", "b"]);
    t.wait_for("the fork again", |t| {
        t.ok(&["show", "b"]).contains(&format!("fork {two}"))
    });
    type_into(&t, "b", "carry on");
    t.wait_for("the copy's own conversation", |t| {
        t.session("b", false).unwrap()["conversation_id"].is_string()
    });
    let copy = t.session("b", false).unwrap()["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(copy != two, "{copy}");
    t.ok(&["restart", "--yes", "b"]);
    t.wait_for("the copy resumed", |t| {
        t.ok(&["show", "b"]).contains(&format!("resume {copy}"))
    });
}

#[test]
fn a_first_prompt_codex_exited_before_taking_is_kept_to_give_again() {
    let Some(t) = TestEnv::new("codex-again") else {
        return;
    };
    with_fake_codex(&t);
    let proj = t.path("proj");
    fs::create_dir_all(&proj).unwrap();
    t.ok(&["project", "add", "demo", proj.to_str().unwrap()]);
    // Declined at what it showed first: gone before its first turn.
    fs::write(proj.join(".exit-early"), "").unwrap();
    t.ok(&["new", "demo", "--name", "s", "--prompt", "fix it"]);
    t.wait_for("codex gone", |t| {
        t.session("s", false).unwrap()["state"] == "stopped"
    });
    let s = t.session("s", false).unwrap();
    assert_eq!(
        (&s["given_prompt"], &s["pending_prompt"]),
        (&json!(true), &json!(false))
    );
    fs::remove_file(proj.join(".exit-early")).unwrap();
    // A start giving it again that fails (its agent gone from the config)
    // leaves it kept as it was, not to be given by a plain start.
    let config = t.config_dir().join("config.toml");
    let with_codex = fs::read_to_string(&config).unwrap();
    let without: String = (with_codex.lines())
        .filter(|l| {
            !(*l == "default_agent = \"codex\""
                || *l == "[agents.codex]"
                || l.starts_with("command = ") && l.contains("fake-codex"))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&config, without).unwrap();
    let out = t.run(&["start", "--yes", "--prompt-again", "s"]);
    assert!(!out.status.success());
    fs::write(&config, &with_codex).unwrap();
    let s = t.session("s", false).unwrap();
    assert_eq!(
        (&s["state"], &s["given_prompt"], &s["pending_prompt"]),
        (&json!("stopped"), &json!(true), &json!(false))
    );
    // Started without a word on it (and no terminal to ask in), it's said
    // what to pass; told not to, it goes without it, and says it's kept.
    let out = t.run(&["start", "--yes", "s"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        said.contains("exited before it said it took its first prompt, \"fix it\"")
            && said.contains("--prompt-again")
            && said.contains("--no-prompt-again"),
        "{said}"
    );
    let out = t.ok(&["start", "--no-prompt-again", "s"]);
    assert!(
        out.contains("is kept: starting it again asks again"),
        "{out}"
    );
    t.wait_for("codex up", |t| t.ok(&["show", "s"]).contains("fake codex:"));
    assert!(!t.ok(&["show", "s"]).contains("turn: fix it"));
    t.ok(&["stop", "--yes", "s"]);
    // Asked for, it's given again; taken, it's no longer kept.
    t.ok(&["start", "--prompt-again", "s"]);
    t.wait_for("its first turn", |t| {
        t.ok(&["show", "s"]).contains("turn: fix it")
    });
    t.wait_for("taken", |t| {
        t.session("s", false).unwrap()["given_prompt"] == false
    });
    // Nothing to give again now.
    t.ok(&["stop", "--yes", "s"]);
    assert!(
        !t.run(&["start", "--yes", "--prompt-again", "s"])
            .status
            .success()
    );
    // Nor in one never given a first prompt, whose conversation isn't known
    // (it exited before saying): said so, rather than asked about starting a
    // new one.
    fs::write(proj.join(".exit-early"), "").unwrap();
    t.ok(&["new", "demo", "--name", "np"]);
    t.wait_for("codex gone", |t| {
        t.session("np", false).unwrap()["state"] == "stopped"
    });
    let out = t.run(&["start", "--prompt-again", "np"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(said.contains("has no first prompt to give again"), "{said}");
}

#[test]
fn a_sessions_folder_is_trusted_in_its_agent_when_asked() {
    let Some(t) = TestEnv::new("codex-trust") else {
        return;
    };
    with_fake_codex(&t);
    let proj = t.path("proj");
    t.ok(&["project", "add", "demo", proj.to_str().unwrap()]);
    let dir = fs::canonicalize(&proj).unwrap();
    let key = dir.to_str().unwrap();
    let codex_config = t.codex_dir().join("config.toml");
    let claude_config = t.claude_dir().join(".claude.json");
    fs::create_dir_all(t.claude_dir()).unwrap();
    fs::write(&claude_config, r#"{"numStartups":1}"#).unwrap();
    let trusted = |config: &std::path::Path| -> bool {
        fs::read_to_string(config)
            .ok()
            .and_then(|text| toml::from_str::<toml::Table>(&text).ok())
            .and_then(|c| {
                let level = c.get("projects")?.get(key)?.get("trust_level")?;
                level.as_str().map(|l| l == "trusted")
            })
            .unwrap_or(false)
    };
    // Not by default.
    t.ok(&["new", "demo", "--name", "off"]);
    t.wait_for("codex up", |t| {
        t.ok(&["show", "off"]).contains("fake codex:")
    });
    assert!(!codex_config.exists());
    // Asked: as each starts, in each one's own config.
    t.ok(&["config", "set", "trust_project_folders", "true"]);
    t.ok(&["new", "demo", "--name", "on"]);
    t.wait_for("trusted in codex", |_| trusted(&codex_config));
    t.wait_for("codex up", |t| {
        t.ok(&["show", "on"]).contains("fake codex:")
    });
    t.ok(&["new", "demo", "--agent", "claude", "--name", "claude"]);
    t.wait_for("trusted in claude", |_| {
        fs::read(&claude_config)
            .ok()
            .and_then(|text| serde_json::from_slice::<serde_json::Value>(&text).ok())
            .is_some_and(|c| c["projects"][key]["hasTrustDialogAccepted"] == json!(true))
    });
    let claude: serde_json::Value =
        serde_json::from_slice(&fs::read(&claude_config).unwrap()).unwrap();
    assert_eq!(claude["numStartups"], json!(1));
    // Its own config, as the project's env file has it, not roster's.
    let elsewhere = t.path("codex-elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(
        proj.join(".agent-roster.env"),
        format!("CODEX_HOME='{}'\n", elsewhere.display()),
    )
    .unwrap();
    fs::write(&codex_config, "").unwrap();
    t.ok(&["new", "demo", "--name", "elsewhere"]);
    t.wait_for("trusted where codex looks", |_| {
        trusted(&elsewhere.join("config.toml"))
    });
    assert!(!trusted(&codex_config));
}
