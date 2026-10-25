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

    // Archiving needs confirmation; archived sessions can be restored.
    let out = t.run(&["archive", "one"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    t.ok(&["archive", "--yes", "one"]);
    assert!(t.session("one", false).is_none());
    let archived = t.session("one", true).unwrap();
    assert_eq!(archived["conversation_id"], "conv-1");
    assert!(archived["archived_at"].is_i64());
    t.ok(&["restore", "one"]);
    let s = t.session("one", false).unwrap();
    assert_eq!(
        (s["state"].as_str(), s["conversation_id"].as_str()),
        (Some("stopped"), Some("conv-1"))
    );
    // A project with a live session can't be removed, so that's said before
    // anything is asked.
    let out = t.run(&["project", "rm", "demo"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("archive its sessions first")
            && !err.contains("--yes"),
        "{err}"
    );

    // Deleting for good: only archived sessions, and it asks first.
    let out = t.run(&["rm", "--yes", "one"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't archived"));
    t.ok(&["archive", "--yes", "one"]);
    assert!(!t.run(&["rm", "one"]).status.success());
    let out = t.ok(&["rm", "--yes", "one"]);
    assert!(out.contains("for good"), "{out}");
    assert!(t.session("one", false).is_none() && t.session("one", true).is_none());
    // Removing a project asks too: its archived sessions would go with it.
    t.ok(&["new", "demo", "--name", "two"]);
    t.ok(&["archive", "--yes", "two"]);
    let out = t.run(&["project", "rm", "demo"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("Its archived session goes for good")
            && err.contains("--yes"),
        "{err}"
    );
    assert!(t.session("two", true).is_some());
    let out = t.ok(&["project", "rm", "--yes", "demo"]);
    assert!(out.contains("with archived session two"), "{out}");
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
fn works_in_a_locale_without_utf8() {
    let Some(t) = TestEnv::new("locale") else {
        return;
    };
    // Roster and so its tmux server and agents, all in the C locale.
    let run = |args: &[&str]| {
        let out = t
            .command(args)
            .env("LC_ALL", "C")
            .env_remove("LC_CTYPE")
            .env_remove("LANG")
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    run(&["project", "add", "demo", t.path("proj").to_str().unwrap()]).unwrap();
    run(&["new", "demo", "--name", "one"]).unwrap();
    t.wait_for("the agent's output", |_| {
        run(&["show", "one"]).is_some_and(|s| s.contains("fake agent"))
    });
    let ls: Value = serde_json::from_str(&run(&["ls", "--json"]).unwrap()).unwrap();
    assert_ne!(ls[0]["state"], "stopped", "{ls}");
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
    // Commands that also look among archived sessions say so too.
    let out = t.run(&["info", "same"]);
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

/// Stand-in for pi: reports to roster the way roster's extension does, and
/// keeps a session file like pi's. A line `name …` names its session.
const FAKE_PI: &str = r#"#!/bin/sh
id=; extension=; prev=
for a in "$@"; do
  case "$prev" in --session-id) id=$a ;; -e) extension=$a ;; esac
  prev=$a
done
echo "fake pi: $*"
[ -f "$extension" ] && echo "with the extension"
file="$PWD/.pi-$id.jsonl"
report() { printf '%s' "$2" | "$ROSTER_EXE" report "$1"; }
report session "{\"conversation_id\":\"$id\",\"transcript_path\":\"$file\",\"model\":\"fake-model-1\",\"effort\":\"high\",\"title\":null,\"context_window\":200000,\"started\":true}"
while read -r line; do
  report prompt '{}'
  echo "got: $line"
  printf '{"type":"message","message":{"role":"user","content":[{"type":"text","text":"%s"}]}}\n' "$line" >> "$file"
  printf '{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":5,"cacheRead":100,"cost":{"total":0.5}}}}\n' >> "$file"
  case "$line" in "name "*) report info "{\"title\":\"${line#name }\"}" ;; esac
  report idle '{}'
done
"#;

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
}

/// Stand-in for codex, as codex's source says it behaves: its hooks report
/// nothing before its first turn, which reports `SessionStart` first; a
/// prompt after `--` starts that turn at once; `resume` keeps the id given,
/// `fork` and a new conversation get one of their own; a `clear` line starts
/// a new conversation, reported with its first turn. It keeps a rollout like
/// codex's.
const FAKE_CODEX: &str = r#"#!/bin/sh
id=; sub=; prev=; model=; effort=; prompt=; rest=no; n=0
for a in "$@"; do
  if [ "$rest" = yes ]; then prompt=$a; continue; fi
  case "$a" in
    --) rest=yes ;;
    model=*) model=$(printf '%s' "${a#model=}" | tr -d '"') ;;
    model_reasoning_effort=*) effort=$(printf '%s' "${a#model_reasoning_effort=}" | tr -d '"') ;;
    resume|fork) sub=$a ;;
  esac
  case "$prev" in resume|fork) [ -z "$id" ] && id=$a ;; esac
  prev=$a
done
new_id() { n=$((n + 1)); printf '%08x-0000-4000-8000-%012x' "$$" "$n"; }
source=startup
case "$sub" in
  resume) source=resume ;;
  fork) source=fork; id=$(new_id) ;;
  *) id=$(new_id) ;;
esac
started=no
echo "fake codex: $*"
report() { printf '%s' "$2" | "$ROSTER_EXE" report "$1"; }
turn() {
  file="$PWD/.codex-$id.jsonl"
  pay="{\"session_id\":\"$id\",\"transcript_path\":\"$file\",\"cwd\":\"$PWD\",\"model\":\"${model:-fake-model-1}\",\"source\":\"$source\"}"
  if [ "$started" = no ]; then report session "$pay"; started=yes; fi
  echo "turn: $1"
  report prompt "$pay"
  printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$PWD" >> "$file"
  printf '{"type":"turn_context","payload":{"model":"%s","effort":"%s"}}\n' "${model:-fake-model-1}" "${effort:-medium}" >> "$file"
  printf '{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"%s"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}\n' "$1" >> "$file"
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}\n' >> "$file"
  printf '{"type":"token_usage_record","payload":{"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":115}}}\n' >> "$file"
  printf '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"output_tokens":5,"total_tokens":115},"last_token_usage":{"total_tokens":115},"model_context_window":400000}}}\n' >> "$file"
  report working "$pay"
  report idle "$pay"
}
[ -n "$prompt" ] && turn "$prompt"
while read -r line; do
  case "$line" in
    clear) id=$(new_id); source=clear; started=no; echo "cleared"; continue ;;
  esac
  echo "got: $line"
  turn "$line"
done
"#;

/// A codex stand-in in `t`, as `[agents.codex]`'s command, its agent the default.
fn with_fake_codex(t: &TestEnv) {
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
    t.ok(&[
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

    // Codex names no task: after its first turn roster has it make a title.
    t.wait_for("the automatic title", |t| {
        t.session("c", false).unwrap()["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("fake codex: exec"))
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
    t.wait_for("its conversation, from its first turn", |t| {
        t.session("quiet", false).unwrap()["conversation_id"].is_string()
    });
    // Known, it resumes without asking.
    t.ok(&["restart", "quiet"]);
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
    // Roster's own id for it, until codex reports the copy's.
    let made_up = t.session("b", false).unwrap()["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    t.wait_for("the fork", |t| {
        t.ok(&["show", "b"]).contains(&format!("fork {two}"))
    });
    t.ok(&["restart", "--yes", "b"]);
    t.wait_for("the fork again", |t| {
        t.ok(&["show", "b"]).contains(&format!("fork {two}"))
    });
    type_into(&t, "b", "carry on");
    t.wait_for("the copy's own conversation", |t| {
        t.session("b", false).unwrap()["conversation_id"] != made_up.as_str()
    });
    let copy = t.session("b", false).unwrap()["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(copy != two && copy != made_up, "{copy}");
    t.ok(&["restart", "--yes", "b"]);
    t.wait_for("the copy resumed", |t| {
        t.ok(&["show", "b"]).contains(&format!("resume {copy}"))
    });
}

#[test]
fn settings_change_config_toml_in_place() {
    let Some(t) = TestEnv::new("settings") else {
        return;
    };
    // The config directory a link into dotfiles, as many keep it.
    let dotfiles = t.path("dotfiles/agent-roster");
    fs::create_dir_all(dotfiles.parent().unwrap()).unwrap();
    fs::rename(t.config_dir(), &dotfiles).unwrap();
    std::os::unix::fs::symlink(&dotfiles, t.config_dir()).unwrap();
    let file = dotfiles.join("config.toml");
    let mut text = fs::read_to_string(&file).unwrap();
    text.insert_str(0, "# Mine.\n");
    fs::write(&file, &text).unwrap();

    let shown = t.ok(&["config"]);
    assert!(
        shown.contains("status_line    on") && shown.contains("default"),
        "{shown}"
    );
    t.ok(&["config", "set", "status_line", "off"]);
    t.ok(&["config", "set", "keys.session.stop", "y Delete"]);
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("# Mine.\n") && text.contains("status_line = false"),
        "{text}"
    );
    assert!(text.contains("stop = [\"y\", \"Delete\"]"), "{text}");
    assert!(
        fs::symlink_metadata(t.config_dir())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let shown = t.ok(&["config"]);
    assert!(
        shown.contains("status_line    off") && shown.contains("yours"),
        "{shown}"
    );
    // An invalid one is refused, and the file stays as it was.
    let out = t.run(&["config", "set", "keys.up", "Nope"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown key"),
        "{out:?}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), text);
    t.ok(&["config", "unset", "status_line"]);
    assert!(!fs::read_to_string(&file).unwrap().contains("status_line"));
    assert!(t.ok(&["config", "--defaults"]).contains("# [keys.session]"));

    // A setting you have, with what you wrote about it: changed, the comments
    // stay; taken out, its comment goes, and the agent's table stays.
    let top = "# Agents keep the terminal's size.\nfit_preview = \"never\"  # for now\n\n";
    let pi = "\n# pi on the normal screen.\n[agents.pi]\nargs = [\"--tui-mode\", \"regular\"]\n";
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("{top}{text}{pi}")).unwrap();
    t.ok(&["config", "set", "fit_preview", "auto"]);
    let changed = fs::read_to_string(&file).unwrap();
    assert!(
        changed.starts_with(
            "# Agents keep the terminal's size.\nfit_preview = \"auto\"  # for now\n\n# Mine.\n"
        ),
        "{changed}"
    );
    t.ok(&["config", "unset", "agents.pi.args"]);
    let changed = fs::read_to_string(&file).unwrap();
    assert!(
        changed.contains("# pi on the normal screen.\n[agents.pi]\n")
            && !changed.contains("regular"),
        "{changed}"
    );
    // A whole agent's settings ask first, so without a terminal: refused.
    let out = t.run(&["config", "unset", "agents.pi"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("Take pi's settings out of config.toml")
            && err.contains("Pass --yes to confirm."),
        "{err}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), changed);
    let out = t.ok(&["config", "unset", "agents.pi", "--yes"]);
    assert_eq!(out, "Took [agents.pi] out of config.toml\n");
    let changed = fs::read_to_string(&file).unwrap();
    assert!(!changed.contains("[agents.pi]"), "{changed}");
    // A table the file doesn't have: nothing to ask about, and nothing done.
    t.ok(&["config", "unset", "notes"]);
    assert_eq!(fs::read_to_string(&file).unwrap(), changed);
    // A back key tmux doesn't know, or that takes a key from the agents: not written.
    for key in ["Backspace", "q", "C-c"] {
        let out = t.run(&["config", "set", "keys.detach", key]);
        assert!(!out.status.success(), "{key}");
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), changed);
}

#[test]
fn an_agents_model_and_effort_are_its_new_sessions() {
    let Some(t) = TestEnv::new("agent-model") else {
        return;
    };
    fs::create_dir_all(t.path("big")).unwrap();
    t.ok(&["config", "set", "agents.claude.model", "sonnet"]);
    t.ok(&["config", "set", "agents.claude.effort", "low"]);
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&[
        "project",
        "add",
        "big",
        t.path("big").to_str().unwrap(),
        "--model",
        "opus",
    ]);
    let model = |name: &str| {
        let s = t.session(name, false).unwrap();
        (
            s["model"].as_str().map(String::from),
            s["effort"].as_str().map(String::from),
        )
    };
    let sonnet_low = (Some("sonnet".to_string()), Some("low".to_string()));
    t.ok(&["new", "demo", "--name", "a"]);
    assert_eq!(model("a"), sonnet_low);
    // The project's, or the session's own, come first.
    t.ok(&["new", "big", "--name", "b"]);
    assert_eq!(model("b"), (Some("opus".into()), Some("low".into())));
    t.ok(&[
        "new", "demo", "--name", "c", "--model", "haiku", "--effort", "default",
    ]);
    assert_eq!(model("c"), (Some("haiku".into()), Some("low".into())));
    // Renamed, it keeps its model; changed to the default, it gets this one.
    t.ok(&["edit", "b", "--name", "b2"]);
    assert_eq!(model("b2").0.as_deref(), Some("opus"));
    t.ok(&["edit", "b2", "--model", "default"]);
    assert_eq!(model("b2"), sonnet_low);
    // An effort the agent doesn't have isn't written.
    assert!(
        !t.run(&["config", "set", "agents.claude.effort", "minimal"])
            .status
            .success()
    );
    t.ok(&["config", "set", "agents.claude.effort", "default"]);
    assert!(!t.ok(&["config"]).contains("effort         low"));
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

    // One with an error is said once, and roster goes on: it isn't read
    // again until it changes, since that would run what it holds again.
    let user_conf = t.config_dir().join("tmux.conf");
    fs::write(
        &user_conf,
        "bind -n F10 display-message hi\nno-such-command\n",
    )
    .unwrap();
    let out = t.run(&["ls"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success()
            && err.contains("tmux.conf:2: unknown command: no-such-command")
            && err.contains("roster's tmux goes on without it"),
        "{err}"
    );
    let out = t.run(&["ls"]);
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    // A line tmux skips: the rest is run once, not at every roster command.
    fs::write(
        &user_conf,
        "set -ag @roster_test x\nset -g no-such-option 1\n",
    )
    .unwrap();
    let out = t.run(&["ls"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid option: no-such-option"),
        "{out:?}"
    );
    let out = t.run(&["ls"]);
    assert!(out.stderr.is_empty(), "{out:?}");
    let option = Command::new("tmux")
        .args(["-L", &t.socket, "show", "-gv", "@roster_test"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&option.stdout).trim(), "x");
    // Said by the next roster you run: not taken by one of roster's own in
    // the background, whose output goes nowhere, nor lost by a TUI that
    // can't start.
    fs::write(&user_conf, "no-such-command\n").unwrap();
    let out = t.run(&["deliver", "claude-1"]);
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    let out = t.run(&[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("unknown command: no-such-command")
            && err.contains("the TUI needs a terminal"),
        "{err}"
    );
    // Changing a setting meanwhile doesn't say it again.
    t.ok(&["config", "set", "agents.claude.title_model", "haiku"]);
    fs::write(&user_conf, "bind -n F11 display-message hi\n").unwrap();
    let out = t.run(&["ls"]);
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    assert!(root_keys().contains("F11"));
    let out = t.run(&["ls"]);
    assert!(out.stderr.is_empty(), "{out:?}");
}

#[test]
fn config_toml_opens_in_the_editor() {
    let Some(t) = TestEnv::new("edit-config") else {
        return;
    };
    let file = t.config_dir().join("config.toml");
    let editor = |name: &str, sed: &str| {
        let path = t.path(name);
        fs::write(&path, format!("#!/bin/sh\nsed -i '{sed}' \"$1\"\n")).unwrap();
        Command::new("chmod").arg("+x").arg(&path).status().unwrap();
        path
    };
    let edit = |editor: &std::path::Path| {
        t.command(&["config", "--edit"])
            .env("EDITOR", editor)
            .env_remove("VISUAL")
            .output()
            .unwrap()
    };
    let says = |out: &std::process::Output, said: &str| {
        String::from_utf8_lossy(&out.stdout).contains(said)
            || String::from_utf8_lossy(&out.stderr).contains(said)
    };
    let out = edit(&editor("on.sh", "1i status_line = false"));
    assert!(
        out.status.success() && says(&out, "config.toml saved"),
        "{out:?}"
    );
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .starts_with("status_line = false\n")
    );
    let out = edit(std::path::Path::new("true"));
    assert!(
        out.status.success() && says(&out, "config.toml unchanged"),
        "{out:?}"
    );

    // A mistake is said, and --edit is still the way to fix it.
    let out = edit(&editor("bad.sh", "1i no_such_setting = 1"));
    assert!(
        !out.status.success() && says(&out, "no_such_setting"),
        "{out:?}"
    );
    let out = edit(&editor("fix.sh", "/no_such_setting/d"));
    assert!(
        out.status.success() && says(&out, "config.toml saved"),
        "{out:?}"
    );
    assert!(t.ok(&["config"]).contains("status_line"));

    // An editor that fails is said.
    let out = edit(std::path::Path::new("false"));
    assert!(
        !out.status.success() && says(&out, "the editor exited with an error"),
        "{out:?}"
    );

    // With no file yet, it's made from the template first.
    fs::remove_file(&file).unwrap();
    let out = edit(std::path::Path::new("true"));
    assert!(
        out.status.success() && says(&out, "config.toml made, every setting commented out"),
        "{out:?}"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        t.ok(&["config", "--defaults"])
    );

    // Not with a change: one or the other.
    let out = t.run(&["config", "--edit", "set", "status_line", "on"]);
    assert!(!out.status.success(), "{out:?}");

    // Fixed with --edit, it's in use at once, in a running server too.
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo"]);
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        format!("{text}[keys]\ndetach = \"C-b\"\nno_such_key = 1\n"),
    )
    .unwrap();
    let out = edit(&editor("fix-keys.sh", "/no_such_key/d"));
    assert!(
        out.status.success() && says(&out, "config.toml saved"),
        "{out:?}"
    );
    let keys = Command::new("tmux")
        .args(["-L", &t.socket, "list-keys", "-T", "root"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&keys.stdout).contains("C-b"),
        "{keys:?}"
    );

    // Roster failing to open for another reason isn't config.toml's to fix.
    let ran = t.path("ran");
    let marks = t.path("marks.sh");
    fs::write(&marks, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&marks)
        .status()
        .unwrap();
    fs::write(t.db(), "not a database, not at all").unwrap();
    let out = edit(&marks);
    assert!(!out.status.success() && !ran.exists(), "{out:?}");
}

#[test]
fn a_tmux_conf_with_an_error_is_said_when_the_server_starts_too() {
    let Some(t) = TestEnv::new("coldconf") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    let user_conf = t.config_dir().join("tmux.conf");
    let tmux = |args: &[&str]| {
        let out = Command::new("tmux")
            .args(["-L", &t.socket])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let option = || tmux(&["show", "-gv", "@roster_test"]);
    // A good one: nothing said, and run once, not again to check it.
    fs::write(&user_conf, "set -ag @roster_test x\n").unwrap();
    let out = t.run(&["new", "demo", "--name", "one"]);
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    assert_eq!(option(), "x");

    // A line tmux skips: said once as the server starts (it says nothing
    // itself), the rest of the file run once, and the session runs anyway.
    tmux(&["kill-server"]);
    fs::write(
        &user_conf,
        "set -ag @roster_test x\nset -g no-such-option 1\n",
    )
    .unwrap();
    let out = t.run(&["new", "demo", "--name", "two"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success()
            && err
                .matches("tmux.conf:2: invalid option: no-such-option")
                .count()
                == 1
            && err.contains("roster's tmux goes on without it"),
        "{err}"
    );
    assert_eq!(t.session("two", false).unwrap()["state"], "working");
    let out = t.run(&["ls"]);
    assert!(out.stderr.is_empty(), "{out:?}");
    assert_eq!(option(), "x");

    // One tmux can't read at all: said too.
    tmux(&["kill-server"]);
    fs::write(&user_conf, "no-such-command\n").unwrap();
    let out = t.run(&["start", "one"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success()
            && err
                .matches("tmux.conf:1: unknown command: no-such-command")
                .count()
                == 1,
        "{err}"
    );
    // Fixed, it's read at the next roster command.
    fs::write(&user_conf, "set -g @roster_test fixed\n").unwrap();
    let out = t.run(&["ls"]);
    assert!(out.stderr.is_empty(), "{out:?}");
    assert_eq!(option(), "fixed");
}

#[test]
fn first_prompts_are_typed_in_once_the_agent_is_ready() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let Some(t) = TestEnv::new("prompt") else {
        return;
    };
    t.ok(&[
        "project",
        "add",
        "demo",
        t.path("proj").to_str().unwrap(),
        "--model",
        "opus",
        "--effort",
        "high",
    ]);

    // Claude is ready once it reports its session; `new` waits for that.
    let waiting = t
        .command(&["new", "demo", "--name", "c", "--prompt", "fix the tests"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    t.wait_for("the session", |t| t.session("c", false).is_some());
    let s = t.session("c", false).unwrap();
    assert_eq!(
        (s["model"].as_str(), s["effort"].as_str()),
        (Some("opus"), Some("high"))
    );
    let id = s["id"].as_str().unwrap().to_string();
    // (Once the stand-in has printed its arguments: on a busy machine that
    // comes a moment after the session, whose row is saved before its window
    // is made, so `show` fails until then.)
    t.wait_for("the agent's arguments", |t| {
        let out = t.run(&["show", "c"]);
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("fake agent:")
    });
    let screen = t.ok(&["show", "c"]);
    assert!(screen.contains("--model opus --effort high"), "{screen}");
    assert!(
        !screen.contains("fix the tests"),
        "typed before the agent was ready: {screen}"
    );
    let payload = format!(
        r#"{{"session_id":"x","transcript_path":"{}"}}"#,
        t.path("x.jsonl").display()
    );
    t.report(&id, "session", &payload);
    let out = waiting.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Sent the first prompt"));
    t.wait_for("the prompt in the agent", |t| {
        t.ok(&["show", "c"]).contains("fix the tests")
    });

    // A generic agent is ready once it's idle after starting up.
    let agent = t.path("generic-agent");
    fs::write(&agent, "#!/bin/sh\necho ready\nexec cat\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "{text}[agents.echo]\ncommand = \"{}\"\nidle_after = 1\n",
            agent.display()
        ),
    )
    .unwrap();
    let out = t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "g",
        "--prompt",
        "hello generic",
    ]);
    assert!(out.contains("Sent the first prompt"), "{out}");
    t.wait_for("the prompt in the generic agent", |t| {
        t.ok(&["show", "g"]).contains("hello generic")
    });
    // A generic agent takes no model or effort, so its session doesn't keep them.
    assert_eq!(t.session("g", false).unwrap()["model"], Value::Null);

    // Any text is typed in as it is: a lone `;` is no tmux command separator.
    let out = t.ok(&[
        "new", "demo", "--agent", "echo", "--name", "semi", "--prompt", ";",
    ]);
    assert!(out.contains("Sent the first prompt"), "{out}");
    t.wait_for("the `;` in the agent", |t| {
        t.ok(&["show", "semi"]).lines().any(|l| l.trim() == ";")
    });
    // And at any length: tmux takes a command line of about 16 KB at most.
    let long = format!("{}end", "a long prompt ".repeat(1500));
    let out = t.ok(&[
        "new", "demo", "--agent", "echo", "--name", "long", "--prompt", &long,
    ]);
    assert!(out.contains("Sent the first prompt"), "{out}");
    assert_eq!(t.session("long", false).unwrap()["pending_prompt"], false);
}

#[test]
fn projects_and_sessions_can_be_edited() {
    let Some(t) = TestEnv::new("edit") else {
        return;
    };
    t.ok(&[
        "project",
        "add",
        "demo",
        t.path("proj").to_str().unwrap(),
        "--model",
        "sonnet",
    ]);
    fs::create_dir_all(t.path("moved")).unwrap();
    t.ok(&[
        "project",
        "edit",
        "demo",
        "--name",
        "renamed",
        "--path",
        t.path("moved").to_str().unwrap(),
        "--model",
        "default",
        "--effort",
        "max",
    ]);
    assert!(t.ok(&["project", "ls"]).contains("renamed"));
    assert!(
        !t.run(&["project", "edit", "renamed", "--effort", "extreme"])
            .status
            .success()
    );

    t.ok(&["new", "renamed", "--name", "s"]);
    let s = t.session("s", false).unwrap();
    assert_eq!(
        (s["model"].clone(), s["effort"].as_str()),
        (Value::Null, Some("max"))
    );
    let out = t.ok(&["edit", "s", "--name", "t", "--model", "haiku"]);
    assert!(out.contains("next start"), "{out}");
    let s = t.session("t", false).unwrap();
    assert_eq!(
        (s["model"].as_str(), s["effort"].as_str()),
        (Some("haiku"), Some("max"))
    );
    t.ok(&["restart", "--yes", "t"]);
    t.wait_for("the new model", |t| {
        t.ok(&["show", "t"]).contains("--model haiku --effort max")
    });
}

#[test]
fn doctor_finds_orphans_and_fixes_them() {
    let Some(t) = TestEnv::new("doctor") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "api"]);
    t.ok(&["new", "demo", "--name", "old"]);
    t.ok(&["stop", "--yes", "old"]);
    let report = t.ok(&["doctor"]);
    assert!(report.contains("✓ back key    C-q, bound"), "{report}");
    assert!(report.contains("✓ environment clean"), "{report}");
    assert!(
        report.contains("demo/api") && report.contains("(stopped; last screen kept)"),
        "{report}"
    );
    assert!(report.contains("problems (0)"), "{report}");

    // A tmux session no roster session owns.
    Command::new("tmux")
        .args([
            "-L",
            &t.socket,
            "new-session",
            "-d",
            "-s",
            "r-deadbeef",
            "sleep 600",
        ])
        .env_remove("TMUX")
        .output()
        .unwrap();
    let out = t.run(&["doctor"]);
    assert!(!out.status.success());
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(
        report.contains("r-deadbeef") && report.contains("← orphan"),
        "{report}"
    );
    assert!(report.contains("belongs to no roster session"), "{report}");
    assert!(
        !t.run(&["doctor", "--fix"]).status.success(),
        "fixed without --yes"
    );
    let fixed = t.ok(&["doctor", "--fix", "--yes"]);
    assert!(
        fixed.contains("Killed 1 orphaned tmux session(s)") && fixed.contains("problems (0)"),
        "{fixed}"
    );
}

#[test]
fn ls_shows_titles_models_and_filters() {
    let Some(t) = TestEnv::new("ls") else { return };
    t.ok(&[
        "project",
        "add",
        "a",
        t.path("proj").to_str().unwrap(),
        "--model",
        "opus",
        "--effort",
        "high",
    ]);
    fs::create_dir_all(t.path("other")).unwrap();
    t.ok(&["project", "add", "b", t.path("other").to_str().unwrap()]);
    fs::write(t.path("other/.agent-roster.env"), "X=1\n").unwrap();
    let projects = t.ok(&["project", "ls"]);
    let line = |name: &str| {
        let row = projects
            .lines()
            .find(|l| l.starts_with(&format!("{name} ")));
        row.unwrap_or_default().to_string()
    };
    assert!(line("a").ends_with("claude · opus · high"), "{projects}");
    assert!(line("b").ends_with("· env file"), "{projects}");
    t.ok(&["new", "a", "--name", "one"]);
    t.ok(&["new", "b", "--name", "two"]);
    let id = t.session("two", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    t.report(&id, "idle", "{}");

    let all = t.ok(&["ls"]);
    assert!(
        all.contains("TITLE") && all.contains("claude · opus · high"),
        "{all}"
    );
    let only_a = t.ok(&["ls", "--project", "a"]);
    assert!(
        only_a.contains("one") && !only_a.contains("two"),
        "{only_a}"
    );
    assert!(!t.run(&["ls", "--project", "nope"]).status.success());
    // By attention, the unread session comes first.
    let by_attention = t.ok(&["ls", "--sort", "attention"]);
    let lines: Vec<&str> = by_attention.lines().collect();
    assert!(
        lines[1].contains("two") && lines[1].contains("●"),
        "{by_attention}"
    );
    // Words filter fuzzily, by project, title or name.
    let matching = t.ok(&["ls", "b/tw"]);
    assert!(
        matching.contains("two") && !matching.contains("one"),
        "{matching}"
    );
    assert!(t.ok(&["ls", "zzz"]).contains("No sessions match \"zzz\""));
    // A title of your own is the title it shows.
    t.ok(&["edit", "one", "--title", "Chosen review title"]);
    let chosen = t.ok(&["ls", "Chosen"]);
    assert!(chosen.contains("Chosen review title"), "{chosen}");
}

#[test]
fn a_session_shows_the_model_and_effort_its_agent_runs_with() {
    let Some(t) = TestEnv::new("running") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&[
        "new", "demo", "--name", "one", "--model", "opus", "--effort", "high",
    ]);
    let id = t.session("one", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let agent = |t: &TestEnv| {
        let info = t.ok(&["info", "one"]);
        let line = info.lines().find(|l| l.contains("claude"));
        line.unwrap_or_default().to_string()
    };
    // Until its agent says, what its start asked for.
    assert!(agent(&t).contains("claude · opus · high"), "{}", agent(&t));
    t.report(
        &id,
        "session",
        r#"{"session_id":"c","model":"claude-opus-5-5","source":"startup"}"#,
    );
    t.report(
        &id,
        "idle",
        r#"{"session_id":"c","effort":{"level":"high"}}"#,
    );
    assert!(
        agent(&t).contains("claude · Opus 5.5 · high"),
        "{}",
        agent(&t)
    );

    // Edited, it runs on as it was until its next start, which asks for the
    // new ones: a model as it was learned to be, else as asked.
    t.ok(&["edit", "one", "--model", "haiku", "--effort", "low"]);
    assert!(
        agent(&t).contains("claude · Opus 5.5 · high"),
        "{}",
        agent(&t)
    );
    t.ok(&["restart", "--yes", "one"]);
    assert!(agent(&t).contains("claude · haiku · low"), "{}", agent(&t));
    t.ok(&["edit", "one", "--model", "opus", "--effort", "default"]);
    t.ok(&["restart", "--yes", "one"]);
    let line = agent(&t);
    assert!(
        line.contains("claude · Opus 5.5") && !line.contains("low"),
        "{line}"
    );
}

#[test]
fn titles_and_statistics() {
    let Some(t) = TestEnv::new("titles") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "one"]);
    let id = t.session("one", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A transcript like Claude's: two prompts, a reply written twice, cost so far.
    let transcript = t.path("conv.jsonl");
    let lines = [
        r#"{"type":"user","message":{"content":"fix the login tests"}}"#,
        r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":2,"cache_read_input_tokens":117000,"cache_creation_input_tokens":0,"output_tokens":900}}}"#,
        r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":2,"cache_read_input_tokens":117000,"cache_creation_input_tokens":0,"output_tokens":900}}}"#,
        r#"{"type":"cost-state","totalCostUSD":3.2,"totalLinesAdded":420,"totalLinesRemoved":35}"#,
        r#"{"type":"user","message":{"content":"and add a retry"}}"#,
    ];
    fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    let payload = format!(
        r#"{{"session_id":"c","transcript_path":"{}","model":"claude-opus-5-5[1m]"}}"#,
        transcript.display()
    );
    t.report(&id, "session", &payload);
    t.report(&id, "working", "{}");
    t.report(&id, "idle", "{}");

    let info = t.ok(&["info", "one"]);
    assert!(info.contains("claude · Opus 5.5 (1M)"), "{info}");
    assert!(info.contains("created now · 2 prompts"), "{info}");
    assert!(
        info.contains("context 117k/1M (12%) · 900 out · $3.20 · +420 −35"),
        "{info}"
    );

    // A title of your own wins; "" follows the agent's again.
    t.ok(&["edit", "one", "--title", "My own title"]);
    assert_eq!(
        t.session("one", false).unwrap()["title_override"],
        "My own title"
    );
    assert!(t.ok(&["info", "one"]).contains("My own title (yours)"));
    t.ok(&["edit", "one", "--title", ""]);
    assert_eq!(
        t.session("one", false).unwrap()["title_override"],
        Value::Null
    );

    // retitle asks "claude -p" (here the stand-in, which echoes its arguments).
    let title = t.ok(&["retitle", "one"]);
    assert!(
        title.starts_with("fake agent: -p --no-session-persistence"),
        "{title}"
    );
    assert!(
        t.session("one", false).unwrap()["title_override"]
            .as_str()
            .unwrap()
            .starts_with("fake agent: -p")
    );
}

#[test]
fn conversations_started_elsewhere_can_be_adopted() {
    let Some(t) = TestEnv::new("adopt") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    let sub = t.path("proj/sub");
    let other = t.path("other");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir_all(&other).unwrap();
    let kept = t.conversation("11111111-aaaa", &sub, "Fix the parser");
    t.conversation("22222222-bbbb", &other, "Elsewhere");
    t.conversation("33333333-cccc", &t.path("proj"), "Still open");
    let mut claude = t.open_in_claude("33333333-cccc");

    // By default only those in your projects' directories.
    let list = t.ok(&["adopt", "--list"]);
    assert!(
        list.contains("11111111") && list.contains("Fix the parser"),
        "{list}"
    );
    assert!(list.contains("open in Claude (pid"), "{list}");
    assert!(!list.contains("22222222"), "{list}");
    let all = t.ok(&["adopt", "--all"]);
    assert!(
        all.contains("22222222") && all.contains("new: other"),
        "{all}"
    );

    // Open in Claude: it asks, so without a terminal it's refused, unless
    // it's a copy.
    let out = t.run(&["adopt", "3333"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        err.contains("open in Claude (pid")
            && err.contains("--fork adopts a copy")
            && err.contains("Pass --yes to confirm."),
        "{err}"
    );
    let out = t.ok(&["adopt", "3333", "--fork", "--name", "copy"]);
    assert!(out.starts_with("Adopted demo/copy as a copy"), "{out}");
    // On what it last ran on.
    t.wait_for("the forked agent", |t| {
        t.ok(&["show", "copy"]).contains(
            "fake agent: --model claude-sonnet-5-5 --resume 33333333-cccc --fork-session --session-id",
        )
    });
    let copy = t.session("copy", false).unwrap();
    assert_ne!(copy["conversation_id"], "33333333-cccc");
    // The refusal left nothing behind, and --yes moves it anyway.
    assert!(t.ok(&["adopt", "--list"]).contains("33333333"));
    let out = t.ok(&["adopt", "3333", "--yes", "--name", "moved"]);
    assert!(out.starts_with("Adopted demo/moved ("), "{out}");
    assert_eq!(
        t.session("moved", false).unwrap()["conversation_id"],
        "33333333-cccc"
    );
    claude.kill().unwrap();
    claude.wait().unwrap();

    // Moved: resumed where it ran, in the project holding it, with its title and model.
    // With the defaults instead: the project has none, nor config.toml.
    t.ok(&["adopt", "11111111-aaaa", "--name", "one", "--defaults"]);
    t.wait_for("the resumed agent", |t| {
        t.ok(&["show", "one"])
            .contains("fake agent: --resume 11111111-aaaa")
    });
    let one = t.session("one", false).unwrap();
    assert_eq!(
        (one["project"].as_str(), one["title"].as_str()),
        (Some("demo"), Some("Fix the parser"))
    );
    assert_eq!(one["workdir"], sub.to_str().unwrap());
    assert_eq!(one["transcript_path"], kept.to_str().unwrap());
    assert_eq!(one["running_model"], "claude-sonnet-5-5");
    // It's roster's now, and the original is no longer offered.
    assert!(!t.ok(&["adopt", "--list", "--all"]).contains("11111111"));

    // Outside any project: a project is created for its directory.
    t.ok(&["adopt", "2222"]);
    let projects = t.ok(&["project", "ls"]);
    assert!(projects.contains("other"), "{projects}");
    let s = t.session("claude-1", false).unwrap();
    assert_eq!(
        (s["project"].as_str(), s["workdir"].as_str()),
        (Some("other"), other.to_str())
    );

    // An archived session's conversation is restored, not adopted again.
    t.ok(&["archive", "--yes", "one"]);
    assert!(
        t.ok(&["adopt", "--list"])
            .contains("archived in roster as demo/one")
    );
    let out = t.run(&["adopt", "1111"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("restore it instead"));
    // Deleted for good, it can be adopted again.
    t.ok(&["rm", "--yes", "one"]);
    let list = t.ok(&["adopt", "--list"]);
    assert!(
        list.contains("11111111") && !list.contains("archived"),
        "{list}"
    );
}

#[test]
fn chats_need_no_project() {
    let Some(t) = TestEnv::new("chat") else {
        return;
    };
    // No project yet: a chat creates the chats project, in roster's state directory.
    let out = t.ok(&["chat", "--name", "hello"]);
    assert!(out.starts_with("Started chats/hello"), "{out}");
    let dir = t.path("state/agent-roster/chats");
    let s = t.session("hello", false).unwrap();
    assert_eq!(
        (s["project"].as_str(), s["workdir"].as_str()),
        (Some("chats"), dir.to_str())
    );
    t.wait_for("the agent", |t| {
        t.ok(&["show", "hello"])
            .contains("fake agent: --session-id")
    });
    // One project for every chat.
    t.ok(&["chat", "--name", "again"]);
    let projects = t.ok(&["project", "ls"]);
    assert_eq!(projects.lines().count(), 2, "{projects}");
    assert!(projects.contains("built-in, for chats"), "{projects}");

    // Its directory is roster's; its name and defaults are yours.
    let proj = t.path("proj");
    let out = t.run(&["project", "edit", "chats", "--path", proj.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("chats_dir"));
    t.ok(&[
        "project", "edit", "chats", "--name", "talk", "--model", "haiku",
    ]);
    t.ok(&["chat", "--name", "second"]);
    let s = t.session("second", false).unwrap();
    assert_eq!(
        (s["project"].as_str(), s["model"].as_str()),
        (Some("talk"), Some("haiku"))
    );

    // Removed once empty, it comes back with the next chat, where chats_dir says.
    for name in ["hello", "again", "second"] {
        t.ok(&["archive", "--yes", name]);
        t.ok(&["rm", "--yes", name]);
    }
    t.ok(&["project", "rm", "--yes", "talk"]);
    let mine = t.path("my chats");
    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("chats_dir = \"{}\"\n{text}", mine.display()),
    )
    .unwrap();
    t.ok(&["chat", "--name", "third"]);
    let s = t.session("third", false).unwrap();
    assert_eq!(
        (s["project"].as_str(), s["workdir"].as_str()),
        (Some("chats"), mine.to_str())
    );
}

#[test]
fn notes_are_kept_with_their_session() {
    let Some(t) = TestEnv::new("notes") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "one"]);
    let out = t.run(&["note", "one"]);
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("demo/one has no note"));

    assert!(
        t.ok(&["note", "one", "--set", "TODO"])
            .contains("Saved the note of demo/one")
    );
    t.ok(&["note", "one", "--append", "- [ ] first"]);
    t.ok(&["note", "one", "--append", "- [ ] second\n\n"]);
    assert_eq!(t.ok(&["note", "one"]), "TODO\n- [ ] first\n- [ ] second\n");
    let s = t.session("one", false).unwrap();
    assert_eq!(s["note"], "TODO\n- [ ] first\n- [ ] second");
    let info = t.ok(&["info", "one"]);
    assert!(
        info.contains("note         TODO\n             - [ ] first\n"),
        "{info}"
    );
    assert!(t.ok(&["note", "one", "--append", ""]).contains("unchanged"));
    // Pinned from the CLI as with mm in the TUI; ls marks it.
    let out = t.ok(&["note", "one", "--pin", "2"]);
    assert!(
        out.contains("Pinned line 2 of the note of demo/one: - [ ] first"),
        "{out}"
    );
    assert_eq!(t.session("one", false).unwrap()["note_pin"], 1);
    assert!(
        t.ok(&["info", "one"])
            .contains("             ▸ - [ ] first\n")
    );
    assert!(t.ok(&["ls"]).contains("✎ "));
    assert!(!t.run(&["note", "one", "--pin", "4"]).status.success());
    assert!(
        !t.run(&["note", "one", "--pin", "1", "--unpin"])
            .status
            .success()
    );
    assert!(
        t.ok(&["note", "one", "--unpin"])
            .contains("Unpinned the note of demo/one")
    );
    assert_eq!(t.session("one", false).unwrap()["note_pin"], Value::Null);
    t.ok(&["note", "one", "--pin", "3"]);
    assert!(
        !t.run(&["note", "one", "--set", "x", "--clear"])
            .status
            .success()
    );

    // In $EDITOR; one that fails leaves the note as it was.
    let editor = t.path("editor.sh");
    fs::write(&editor, "#!/bin/sh\nsed -i 's/first/first, done/' \"$1\"\n").unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&editor)
        .status()
        .unwrap();
    let edit = |editor: &str| {
        t.command(&["note", "one", "--edit"])
            .env("EDITOR", editor)
            .env_remove("VISUAL")
            .output()
            .unwrap()
    };
    assert!(edit(editor.to_str().unwrap()).status.success());
    assert_eq!(
        t.ok(&["note", "one"]),
        "TODO\n- [ ] first, done\n- [ ] second\n"
    );
    let out = edit("false");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("the note is as it was"));
    assert_eq!(
        t.ok(&["note", "one"]),
        "TODO\n- [ ] first, done\n- [ ] second\n"
    );
    // Its file isn't left behind, unless what was written can't be kept.
    assert_eq!(
        fs::read_dir(t.path("state/agent-roster/notes"))
            .unwrap()
            .count(),
        0
    );
    fs::write(&editor, "#!/bin/sh\nprintf 'x\\377' > \"$1\"\n").unwrap();
    let out = edit(editor.to_str().unwrap());
    assert!(!out.status.success());
    // In a file of its own: the session's name and id, and the editing's.
    let kept_in = |out: &std::process::Output| {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let at = stderr
            .find("kept in ")
            .unwrap_or_else(|| panic!("{stderr}"))
            + 8;
        let path: String = stderr[at..].chars().take_while(|c| *c != ':').collect();
        std::path::PathBuf::from(path)
    };
    let kept = kept_in(&out);
    let id = t.session("one", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let name = kept.file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with(&format!("demo-one-{id}.")), "{name}");
    assert_eq!(fs::read(&kept).unwrap(), b"x\xff");
    fs::remove_file(&kept).unwrap();

    // Saved by another meanwhile (an agent, say): yours isn't saved over it,
    // and is kept.
    let roster = env!("CARGO_BIN_EXE_roster");
    fs::write(
        &editor,
        format!("#!/bin/sh\n'{roster}' note one --append 'meanwhile'\necho mine >> \"$1\"\n"),
    )
    .unwrap();
    let out = edit(editor.to_str().unwrap());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success() && stderr.contains("changed meanwhile"),
        "{stderr}"
    );
    assert!(t.ok(&["note", "one"]).ends_with("meanwhile\n"));
    let kept = kept_in(&out);
    assert!(fs::read_to_string(&kept).unwrap().ends_with("mine\n"));
    fs::remove_file(&kept).unwrap();
    // A blank text isn't taken for a note to remove: --clear is.
    let out = t.run(&["note", "one", "--set", " "]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("--clear removes it"));

    // From stdin; through archiving; cleared.
    let mut child = t
        .command(&["note", "one", "--set", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"from stdin\n").unwrap();
    assert!(child.wait().unwrap().success());
    t.ok(&["archive", "--yes", "one"]);
    t.ok(&["restore", "one"]);
    assert_eq!(t.ok(&["note", "one"]), "from stdin\n");
    assert!(
        t.ok(&["note", "one", "--clear"])
            .contains("demo/one has no note now")
    );
    assert_eq!(t.session("one", false).unwrap()["note"], Value::Null);
}

#[test]
fn an_agent_killed_by_a_signal_exited_with_128_and_it() {
    let Some(t) = TestEnv::new("signal") else {
        return;
    };
    fs::write(
        t.config_dir().join("config.toml"),
        "[agents.doomed]\ncommand = \"sh\"\nargs = [\"-c\", \"echo dying; kill -9 $$\"]\n",
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "gone", "--agent", "doomed"]);
    t.wait_for("stopped", |t| {
        t.session("gone", false).unwrap()["state"] == "stopped"
    });
    assert_eq!(t.session("gone", false).unwrap()["exit_status"], 137);
    assert!(t.ok(&["show", "gone"]).contains("dying"));
}

#[test]
fn a_title_made_while_its_session_changed_isnt_kept() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("retitlechanged") else {
        return;
    };
    // Claude's stand-in, making its title once the test lets it.
    let slow = t.path("slow-claude");
    let dir = t.path("titles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        &slow,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = -p ]; then\n\
               touch '{dir}/started'\n\
               while [ ! -e '{dir}/release' ]; do sleep 0.05; done\n\
               echo 'Slow title'; exit 0\n\
             fi\n\
             exec '{agent}' \"$@\"\n",
            dir = dir.display(),
            agent = t.path("fake-agent").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        t.config_dir().join("config.toml"),
        format!(
            "shell = \"/bin/sh\"\n[agents.claude]\ncommand = \"{}\"\n",
            slow.display()
        ),
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "one"]);
    let id = t.session("one", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let transcript = t.path("one.jsonl");
    fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"content\":\"a task\"}}\n",
    )
    .unwrap();
    let payload = format!(
        r#"{{"session_id":"c","transcript_path":"{}"}}"#,
        transcript.display()
    );
    t.report(&id, "session", &payload);

    // A title of yours given while it's made: yours stays, `retitle` says so.
    let retitle = t
        .command(&["retitle", "one"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    t.wait_for("the title's run", |_| dir.join("started").exists());
    t.ok(&["edit", "one", "--title", "Mine"]);
    fs::write(dir.join("release"), "").unwrap();
    let out = retitle.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("its title changed meanwhile"),
        "{stderr}"
    );
    assert_eq!(t.session("one", false).unwrap()["title_override"], "Mine");

    // On another conversation by the time it's made (a /clear): not kept.
    fs::remove_file(dir.join("release")).unwrap();
    fs::remove_file(dir.join("started")).unwrap();
    t.ok(&["edit", "one", "--title", ""]);
    let retitle = t
        .command(&["retitle", "one"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    t.wait_for("the title's run again", |_| dir.join("started").exists());
    let moved = format!(
        r#"{{"session_id":"c-later","transcript_path":"{}"}}"#,
        transcript.display()
    );
    t.report(&id, "session", &moved);
    fs::write(dir.join("release"), "").unwrap();
    let out = retitle.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("on another conversation now"),
        "{stderr}"
    );
    assert_eq!(
        t.session("one", false).unwrap()["title_override"],
        Value::Null
    );
}
