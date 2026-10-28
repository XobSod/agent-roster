//! Sessions from start to deletion: launching, addressing, first prompts,
//! titles, statistics, notes, chats, and checking them with doctor.

use std::fs;
use std::process::Command;

use serde_json::{Value, json};

use crate::common::TestEnv;

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

    // A turn that ends with a subagent left at work: working till it stops.
    let subagent = r#"{"session_id":"conv-1","agent_id":"b1","agent_type":"Explore"}"#;
    t.report(&id, "prompt", &payload);
    t.report(&id, "subagent-start", subagent);
    t.report(&id, "idle", &payload);
    let state = |t: &TestEnv| {
        let s = t.session("one", false).unwrap();
        (
            s["state"].clone(),
            s["unread"].clone(),
            s["subagents"].clone(),
        )
    };
    assert_eq!(state(&t), (json!("working"), json!(false), json!(1)));
    // Its screen, still meanwhile, doesn't end it.
    std::thread::sleep(std::time::Duration::from_millis(4500));
    assert_eq!(state(&t), (json!("working"), json!(false), json!(1)));
    t.report(&id, "subagent-stop", subagent);
    assert_eq!(state(&t), (json!("idle"), json!(true), json!(0)));
    t.ok(&["read", "one"]);
    // One never heard of again is taken for gone, by any roster.
    t.report(&id, "prompt", &payload);
    t.report(&id, "subagent-start", subagent);
    t.report(&id, "idle", &payload);
    assert_eq!(state(&t), (json!("working"), json!(false), json!(1)));
    let db = rusqlite::Connection::open(t.db()).unwrap();
    db.execute(
        "UPDATE sessions SET subagents = '{\"b1\":1000}' WHERE id = ?1",
        [&id],
    )
    .unwrap();
    assert_eq!(state(&t), (json!("idle"), json!(true), json!(0)));
    t.ok(&["read", "one"]);

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

    // Deleting for good asks first, archived or not.
    t.ok(&["archive", "--yes", "one"]);
    assert!(!t.run(&["rm", "one"]).status.success());
    let out = t.ok(&["rm", "--yes", "one"]);
    assert!(out.contains("for good"), "{out}");
    assert!(t.session("one", false).is_none() && t.session("one", true).is_none());
    // A live one too: its agent stops and its tmux session goes with it.
    t.ok(&["new", "demo", "--name", "live"]);
    let id = t.session("live", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    t.wait_for("idle", |t| {
        t.session("live", false).unwrap()["state"] == "idle"
    });
    // What its hooks said since it was last looked at is in the question.
    t.report(&id, "prompt", &payload);
    t.report(&id, "subagent-start", subagent);
    let out = t.run(&["rm", "live"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("Its agent stops (it's working, with a subagent)"),
        "{err}"
    );
    t.ok(&["rm", "--yes", "live"]);
    assert!(t.session("live", false).is_none() && t.session("live", true).is_none());
    let tmux = Command::new("tmux")
        .args(["-L", &t.socket, "has-session", "-t", &format!("=r-{id}")])
        .output()
        .unwrap();
    assert!(!tmux.status.success());
    // A name a live session and an archived one share names neither: what
    // goes for good is given by its id.
    t.ok(&["new", "demo", "--name", "dup"]);
    let gone = t.session("dup", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    t.ok(&["archive", "--yes", "dup"]);
    t.ok(&["new", "demo", "--name", "dup"]);
    let out = t.run(&["rm", "--yes", "dup"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("a live session") && err.contains(&gone),
        "{err}"
    );
    assert!(t.session("dup", false).is_some() && t.session("dup", true).is_some());
    t.ok(&["rm", "--yes", &gone]);
    t.ok(&["rm", "--yes", "dup"]);
    assert!(t.session("dup", false).is_none() && t.session("dup", true).is_none());
    let out = t.run(&["rm", "--yes", "dup"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no session \"dup\""), "{err}");
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
    // A title alone needs no restart; a model does.
    let out = t.ok(&["edit", "s", "--title", "Mine"]);
    assert!(!out.contains("next start"), "{out}");
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
    // What the live one's agent and all it started use, its processes
    // under it; CPU only between readings.
    let api = report
        .split("demo/api")
        .nth(1)
        .unwrap_or_else(|| panic!("{report}"));
    let mut lines = api.lines();
    let line = lines.next().unwrap();
    assert!(
        line.contains(" MB  CPU     -  1 process      alive, pid "),
        "{line}"
    );
    let agent = lines.next().unwrap();
    assert!(
        agent.starts_with("    ") && agent.contains(" MB  CPU     -  pid "),
        "{agent}"
    );
    let list: serde_json::Value =
        serde_json::from_str(&t.ok(&["ls", "--resources", "--json"])).unwrap();
    let api = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "api")
        .unwrap();
    let resources = &api["resources"];
    assert!(resources["memory"].as_u64().unwrap() > 0, "{resources}");
    assert!(resources["processes"].as_u64().unwrap() >= 1, "{resources}");
    assert!(resources["cpu"].as_f64().is_some(), "{resources}");
    let table = t.ok(&["ls", "--resources"]);
    assert!(
        table.starts_with("PROJECT  SESSION  ") && table.contains("CPU  MEMORY"),
        "{table}"
    );

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

    // Saved by another meanwhile (an agent, say): merged with yours, each
    // having changed other lines.
    let roster = env!("CARGO_BIN_EXE_roster");
    fs::write(
        &editor,
        format!("#!/bin/sh\n'{roster}' note one --append 'theirs'\nsed -i '1s/^/mine: /' \"$1\"\n"),
    )
    .unwrap();
    let out = edit(editor.to_str().unwrap());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success() && stdout.contains("merged with the external change"),
        "{stdout}"
    );
    let note = t.ok(&["note", "one"]);
    assert!(
        note.starts_with("mine: ") && note.ends_with("theirs\n"),
        "{note}"
    );
    // Both added a line at its end: yours isn't saved over theirs, and is
    // kept.
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
fn a_projects_note_is_its_own() {
    let Some(t) = TestEnv::new("pnotes") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "one"]);
    let out = t.run(&["note", "--project", "demo"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("project demo has no note"));
    assert!(
        t.ok(&["note", "--project", "demo", "--set", "backlog"])
            .contains("Saved the note of project demo")
    );
    t.ok(&["note", "--project", "demo", "--append", "- [ ] retry"]);
    assert!(
        t.ok(&["note", "--project", "demo", "--pin", "2"])
            .contains("Pinned line 2 of the note of project demo: - [ ] retry")
    );
    assert_eq!(
        t.ok(&["note", "--project", "demo"]),
        "backlog\n- [ ] retry\n"
    );
    // Apart from its sessions' notes.
    assert!(t.run(&["note", "one"]).stdout.is_empty());
    // A session's project's, by the session; inside an agent, its own's.
    assert_eq!(
        t.ok(&["note", "one", "--project"]),
        "backlog\n- [ ] retry\n"
    );
    let id = t.session("one", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let inside = |args: &[&str]| {
        let out = t.command(args).env("ROSTER_SESSION", &id).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    assert_eq!(
        inside(&["note", "--project"]),
        (true, "backlog\n- [ ] retry\n".into())
    );
    // And without a session, its own session's, read and written.
    t.ok(&["note", "one", "--set", "mine"]);
    assert_eq!(inside(&["note"]), (true, "mine\n".into()));
    assert!(inside(&["note", "--append", "- [x] done"]).0);
    assert_eq!(t.ok(&["note", "one"]), "mine\n- [x] done\n");
    t.ok(&["note", "one", "--clear"]);
    // An id left over from a session gone says where it came from.
    let out = t
        .command(&["note"])
        .env("ROSTER_SESSION", "0123abcd")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("ROSTER_SESSION is 0123abcd"),
        "{err}"
    );
    for which in [
        &["note", "--project"][..],
        &["note"],
        &["note", "one", "--project", "demo"],
        &["note", "--project", "nope"],
    ] {
        assert!(!t.run(which).status.success(), "{which:?}");
    }
    // Removing the project says its note goes too.
    t.ok(&["archive", "--yes", "one"]);
    let out = t.run(&["project", "rm", "demo"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Remove project demo and its note?"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Removed while its note is in $EDITOR, and another added: what's
    // written isn't saved, least of all as the other's note.
    for dir in ["old", "new"] {
        fs::create_dir_all(t.path(dir)).unwrap();
    }
    t.ok(&["project", "add", "old", t.path("old").to_str().unwrap()]);
    let roster = env!("CARGO_BIN_EXE_roster");
    let editor = t.path("editor.sh");
    fs::write(
        &editor,
        format!(
            "#!/bin/sh\n'{roster}' project rm old --yes\n'{roster}' project add new '{}'\n\
             echo 'for old' > \"$1\"\n",
            t.path("new").display()
        ),
    )
    .unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&editor)
        .status()
        .unwrap();
    let out = t
        .command(&["note", "--project", "old", "--edit"])
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("no project"), "{stderr}");
    let out = t.run(&["note", "--project", "new"]);
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn an_agent_that_died_unseen_stopped_when_it_died() {
    let Some(t) = TestEnv::new("unseen") else {
        return;
    };
    fs::write(
        t.config_dir().join("config.toml"),
        "[agents.brief]\ncommand = \"sh\"\nargs = [\"-c\", \"exit 2\"]\n",
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "brief", "--agent", "brief"]);
    // Nothing of roster's runs meanwhile (you attached elsewhere, say).
    std::thread::sleep(std::time::Duration::from_millis(2_500));
    let seen = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let s = t.session("brief", false).unwrap();
    assert_eq!(
        (s["state"].as_str(), s["exit_status"].as_i64()),
        (Some("stopped"), Some(2))
    );
    let since = s["state_since"].as_i64().unwrap();
    assert!(
        seen - since >= 2_000,
        "stopped {}ms before it was seen",
        seen - since
    );
}

#[test]
fn killing_a_session_ends_its_agent_and_all_it_started() {
    let Some(t) = TestEnv::new("kill") else {
        return;
    };
    // An agent leaving a process behind it, out of its tree.
    fs::write(
        t.config_dir().join("config.toml"),
        "[agents.leaver]\ncommand = \"sh\"\nargs = [\"-c\", \"(setsid sleep 600 >/dev/null 2>&1 &); exec sleep 600\"]\n",
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "leaver", "--agent", "leaver"]);
    let processes = |t: &TestEnv| {
        let list: serde_json::Value =
            serde_json::from_str(&t.ok(&["ls", "--resources", "--json"])).unwrap();
        list[0]["resources"]["processes"].as_u64()
    };
    t.wait_for("both running", |t| processes(t) == Some(2));
    let out = t.ok(&["stop", "--kill", "--yes", "leaver"]);
    assert_eq!(out, "Killed demo/leaver: 2 processes\n");
    let s = t.session("leaver", false).unwrap();
    // Stopped, its exit asked for, no error.
    assert_eq!(
        (s["state"].as_str(), &s["exit_status"]),
        (Some("stopped"), &Value::Null)
    );
    assert_eq!(processes(&t), None);
    assert_eq!(
        t.ok(&["stop", "--kill", "--yes", "leaver"]),
        "demo/leaver: nothing running\n"
    );

    // Archived, what it left running; deleted for good, by its id.
    let left = |t: &TestEnv, name: &str| {
        let id = t.session(name, false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        t.wait_for("its leftover", |t| {
            let list: serde_json::Value =
                serde_json::from_str(&t.ok(&["ls", "--resources", "--json"])).unwrap();
            let s = list
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == id.as_str())
                .unwrap();
            s["resources"]["processes"].as_u64() == Some(2)
        });
        id
    };
    t.ok(&["new", "demo", "--name", "stray", "--agent", "leaver"]);
    left(&t, "stray");
    t.ok(&["archive", "--yes", "stray"]);
    let out = t.ok(&["stop", "--kill", "--yes", "stray"]);
    assert_eq!(out, "Killed demo/stray: 1 process\n");
    t.ok(&["new", "demo", "--name", "gone", "--agent", "leaver"]);
    let id = left(&t, "gone");
    t.ok(&["rm", "--yes", "gone"]);
    let out = t.ok(&["stop", "--kill", "--yes", &id]);
    assert_eq!(out, format!("Killed {id} (deleted): 1 process\n"));
    assert!(!t.run(&["stop", "--kill", "--yes", &id]).status.success());
}

#[test]
fn a_session_can_kill_itself_from_inside() {
    let Some(t) = TestEnv::new("killself") else {
        return;
    };
    // Its agent leaving a process behind, then running roster to kill its
    // own session: roster outlives the rest, and says so.
    let out = t.path("out");
    let script = format!(
        "(setsid sleep 600 >/dev/null 2>&1 &); sleep 1; \"$ROSTER_EXE\" stop --kill --yes \"$ROSTER_SESSION\" > '{}' 2>&1; sleep 600",
        out.display()
    );
    let config = format!(
        "[agents.self]\ncommand = \"sh\"\nargs = [\"-c\", {}]\n",
        toml::Value::String(script)
    );
    fs::write(t.config_dir().join("config.toml"), config).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "self", "--agent", "self"]);
    t.wait_for("its report", |_| {
        fs::read_to_string(&out).is_ok_and(|o| o.ends_with('\n'))
    });
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "Killed demo/self: 2 processes\n"
    );
    let s = t.session("self", false).unwrap();
    assert_eq!(
        (s["state"].as_str(), &s["exit_status"]),
        (Some("stopped"), &Value::Null)
    );
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
