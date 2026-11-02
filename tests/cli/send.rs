//! Prompts sent to a session, by another session or by you: queued, and
//! typed into its agent one at a time once it's ready.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Output, Stdio};

use crate::common::{Terminal, TestEnv};

/// Two stand-ins without hooks: `calm` waits for lines, and `busy` prints
/// all the while, working, till its project's directory has `done` in it
/// (then waits for lines too).
fn add_agents(t: &TestEnv, settings: &str) {
    let script = |name: &str, text: &str| {
        let path = t.path(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let calm = script("calm-agent", "#!/bin/sh\necho calm agent ready\nexec cat\n");
    let busy = script(
        "busy-agent",
        "#!/bin/sh\nwhile [ ! -e done ]; do date +%s%N; sleep 0.2; done\nexec cat\n",
    );
    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "{settings}\n{text}[agents.calm]\ncommand = \"{}\"\nidle_after = 1\n\
             [agents.busy]\ncommand = \"{}\"\nidle_after = 1\n",
            calm.display(),
            busy.display(),
        ),
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
}

/// `roster` run as if by session `id`'s agent.
fn run_in(t: &TestEnv, id: &str, args: &[&str]) -> Output {
    t.command(args).env("ROSTER_SESSION", id).output().unwrap()
}

/// The environment of each `roster deliver-queued` of `t`'s.
fn deliverers(t: &TestEnv) -> Vec<Vec<u8>> {
    let state = format!("XDG_STATE_HOME={}", t.path("state").display());
    (fs::read_dir("/proc").into_iter().flatten().flatten())
        .filter_map(|e| {
            let command = fs::read(e.path().join("cmdline")).ok()?;
            if !command.ends_with(b"\0deliver-queued\0") {
                return None;
            }
            let env = fs::read(e.path().join("environ")).ok()?;
            (env.split(|b| *b == 0))
                .any(|v| v == state.as_bytes())
                .then_some(env)
        })
        .collect()
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn prompts_sent_wait_in_a_queue_and_go_in_one_at_a_time_in_order() {
    let Some(t) = TestEnv::new("send") else {
        return;
    };
    add_agents(&t, "queue_limit = 3");
    t.ok(&["new", "demo", "--agent", "calm", "--name", "lead"]);
    t.ok(&["new", "demo", "--agent", "busy", "--name", "w"]);
    let lead = t.session("lead", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Inside an agent, `info` of no session is its own: where to send to.
    let out = run_in(&t, &lead, &["info"]);
    assert!(
        out.status.success() && said(&out).contains(&format!("demo/lead ({lead})")),
        "{}",
        said(&out)
    );
    let out = t.run(&["info"]);
    assert!(
        !out.status.success() && said(&out).contains("which session?"),
        "{}",
        said(&out)
    );

    // While it works, each waits behind the one before, whoever sent it.
    let out = run_in(&t, &lead, &["send", "w", "first"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(
        said(&out).contains("Queued for demo/w (next): it's working"),
        "{}",
        said(&out)
    );
    // Typed in by a roster of its own meanwhile: not one the sender's agent
    // started, for all it was started from there.
    let found = deliverers(&t);
    assert!(!found.is_empty());
    for env in found {
        assert!(!(env.split(|b| *b == 0)).any(|v| v.starts_with(b"ROSTER_SESSION=")));
    }
    let out = t.ok(&["send", "w", "second"]);
    assert!(out.contains("Queued for demo/w (1 ahead of it)"), "{out}");
    let out = t.ok(&["send", "w", "third"]);
    assert!(out.contains("Queued for demo/w (2 ahead of it)"), "{out}");
    // As many as `queue_limit` at most.
    let out = t.run(&["send", "w", "fourth"]);
    assert!(
        !out.status.success()
            && said(&out).contains("nothing sent to demo/w: 3 prompts are queued for it already"),
        "{}",
        said(&out)
    );
    let list = t.ok(&["send", "w", "--list"]);
    let rows: Vec<Vec<&str>> = (list.lines().skip(1))
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(rows.len(), 3, "{list}");
    assert_eq!((rows[0][0], rows[0][1]), ("1", "demo/lead"), "{list}");
    assert_eq!((rows[1][0], rows[1][1]), ("2", "you"), "{list}");
    assert_eq!(rows[2].last(), Some(&"third"), "{list}");
    // Found by the word the TUI's filter has for them, or left out.
    let names = |words: &str| -> Vec<String> {
        let list: serde_json::Value =
            serde_json::from_str(&t.ok(&["ls", "--json", words])).unwrap();
        (list.as_array().unwrap().iter())
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("queued"), ["w"]);
    assert_eq!(names("!queued"), ["lead"]);

    // Edited, or dropped, before it goes in.
    let editor = t.path("editor.sh");
    fs::write(
        &editor,
        "#!/bin/sh\nsed -i 's/second/second, edited/' \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
    let out = (t.command(&["send", "w", "--edit", "2"]))
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", said(&out));
    // Asked first, as in the TUI: refused with no terminal to ask at.
    let out = t.run(&["send", "w", "--drop", "3"]);
    assert!(
        !out.status.success() && said(&out).contains("Pass --yes to confirm."),
        "{}",
        said(&out)
    );
    let out = t.ok(&["send", "w", "--drop", "3", "--yes"]);
    assert!(out.contains("Dropped prompt 3 of demo/w's queue"), "{out}");
    let out = t.run(&["send", "w", "--drop", "9", "--yes"]);
    assert!(
        !out.status.success()
            && said(&out).contains("2 prompts are queued for demo/w, so no prompt 9"),
        "{}",
        said(&out)
    );

    // Kept while it's stopped, and it's sent to only if it's to start, and
    // starting it asks nothing: this one would begin anew.
    t.ok(&["stop", "--yes", "w"]);
    let list = t.ok(&["send", "w", "--list"]);
    assert_eq!(list.lines().count(), 3, "{list}");
    let out = t.run(&["send", "w", "third again"]);
    assert!(
        !out.status.success() && said(&out).contains("demo/w is stopped: --start starts it"),
        "{}",
        said(&out)
    );
    let out = t.run(&["send", "w", "--start", "third again"]);
    assert!(
        !out.status.success()
            && said(&out).contains("`roster start` it first, as starting it asks this: demo/w (busy) can't resume its conversation"),
        "{}",
        said(&out)
    );
    t.ok(&["start", "--yes", "w"]);
    let out = t.ok(&["send", "w", "third again"]);
    assert!(out.contains("Queued for demo/w (2 ahead of it)"), "{out}");

    // Its turns over, they go in, in the order sent, each once the one
    // before is done with.
    fs::write(t.path("proj/done"), "").unwrap();
    for prompt in ["first", "second, edited", "third again"] {
        t.wait_for(prompt, |t| {
            (t.ok(&["show", "w"]).lines()).any(|l| l.trim() == prompt)
        });
    }
    let screen = t.ok(&["show", "w"]);
    let at = |prompt: &str| screen.find(&format!("{prompt}\n")).unwrap();
    assert!(
        at("first") < at("second, edited") && at("second, edited") < at("third again"),
        "{screen}"
    );
    t.wait_for("the queue emptied", |t| {
        said(&t.run(&["send", "w", "--list"])).contains("Nothing queued for demo/w")
    });
    // Nothing left to type, it's gone.
    t.wait_for("no roster typing in the background", |t| {
        deliverers(t).is_empty()
    });

    // Typed in at once to an agent that's ready, from stdin too.
    t.wait_for("lead ready", |t| {
        t.session("lead", false).unwrap()["state"] == "idle"
    });
    let mut child = (t.command(&["send", "lead", "-"]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    (child.stdin.take().unwrap())
        .write_all(b"from stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let typed = said(&out);
    assert!(
        out.status.success()
            && (typed.contains("Typed into demo/lead")
                || typed.contains("Queued for demo/lead (next)")),
        "{typed}"
    );
    t.wait_for("the prompt from stdin", |t| {
        (t.ok(&["show", "lead"]).lines()).any(|l| l.trim() == "from stdin")
    });
}

#[test]
fn a_prompt_is_refused_to_itself_to_an_archived_session_and_when_empty() {
    let Some(t) = TestEnv::new("sendno") else {
        return;
    };
    add_agents(&t, "");
    t.ok(&["new", "demo", "--agent", "busy", "--name", "w"]);
    let w = t.session("w", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let out = run_in(&t, &w, &["send", "w", "hello"]);
    assert!(
        !out.status.success() && said(&out).contains("a session can't send to itself"),
        "{}",
        said(&out)
    );
    let out = t.run(&["send", "w", " "]);
    assert!(
        !out.status.success() && said(&out).contains("nothing to send"),
        "{}",
        said(&out)
    );

    // Archiving drops its queue, and asks knowing it.
    t.ok(&["send", "w", "one"]);
    t.ok(&["send", "w", "two"]);
    let out = t.run(&["archive", "w"]);
    assert!(
        !out.status.success()
            && said(&out)
                .contains("Archive demo/w? Its agent stops. The 2 prompts queued for it go."),
        "{}",
        said(&out)
    );
    t.ok(&["archive", "--yes", "w"]);
    let out = t.run(&["send", "w", "three"]);
    assert!(
        !out.status.success() && said(&out).contains("demo/w is archived; restore it first"),
        "{}",
        said(&out)
    );
    t.ok(&["restore", "w"]);
    assert!(said(&t.run(&["send", "w", "--list"])).contains("Nothing queued for demo/w"));

    // All of it dropped, or one, by its number.
    t.ok(&["start", "--yes", "w"]);
    t.ok(&["send", "w", "four"]);
    t.ok(&["send", "w", "five"]);
    let out = t.run(&["send", "w", "--drop", "0", "--yes"]);
    assert!(
        !out.status.success() && said(&out).contains("so no prompt 0"),
        "{}",
        said(&out)
    );
    let out = t.run(&["send", "w", "--drop"]);
    assert!(
        !out.status.success() && said(&out).contains("Pass --yes to confirm."),
        "{}",
        said(&out)
    );
    let out = t.ok(&["send", "w", "--drop", "--yes"]);
    assert!(
        out.contains("Dropped the 2 prompts queued for demo/w"),
        "{out}"
    );
}

#[test]
fn a_stopped_session_sent_to_with_start_is_given_it_once_its_agent_is_ready() {
    let Some(t) = TestEnv::new("sendstart") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "c"]);
    let c = t.session("c", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let payload = format!(
        r#"{{"session_id":"x","transcript_path":"{}"}}"#,
        t.path("x.jsonl").display()
    );
    t.report(&c, "session", &payload);
    t.ok(&["stop", "--yes", "c"]);

    let out = t.ok(&["send", "c", "--start", "hello claude"]);
    assert!(out.contains("Queued for demo/c (next)"), "{out}");
    t.wait_for("its agent again", |t| {
        let out = t.run(&["show", "c"]);
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("fake agent:")
    });
    assert!(!t.ok(&["show", "c"]).contains("hello claude"));
    // Ready once it says so.
    t.report(&c, "session", &payload);
    t.wait_for("the prompt in its agent", |t| {
        (t.ok(&["show", "c"]).lines()).any(|l| l.trim() == "hello claude")
    });
}

#[test]
fn codex_started_for_a_prompt_sent_is_given_it_on_its_command_line() {
    let Some(t) = TestEnv::new("sendcodex") else {
        return;
    };
    crate::codex::with_fake_codex(&t);
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "c", "--prompt", "first task"]);
    t.wait_for("its first turn", |t| {
        let s = t.session("c", false).unwrap();
        s["state"] == "idle" && s["unread"] == true
    });
    t.ok(&["stop", "--yes", "c"]);

    // Ready only after a turn, it's given the first on its command line as
    // it resumes, and the next typed in after that turn.
    let out = t.ok(&["send", "c", "--start", "second task"]);
    assert!(out.contains("Typed into demo/c"), "{out}");
    let out = t.ok(&["send", "c", "third task"]);
    // (Typed at once if that turn is over already.)
    assert!(
        out.contains("Queued for demo/c (next)") || out.contains("Typed into demo/c"),
        "{out}"
    );
    for turn in ["turn: second task", "turn: third task"] {
        t.wait_for(turn, |t| t.ok(&["show", "c"]).contains(turn));
    }
    // Typed in once the turn before had ended: that turn's report came first.
    let screen = t.ok(&["show", "c"]);
    let at = |text: &str| screen.find(text).unwrap();
    assert!(at("turn: second task") < at("got: third task"), "{screen}");
    // Too long for its command line, where one may go: not sent.
    let mut child = (t.command(&["send", "c", "-"]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let long = "x".repeat(200_000);
    (child.stdin.take().unwrap())
        .write_all(long.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        !out.status.success()
            && said(&out).contains("200000 bytes are too long for codex's command line"),
        "{}",
        said(&out)
    );
    let argv = fs::read_to_string(t.codex_dir().join("argv")).unwrap();
    assert!(
        argv.contains("resume\n") && argv.ends_with("--\nsecond task\n"),
        "{argv}"
    );

    // Resumed with nothing to give it, it says it's ready only once it's had
    // a turn.
    t.ok(&["stop", "--yes", "c"]);
    t.ok(&["start", "c"]);
    let out = t.ok(&["send", "c", "held"]);
    assert!(
        out.contains("Codex says it's ready only after a turn"),
        "{out}"
    );
    // Started to attach to, it isn't given what's queued: what you type
    // goes first.
    t.ok(&["stop", "--yes", "c"]);
    fs::remove_file(t.codex_dir().join("argv")).unwrap();
    let term = Terminal::start(&t, &format!("{} attach c", t.shell_command()), (100, 30));
    term.wait_for("codex", "fake codex:");
    let argv = fs::read_to_string(t.codex_dir().join("argv")).unwrap();
    assert!(
        argv.contains("resume\n") && !argv.contains("--\n"),
        "{argv}"
    );
    term.keys(&["C-q"]);
    let list = t.ok(&["send", "c", "--list"]);
    assert!(list.contains("held"), "{list}");
}

#[test]
fn a_prompt_sent_waits_for_a_turn_a_question_and_you_at_its_agent() {
    let Some(t) = TestEnv::new("sendwait") else {
        return;
    };
    add_agents(&t, "");
    // Sent as its first prompt goes in: after that prompt's turn.
    let new = (t.command(&["new", "demo", "--name", "c", "--prompt", "first"]))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    t.wait_for("its agent", |t| {
        let out = t.run(&["show", "c"]);
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("fake agent:")
    });
    let c = t.session("c", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let payload = format!(
        r#"{{"session_id":"x","transcript_path":"{}"}}"#,
        t.path("x.jsonl").display()
    );
    t.report(&c, "session", &payload);
    let out = new.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Sent the first prompt"));
    let out = t.ok(&["send", "c", "second"]);
    assert!(
        out.contains("Queued for demo/c (next): it's working"),
        "{out}"
    );
    // Nor while it asks something.
    t.report(&c, "needs-input", &payload);
    assert_eq!(t.session("c", false).unwrap()["state"], "needs-input");
    let out = t.ok(&["send", "c", "third"]);
    assert!(
        out.contains("(1 ahead of it): it asks something first"),
        "{out}"
    );
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(!(t.ok(&["show", "c"]).lines()).any(|l| l.trim() == "second"));
    t.report(&c, "idle", &payload);
    t.wait_for("the next in, after the turn", |t| {
        (t.ok(&["show", "c"]).lines()).any(|l| l.trim() == "second")
    });
    // One at a time: the next waits for that turn's end.
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(!(t.ok(&["show", "c"]).lines()).any(|l| l.trim() == "third"));
    t.report(&c, "idle", &payload);
    t.wait_for("the last in", |t| {
        (t.ok(&["show", "c"]).lines()).any(|l| l.trim() == "third")
    });

    // Nor while you're at it: once you're back.
    t.ok(&["new", "demo", "--agent", "calm", "--name", "g"]);
    t.wait_for("its agent idle", |t| {
        t.session("g", false).unwrap()["state"] == "idle"
    });
    let term = Terminal::start(&t, &format!("{} attach g", t.shell_command()), (100, 30));
    term.wait_for("the agent", "calm agent ready");
    let out = t.ok(&["send", "g", "while you were there"]);
    assert!(
        out.contains("Queued for demo/g (next): a terminal is attached"),
        "{out}"
    );
    std::thread::sleep(std::time::Duration::from_secs(6));
    assert!(!term.screen().contains("while you were there"));
    term.keys(&["C-q"]);
    t.wait_for("the prompt in, once you're back", |t| {
        (t.ok(&["show", "g"]).lines()).any(|l| l.trim() == "while you were there")
    });
}

#[test]
fn a_turn_another_session_began_ends_read_and_yours_as_they_were() {
    let Some(t) = TestEnv::new("sendread") else {
        return;
    };
    add_agents(&t, "");
    t.ok(&["new", "demo", "--agent", "calm", "--name", "lead"]);
    t.ok(&["new", "demo", "--name", "c"]);
    let lead = t.session("lead", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = t.session("c", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let transcript = t.path("x.jsonl");
    let hook = |prompt: &str| {
        serde_json::json!({
            "session_id": "x",
            "transcript_path": transcript,
            "prompt": prompt,
        })
        .to_string()
    };
    let unread = |t: &TestEnv| t.session("c", false).unwrap()["unread"] == true;
    t.report(&c, "session", &hook(""));
    // What a turn sent by another ends with went back to it: read.
    let out = run_in(&t, &lead, &["send", "c", "from the lead"]);
    assert!(said(&out).contains("Typed into demo/c"), "{}", said(&out));
    t.report(&c, "prompt", &hook("from the lead"));
    t.report(&c, "idle", &hook(""));
    assert!(!unread(&t));
    // Yours sent so are as they were: unread.
    assert!(
        t.ok(&["send", "c", "from you"])
            .contains("Typed into demo/c")
    );
    t.report(&c, "prompt", &hook("from you"));
    t.report(&c, "idle", &hook(""));
    assert!(unread(&t));
    t.ok(&["read", "c"]);
    // As is one sent by another that you typed into too.
    let out = run_in(&t, &lead, &["send", "c", "again from the lead"]);
    assert!(said(&out).contains("Typed into demo/c"), "{}", said(&out));
    t.report(&c, "prompt", &hook("and from you"));
    t.report(&c, "idle", &hook(""));
    assert!(unread(&t));
    t.ok(&["read", "c"]);

    // A screen-tracked agent's turn after it was typed in.
    t.ok(&["new", "demo", "--agent", "calm", "--name", "g"]);
    let g_unread = |t: &TestEnv| t.session("g", false).unwrap()["unread"] == true;
    t.wait_for("g ready", |t| {
        let out = run_in(t, &lead, &["send", "g", "from the lead"]);
        String::from_utf8_lossy(&out.stdout).contains("Typed into demo/g")
            || !t.ok(&["send", "g", "--list"]).is_empty()
    });
    t.wait_for("its turn over", |t| {
        (t.ok(&["show", "g"]).lines()).any(|l| l.trim() == "from the lead")
            && t.session("g", false).unwrap()["state"] == "idle"
    });
    assert!(!g_unread(&t));
    t.ok(&["send", "g", "from you"]);
    t.wait_for("yours over, unread", g_unread);
}
