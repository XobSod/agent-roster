//! Claude: its conversations, adopted, and resumed while Claude has them.

use std::fs;

use serde_json::json;

use crate::common::TestEnv;

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
fn a_conversation_claude_deleted_isnt_replaced_unasked() {
    let Some(t) = TestEnv::new("conversation-gone") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    let id_of = |name: &str| {
        t.session(name, false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let on = |conversation: &str| {
        let transcript = t.path(&format!("{conversation}.jsonl"));
        json!({"session_id": conversation, "transcript_path": transcript}).to_string()
    };
    let started_with = |name: &str, args: &str| {
        let wanted = format!("fake agent: {args}");
        t.wait_for(&wanted, |t| t.ok(&["show", name]).contains(&wanted));
    };

    // A conversation that has had a turn, so Claude has written it.
    t.ok(&["new", "demo", "--name", "gone"]);
    let gone = id_of("gone");
    fs::write(t.path("conv-gone.jsonl"), "{}\n").unwrap();
    for kind in ["session", "prompt", "idle"] {
        t.report(&gone, kind, &on("conv-gone"));
    }
    t.ok(&["stop", "gone"]);
    // Claude deletes old ones: starting it asks first, saying so, however
    // it starts.
    fs::remove_file(t.path("conv-gone.jsonl")).unwrap();
    for command in ["start", "restart", "attach"] {
        let out = t.run(&[command, "gone"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success()
                && err.contains("demo/gone (claude) can't resume its conversation: Claude no longer has it, as it deletes old ones. Start a new one?")
                && err.contains("--yes"),
            "{command}: {err}"
        );
    }
    assert_eq!(t.session("gone", false).unwrap()["state"], "stopped");
    // Told to, it begins a new one on the same id; that one, not written
    // yet, starts again as it is.
    let out = t.ok(&["start", "--yes", "gone"]);
    assert!(out.starts_with("Started demo/gone"), "{out}");
    started_with("gone", "--session-id conv-gone");
    t.ok(&["stop", "--yes", "gone"]);
    t.ok(&["start", "gone"]);

    // A conversation that hasn't had its first message isn't written yet,
    // nor one just cleared: they start as they are, unasked.
    t.ok(&["new", "demo", "--name", "fresh"]);
    let fresh = id_of("fresh");
    t.report(&fresh, "session", &on("conv-fresh"));
    t.ok(&["stop", "fresh"]);
    t.ok(&["start", "fresh"]);
    started_with("fresh", "--session-id conv-fresh");
    fs::write(t.path("conv-fresh.jsonl"), "{}\n").unwrap();
    for kind in ["session", "prompt", "idle"] {
        t.report(&fresh, kind, &on("conv-fresh"));
    }
    let mut cleared: serde_json::Value = serde_json::from_str(&on("conv-cleared")).unwrap();
    cleared["source"] = json!("clear");
    t.report(&fresh, "session", &cleared.to_string());
    t.ok(&["stop", "fresh"]);
    t.ok(&["start", "fresh"]);
    started_with("fresh", "--session-id conv-cleared");

    // A copy forks its original until it has its own, unasked; a moved one
    // is a conversation Claude kept, asked about once it's gone.
    t.conversation("44444444-dddd", &t.path("proj"), "Kept");
    let moved = t.conversation("55555555-eeee", &t.path("proj"), "Moved");
    t.ok(&["adopt", "4444", "--fork", "--name", "copy"]);
    started_with(
        "copy",
        "--model claude-sonnet-5-5 --resume 44444444-dddd --fork-session --session-id",
    );
    t.ok(&["stop", "--yes", "copy"]);
    assert!(t.ok(&["start", "copy"]).starts_with("Started demo/copy"));
    t.ok(&["adopt", "5555", "--name", "moved"]);
    started_with("moved", "--model claude-sonnet-5-5 --resume 55555555-eeee");
    t.ok(&["stop", "--yes", "moved"]);
    fs::remove_file(moved).unwrap();
    let out = t.run(&["start", "moved"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Claude no longer has it"), "{err}");

    // One Claude has written, though roster saw no turn of it: taken up
    // inside Claude (`/resume`), or kept from before roster noted turns,
    // seen there as it starts. Asked about once it's gone.
    t.ok(&["new", "demo", "--name", "resumed"]);
    let resumed = id_of("resumed");
    let file = t.path("conv-resumed.jsonl");
    fs::write(&file, "{}\n").unwrap();
    let db = rusqlite::Connection::open(t.db()).unwrap();
    let begun = || -> bool {
        db.query_row(
            "SELECT conversation_begun FROM sessions WHERE id = ?1",
            [&resumed],
            |r| r.get(0),
        )
        .unwrap()
    };
    t.report(&resumed, "session", &on("conv-resumed"));
    t.session("resumed", false);
    assert!(begun());
    t.ok(&["stop", "--yes", "resumed"]);
    db.execute(
        "UPDATE sessions SET conversation_begun = 0 WHERE id = ?1",
        [&resumed],
    )
    .unwrap();
    t.ok(&["start", "resumed"]);
    t.session("resumed", false);
    assert!(begun());
    t.ok(&["stop", "--yes", "resumed"]);
    fs::remove_file(&file).unwrap();
    let out = t.run(&["start", "resumed"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Claude no longer has it"), "{err}");
}
