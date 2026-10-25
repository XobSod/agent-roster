//! Claude: its conversations, adopted.

use std::fs;

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
