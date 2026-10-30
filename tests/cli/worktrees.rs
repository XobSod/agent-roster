//! Sessions in git worktrees: made, shared, set up, trusted, made again when
//! gone, and what deleting leaves of them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use crate::common::TestEnv;

/// Stand-in for an agent without hooks: replies to each line.
const GENERIC_AGENT: &str = include_str!("../fakes/generic.sh");

/// `git args…` in `dir`, as nobody's config would have it.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A project `demo` in a repository on `main` with a commit, run by a
/// generic agent `echo`.
fn demo(t: &TestEnv) -> std::path::PathBuf {
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
    let repo = t.path("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    t.ok(&["project", "add", "demo", repo.to_str().unwrap()]);
    fs::canonicalize(&repo).unwrap()
}

fn worktree(t: &TestEnv, name: &str) -> Value {
    t.session(name, false).unwrap()["worktree"].clone()
}

#[test]
fn a_session_works_in_a_worktree_made_for_it_or_shared() {
    let Some(t) = TestEnv::new("worktrees") else {
        return;
    };
    let repo = demo(&t);
    // Outside git, none.
    t.ok(&["project", "add", "plain", t.path("proj").to_str().unwrap()]);
    let out = t.run(&["new", "plain", "--agent", "echo", "--worktree"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("isn't in a git repository"), "{err}");

    // Set up once, by the project's script, told where the checkout is;
    // the script, not committed, is said to stay behind.
    fs::write(
        repo.join(".agent-roster.worktree"),
        "printf '%s' \"$ROSTER_CHECKOUT\" > from-setup\n",
    )
    .unwrap();
    let out = t.run(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "one",
        "--worktree",
    ]);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success()
            && said.contains("The checkout of demo has 1 file changed, not committed"),
        "{said}"
    );
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/one")).unwrap();
    let w = worktree(&t, "one");
    assert_eq!(
        (&w["path"], &w["branch"], &w["base"], &w["made_by_roster"]),
        (&json!(root), &json!("one"), &json!("main"), &json!(true))
    );
    assert_eq!(t.session("one", false).unwrap()["workdir"], json!(root));
    t.wait_for("set up", |t| worktree(t, "one")["set_up"] == json!(true));
    assert_eq!(
        fs::read_to_string(root.join("from-setup")).unwrap(),
        repo.to_str().unwrap()
    );
    fs::remove_file(root.join("from-setup")).unwrap();

    // Its details say how it stands.
    let info = t.ok(&["info", "one"]);
    assert!(
        info.contains("worktree     one ← main · even with it"),
        "{info}"
    );
    // Another in it, by the session's name: set up already.
    t.ok(&[
        "new", "demo", "--agent", "echo", "--name", "two", "--in", "one",
    ]);
    let w = worktree(&t, "two");
    assert_eq!(
        (&w["path"], &w["made_by_roster"]),
        (&json!(root), &json!(true))
    );
    assert_eq!(w["set_up"], json!(true));
    // A branch can't be checked out twice; one that's there is checked out
    // as it is, from nothing.
    let out = t.run(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "x",
        "--worktree",
        "one",
    ]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("branch one is checked out already"), "{err}");
    git(&repo, &["branch", "feature"]);
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "four",
        "--worktree",
        "feature",
    ]);
    // Counted against the default branch, having no base of its own.
    let w = worktree(&t, "four");
    assert_eq!(
        (&w["branch"], &w["base"]),
        (&json!("feature"), &json!("main"))
    );

    // Deleting one of two working in it leaves the worktree to the other.
    let out = t.ok(&["rm", "--yes", "two"]);
    assert!(out.contains("stays: demo/one works in it"), "{out}");
    assert!(root.exists());
    // The last: its branch asked about, which --yes doesn't answer.
    let out = t.run(&["rm", "--yes", "one"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success() && err.contains("Its branch one is merged into main"),
        "{err}"
    );
    assert!(err.contains("--delete-branch or --keep-branch"), "{err}");
    let out = t.ok(&["rm", "--yes", "--delete-branch", "one"]);
    assert!(
        out.contains("Removed its worktree") && out.contains("Deleted its branch one"),
        "{out}"
    );
    assert!(!root.exists());
    assert!(git(&repo, &["branch", "--list", "one"]).is_empty());
    // With changes not committed, it stays unless they're to go too.
    let four = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/feature")).unwrap();
    // What its setup made, and a file of the agent's.
    assert!(four.join("from-setup").exists());
    fs::write(four.join("new.txt"), "new\n").unwrap();
    let out = t.ok(&["rm", "--yes", "four"]);
    assert!(
        out.contains("stays, with its 2 files changed, not committed"),
        "{out}"
    );
    assert!(four.exists());
    assert!(t.session("four", false).is_none());
}

#[test]
fn a_worktree_gone_is_made_again_from_its_branch() {
    let Some(t) = TestEnv::new("worktree-gone") else {
        return;
    };
    let repo = demo(&t);
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "five",
        "--worktree",
    ]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/five")).unwrap();
    t.ok(&["stop", "--yes", "five"]);
    fs::remove_dir_all(&root).unwrap();
    // Asked first, as something removed it.
    let out = t.run(&["start", "five"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    // Said with what else it asks: a generic agent can't resume.
    assert!(
        err.contains(
            "is gone: it's made again from branch five, where it was. demo/five (echo) can't resume"
        ),
        "{err}"
    );
    t.ok(&["start", "--yes", "five"]);
    assert!(root.join("a.txt").exists());
    // Its branch gone too: it can't start.
    t.ok(&["stop", "--yes", "five"]);
    fs::remove_dir_all(&root).unwrap();
    git(&repo, &["worktree", "prune"]);
    git(&repo, &["branch", "-D", "five"]);
    let out = t.run(&["start", "--yes", "five"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        err.contains("and so is its branch five: it can't start"),
        "{err}"
    );
    // Deleting one whose worktree git has forgotten too goes as any.
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "six",
        "--worktree",
    ]);
    let six = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/six")).unwrap();
    t.ok(&["stop", "--yes", "six"]);
    fs::remove_dir_all(&six).unwrap();
    git(&repo, &["worktree", "prune"]);
    let out = t.ok(&["rm", "--yes", "--delete-branch", "six"]);
    assert!(
        out.contains("Removed its worktree") && out.contains("Deleted its branch six"),
        "{out}"
    );
}

#[test]
fn a_worktree_is_trusted_in_claude_as_its_checkout_is_till_its_removed() {
    let Some(t) = TestEnv::new("worktree-trust") else {
        return;
    };
    let repo = demo(&t);
    let config = t.claude_dir().join(".claude.json");
    fs::create_dir_all(t.claude_dir()).unwrap();
    let entry = |dir: &Path| -> Value {
        let config: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        config["projects"][dir.to_str().unwrap()].clone()
    };
    // The checkout not trusted: nor is the worktree.
    fs::write(&config, r#"{"numStartups":1}"#).unwrap();
    t.ok(&["new", "demo", "--name", "untrusted", "--worktree"]);
    let untrusted =
        fs::canonicalize(t.path("state/agent-roster/worktrees/demo/untrusted")).unwrap();
    t.wait_for("claude up", |t| {
        t.session("untrusted", false).unwrap()["state"] != "stopped"
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(entry(&untrusted), Value::Null);
    // Trusted: the worktree is too, as it starts, and not after it's gone.
    fs::write(
        &config,
        format!(
            r#"{{"numStartups":1,"projects":{{"{}":{{"hasTrustDialogAccepted":true}}}}}}"#,
            repo.display()
        ),
    )
    .unwrap();
    t.ok(&["new", "demo", "--name", "trusted", "--worktree"]);
    let trusted = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/trusted")).unwrap();
    t.wait_for("trusted in claude", |_| {
        entry(&trusted)["hasTrustDialogAccepted"] == json!(true)
    });
    t.ok(&["rm", "--yes", "--keep-branch", "trusted"]);
    assert!(!trusted.exists());
    assert_eq!(entry(&trusted), Value::Null);
    assert_eq!(entry(&repo)["hasTrustDialogAccepted"], json!(true));
}

#[test]
fn a_shared_worktrees_trust_is_taken_back_wherever_it_was_written_whoever_leaves_last() {
    let Some(t) = TestEnv::new("worktree-trust-shared") else {
        return;
    };
    let repo = demo(&t);
    // Its agents' config where the project's env file says.
    let custom = t.path("custom-claude");
    fs::create_dir_all(&custom).unwrap();
    fs::write(
        repo.join(".agent-roster.env"),
        format!("CLAUDE_CONFIG_DIR={}\n", custom.display()),
    )
    .unwrap();
    let config = custom.join(".claude.json");
    fs::write(
        &config,
        format!(
            r#"{{"numStartups":1,"projects":{{"{}":{{"hasTrustDialogAccepted":true}}}}}}"#,
            repo.display()
        ),
    )
    .unwrap();
    let entry = |dir: &Path| -> Value {
        let config: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        config["projects"][dir.to_str().unwrap()].clone()
    };
    t.ok(&["new", "demo", "--name", "one", "--worktree"]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/one")).unwrap();
    t.wait_for("trusted there", |_| {
        entry(&root)["hasTrustDialogAccepted"] == json!(true)
    });
    // Its agent up: its trust was recorded before it was run.
    let screen = |t: &TestEnv, name: &str| {
        let id = t.session(name, false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = Command::new("tmux")
            .args([
                "-L",
                &t.socket,
                "capture-pane",
                "-p",
                "-t",
                &format!("r-{id}"),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    t.wait_for("its agent up", |t| screen(t, "one").contains("fake agent"));
    // Another in it, trusted already: nothing written of its own.
    t.ok(&["new", "demo", "--name", "two", "--in", "one"]);
    t.wait_for("two up", |t| screen(t, "two").contains("fake agent"));
    // The one that wrote it deleted first, the other last: taken back.
    let out = t.ok(&["rm", "--yes", "one"]);
    assert!(out.contains("stays: demo/two works in it"), "{out}");
    let out = t.ok(&["rm", "--yes", "--keep-branch", "two"]);
    assert!(out.contains("Removed its worktree"), "{out}");
    assert_eq!(entry(&root), Value::Null);
    assert_eq!(entry(&repo)["hasTrustDialogAccepted"], json!(true));
}

#[test]
fn a_worktrees_trust_goes_to_another_projects_session_in_it_as_its_project_goes() {
    let Some(t) = TestEnv::new("worktree-trust-project") else {
        return;
    };
    let repo = demo(&t);
    t.ok(&["project", "add", "other", repo.to_str().unwrap()]);
    let custom = t.path("custom-claude");
    fs::create_dir_all(&custom).unwrap();
    fs::write(
        repo.join(".agent-roster.env"),
        format!("CLAUDE_CONFIG_DIR={}\n", custom.display()),
    )
    .unwrap();
    let config = custom.join(".claude.json");
    fs::write(
        &config,
        format!(
            r#"{{"numStartups":1,"projects":{{"{}":{{"hasTrustDialogAccepted":true}}}}}}"#,
            repo.display()
        ),
    )
    .unwrap();
    let entry = |dir: &Path| -> Value {
        let config: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        config["projects"][dir.to_str().unwrap()].clone()
    };
    let screen = |t: &TestEnv, name: &str| {
        let id = t.session(name, false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = Command::new("tmux")
            .args([
                "-L",
                &t.socket,
                "capture-pane",
                "-p",
                "-t",
                &format!("r-{id}"),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    t.ok(&["new", "demo", "--name", "one", "--worktree"]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/one")).unwrap();
    t.wait_for("trusted there", |_| {
        entry(&root)["hasTrustDialogAccepted"] == json!(true)
    });
    t.wait_for("its agent up", |t| screen(t, "one").contains("fake agent"));
    t.ok(&["new", "other", "--name", "two", "--in", "demo/one"]);
    t.wait_for("two up", |t| screen(t, "two").contains("fake agent"));
    // Its project removed, the session that wrote it archived in it: the
    // other's, which takes it back as it leaves last.
    t.ok(&["archive", "--yes", "demo/one"]);
    t.ok(&["project", "rm", "--yes", "demo"]);
    assert!(root.exists());
    let out = t.ok(&["rm", "--yes", "--keep-branch", "other/two"]);
    assert!(out.contains("Removed its worktree"), "{out}");
    assert_eq!(entry(&root), Value::Null);
}

#[test]
fn a_conversation_that_ran_in_a_worktree_is_adopted_into_its_repositorys_project() {
    let Some(t) = TestEnv::new("worktree-adopt") else {
        return;
    };
    let repo = demo(&t);
    // One made by hand, beside the checkout.
    let elsewhere = t.path("elsewhere");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "mine",
            elsewhere.to_str().unwrap(),
        ],
    );
    let elsewhere = fs::canonicalize(&elsewhere).unwrap();
    t.conversation("c0ffee01", &elsewhere, "Mine");
    let listed = t.ok(&["adopt", "--list"]);
    assert!(
        listed.contains("c0ffee01") && listed.contains("demo"),
        "{listed}"
    );
    t.ok(&["adopt", "c0ffee01", "--name", "mine"]);
    let s = t.session("mine", false).unwrap();
    assert_eq!(s["project"], "demo");
    assert_eq!(s["workdir"], json!(elsewhere));
    let w = &s["worktree"];
    assert_eq!(
        (&w["path"], &w["branch"], &w["made_by_roster"]),
        (&json!(elsewhere), &json!("mine"), &json!(false))
    );
    // A detached one: adopted there, as any directory, no worktree of
    // roster's.
    let detached = t.path("detached");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().unwrap(),
        ],
    );
    let detached = fs::canonicalize(&detached).unwrap();
    t.conversation("c0ffee03", &detached, "Detached");
    t.ok(&["adopt", "c0ffee03", "--name", "detached"]);
    let s = t.session("detached", false).unwrap();
    assert_eq!(
        (&s["project"], &s["worktree"]),
        (&json!("demo"), &Value::Null)
    );
    // One of roster's, gone since with its session, its branch kept: made
    // again where it was.
    t.ok(&["new", "demo", "--name", "gone", "--worktree"]);
    let gone = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/gone")).unwrap();
    t.ok(&["rm", "--yes", "--keep-branch", "gone"]);
    assert!(!gone.exists());
    t.conversation("c0ffee02", &gone, "Gone");
    let out = t.run(&["adopt", "c0ffee02", "--name", "back"]);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success() && said.contains("again from branch gone"),
        "{said}"
    );
    assert!(gone.join("a.txt").exists());
    let w = &t.session("back", false).unwrap()["worktree"];
    assert_eq!(
        (&w["branch"], &w["made_by_roster"]),
        (&json!("gone"), &json!(true))
    );
    // One of roster's left for its changes as its session went: roster's
    // still, so it goes with the session adopted there.
    t.ok(&["new", "demo", "--name", "kept", "--worktree"]);
    let kept = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/kept")).unwrap();
    fs::write(kept.join("wip.txt"), "wip\n").unwrap();
    let out = t.ok(&["rm", "--yes", "--keep-branch", "kept"]);
    assert!(out.contains("stays, with its 1 file changed"), "{out}");
    t.conversation("c0ffee05", &kept, "Kept");
    t.ok(&["adopt", "c0ffee05", "--name", "again"]);
    let w = &t.session("again", false).unwrap()["worktree"];
    assert_eq!(
        (&w["made_by_roster"], &w["base"]),
        (&json!(true), &json!("main"))
    );
    let out = t.ok(&[
        "rm",
        "--yes",
        "--discard-changes",
        "--delete-branch",
        "again",
    ]);
    assert!(out.contains("Removed its worktree"), "{out}");
    assert!(!kept.exists());
}

#[test]
fn a_worktree_another_session_works_in_stays_though_not_as_its_own() {
    let Some(t) = TestEnv::new("worktree-shared-dir") else {
        return;
    };
    demo(&t);
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "one",
        "--worktree",
    ]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/one")).unwrap();
    t.ok(&["stop", "--yes", "one"]);
    // Detached, a conversation that ran there is adopted as any directory's:
    // no worktree of its own.
    git(&root, &["switch", "-q", "--detach"]);
    t.conversation("c0ffee04", &root, "There");
    t.ok(&["adopt", "c0ffee04", "--name", "two"]);
    assert_eq!(worktree(&t, "two"), Value::Null);
    let out = t.ok(&["rm", "--yes", "--keep-branch", "one"]);
    assert!(out.contains("stays: demo/two works in it"), "{out}");
    assert!(root.join("a.txt").exists());
}

#[test]
fn a_shared_worktree_not_set_up_yet_is_set_up_before_any_agent_in_it_starts() {
    let Some(t) = TestEnv::new("worktree-shared-setup") else {
        return;
    };
    let repo = demo(&t);
    // A setup that fails: no agent starts, the next start runs it again.
    let script = repo.join(".agent-roster.worktree");
    fs::write(&script, "echo ran >> \"$ROSTER_CHECKOUT/ran\"\nexit 3\n").unwrap();
    let runs = || {
        fs::read_to_string(repo.join("ran"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "one",
        "--worktree",
    ]);
    t.wait_for("its setup failed", |t| {
        runs() == 1 && t.session("one", false).unwrap()["state"] == "stopped"
    });
    // Shared before it's set up: set up first, not taken for done.
    t.ok(&[
        "new", "demo", "--agent", "echo", "--name", "two", "--in", "one",
    ]);
    assert_eq!(worktree(&t, "two")["set_up"], json!(false));
    t.wait_for("its setup failed again", |t| {
        runs() == 2 && t.session("two", false).unwrap()["state"] == "stopped"
    });
    // Set up once it works, by either: the other's start runs it no more.
    fs::write(&script, "echo ran >> \"$ROSTER_CHECKOUT/ran\"\n").unwrap();
    t.ok(&["start", "--yes", "two"]);
    t.wait_for("set up", |t| worktree(t, "two")["set_up"] == json!(true));
    t.ok(&["start", "--yes", "one"]);
    t.wait_for("one up", |t| {
        t.session("one", false).unwrap()["state"] != "stopped"
    });
    assert_eq!(worktree(&t, "one")["set_up"], json!(true));
    assert_eq!(runs(), 3);
}

#[test]
fn a_worktree_git_makes_only_some_of_leaves_nothing_behind() {
    let Some(t) = TestEnv::new("worktree-half-made") else {
        return;
    };
    let repo = demo(&t);
    // Its checkout's hook fails: git has made the branch, the directory and
    // its record of them by then.
    let hook = repo.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\necho made > made-by-hook\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(&repo, &["branch", "there"]);
    let worktrees = t.path("state/agent-roster/worktrees/demo");
    for name in ["hooked", "there"] {
        let out = t.run(&[
            "new",
            "demo",
            "--agent",
            "echo",
            "--name",
            name,
            "--worktree",
        ]);
        assert!(!out.status.success(), "{name}");
        assert!(t.session(name, false).is_none());
        assert!(!worktrees.join(name).exists(), "{name}");
        let listed = git(&repo, &["worktree", "list", "--porcelain"]);
        assert!(!listed.contains(&format!("demo/{name}")), "{listed}");
    }
    // The branch it made goes; the one that was there stays.
    assert!(git(&repo, &["branch", "--list", "hooked"]).is_empty());
    assert!(!git(&repo, &["branch", "--list", "there"]).is_empty());
    // Made once the hook's mended, from scratch.
    fs::remove_file(&hook).unwrap();
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "hooked",
        "--worktree",
    ]);
    assert!(worktrees.join("hooked/a.txt").exists());
}

#[test]
fn a_worktree_is_set_up_by_one_session_at_a_time_and_anew_once_made_again() {
    let Some(t) = TestEnv::new("worktree-setup-once") else {
        return;
    };
    let repo = demo(&t);
    let script = repo.join(".agent-roster.worktree");
    let runs = || {
        fs::read_to_string(repo.join("ran"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    // Still at it as another session starts in it: that one waits for it.
    fs::write(
        &script,
        "echo ran >> \"$ROSTER_CHECKOUT/ran\"\nwhile [ ! -e \"$ROSTER_CHECKOUT/release\" ]; do sleep 0.1; done\n",
    )
    .unwrap();
    let new = |name: &str, how: &[&str]| {
        let mut args = vec!["new", "demo", "--agent", "echo", "--name", name];
        args.extend(how);
        t.ok(&args);
    };
    new("one", &["--worktree"]);
    t.wait_for("its setup running", |_| runs() == 1);
    new("two", &["--in", "one"]);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(runs(), 1);
    fs::write(repo.join("release"), "").unwrap();
    t.wait_for("both set up", |t| {
        worktree(t, "one")["set_up"] == json!(true) && worktree(t, "two")["set_up"] == json!(true)
    });
    assert_eq!(runs(), 1);
    // Gone, and made again: set up again, whichever starts first, till it
    // works.
    for name in ["one", "two"] {
        t.ok(&["stop", "--yes", name]);
    }
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/one")).unwrap();
    fs::remove_dir_all(&root).unwrap();
    fs::write(&script, "echo ran >> \"$ROSTER_CHECKOUT/ran\"\nexit 3\n").unwrap();
    let failed = |t: &TestEnv, name: &str, n: usize| {
        runs() == n && t.session(name, false).unwrap()["state"] == "stopped"
    };
    t.ok(&["start", "--yes", "one"]);
    t.wait_for("one's setup failed", |t| failed(t, "one", 2));
    t.ok(&["start", "--yes", "two"]);
    t.wait_for("two's setup failed", |t| failed(t, "two", 3));
    fs::write(&script, "echo ran >> \"$ROSTER_CHECKOUT/ran\"\n").unwrap();
    t.ok(&["start", "--yes", "two"]);
    t.wait_for("set up", |t| worktree(t, "two")["set_up"] == json!(true));
    t.ok(&["start", "--yes", "one"]);
    t.wait_for("one up", |t| worktree(t, "one")["set_up"] == json!(true));
    assert_eq!(runs(), 4);
}

#[test]
fn a_worktree_roster_didnt_make_stays_though_its_in_rosters_folder() {
    let Some(t) = TestEnv::new("worktree-not-ours") else {
        return;
    };
    let repo = demo(&t);
    // Made by hand where roster puts its own.
    let manual = t.path("state/agent-roster/worktrees/demo/manual");
    fs::create_dir_all(manual.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "manual",
            manual.to_str().unwrap(),
        ],
    );
    fs::write(manual.join(".local-env"), "mine\n").unwrap();
    let at = manual.to_str().unwrap();
    t.ok(&["new", "demo", "--agent", "echo", "--name", "m", "--in", at]);
    assert_eq!(worktree(&t, "m")["made_by_roster"], json!(false));
    // Gone, made again by roster for its session: still not roster's.
    t.ok(&["stop", "--yes", "m"]);
    git(&repo, &["worktree", "remove", "--force", at]);
    t.ok(&["start", "--yes", "m"]);
    assert!(manual.join("a.txt").exists());
    fs::write(manual.join(".local-env"), "mine\n").unwrap();
    let out = t.ok(&["rm", "--yes", "--keep-branch", "m"]);
    assert!(out.contains("stays: roster didn't make it"), "{out}");
    t.ok(&["new", "demo", "--agent", "echo", "--name", "n", "--in", at]);
    assert_eq!(worktree(&t, "n")["made_by_roster"], json!(false));
    let out = t.ok(&["rm", "--yes", "--keep-branch", "n"]);
    assert!(out.contains("stays: roster didn't make it"), "{out}");
    assert!(manual.join(".local-env").exists());
}

#[test]
fn a_worktree_made_by_hand_where_one_of_rosters_was_isnt_rosters() {
    let Some(t) = TestEnv::new("worktree-replaced") else {
        return;
    };
    let repo = demo(&t);
    // Roster's, left for its changes as its session went.
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "one",
        "--worktree",
    ]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/one")).unwrap();
    fs::write(root.join("wip.txt"), "wip\n").unwrap();
    let out = t.ok(&["rm", "--yes", "--keep-branch", "one"]);
    assert!(out.contains("stays, with its 1 file changed"), "{out}");
    // Removed, and another made there by hand.
    let at = root.to_str().unwrap();
    git(&repo, &["worktree", "remove", "--force", at]);
    git(&repo, &["worktree", "add", "-q", "-b", "manual", at]);
    fs::write(root.join(".local-env"), "mine\n").unwrap();
    t.ok(&[
        "new", "demo", "--agent", "echo", "--name", "two", "--in", at,
    ]);
    assert_eq!(worktree(&t, "two")["made_by_roster"], json!(false));
    let out = t.ok(&["rm", "--yes", "--keep-branch", "two"]);
    assert!(out.contains("stays: roster didn't make it"), "{out}");
    assert!(root.join(".local-env").exists());
    // Replaced the same way while its session's still there: not roster's
    // either, whatever the session has of it.
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "three",
        "--worktree",
    ]);
    let three = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/three")).unwrap();
    t.ok(&["stop", "--yes", "three"]);
    let at = three.to_str().unwrap();
    git(&repo, &["worktree", "remove", "--force", at]);
    git(&repo, &["worktree", "add", "-q", "-b", "by-hand", at]);
    fs::write(three.join(".local-env"), "mine\n").unwrap();
    let out = t.ok(&["rm", "--yes", "--keep-branch", "three"]);
    assert!(out.contains("stays: roster didn't make it"), "{out}");
    assert!(three.join(".local-env").exists());
}

#[test]
fn what_a_setup_leaves_running_and_sessions_made_at_once_wait_for_nothing() {
    let Some(t) = TestEnv::new("worktree-setup-children") else {
        return;
    };
    let repo = demo(&t);
    // A setup that leaves something running: the next session in it doesn't
    // wait on it.
    fs::write(repo.join(".agent-roster.worktree"), "sleep 20 &\n").unwrap();
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "one",
        "--worktree",
    ]);
    t.wait_for("set up", |t| worktree(t, "one")["set_up"] == json!(true));
    t.ok(&[
        "new", "demo", "--agent", "echo", "--name", "two", "--in", "one",
    ]);
    t.wait_for("two set up", |t| {
        worktree(t, "two")["set_up"] == json!(true)
    });
    // Several made at once, named for them: each its own.
    let made: Vec<_> = (0..3)
        .map(|_| {
            t.command(&["new", "demo", "--agent", "echo", "--worktree"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in made {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for name in ["echo-1", "echo-2", "echo-3"] {
        assert_eq!(worktree(&t, name)["branch"], json!(name));
    }
}

#[test]
fn what_a_worktree_holds_now_is_what_deleting_and_making_it_again_go_by() {
    let Some(t) = TestEnv::new("worktree-head") else {
        return;
    };
    demo(&t);
    let new = |name: &str| {
        t.ok(&[
            "new",
            "demo",
            "--agent",
            "echo",
            "--name",
            name,
            "--worktree",
        ]);
        t.ok(&["stop", "--yes", name]);
        fs::canonicalize(t.path(&format!("state/agent-roster/worktrees/demo/{name}"))).unwrap()
    };
    // Moved off its branch, to a commit no branch has: removing it would
    // lose it.
    let one = new("one");
    git(&one, &["switch", "-q", "--detach"]);
    fs::write(one.join("b.txt"), "b\n").unwrap();
    git(&one, &["add", "b.txt"]);
    git(&one, &["commit", "-q", "-m", "second"]);
    let out = t.ok(&["rm", "--yes", "--keep-branch", "one"]);
    assert!(
        out.contains("stays: it's at a commit no branch or tag has"),
        "{out}"
    );
    assert!(one.join("b.txt").exists());
    // Its branch renamed, then the worktree gone: made again on it as it's
    // named now.
    let two = new("two");
    git(&two, &["branch", "-m", "renamed"]);
    fs::remove_dir_all(&two).unwrap();
    let out = t.run(&["start", "two"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("made again from branch renamed"), "{err}");
    t.ok(&["start", "--yes", "two"]);
    assert_eq!(git(&two, &["branch", "--show-current"]).trim(), "renamed");
    assert_eq!(worktree(&t, "two")["branch"], json!("renamed"));
    // Renamed again, in it: deleting asks of the branch it's on.
    t.ok(&["stop", "--yes", "two"]);
    git(&two, &["branch", "-m", "again"]);
    let out = t.ok(&["rm", "--yes", "--delete-branch", "two"]);
    assert!(out.contains("Deleted its branch again"), "{out}");
    assert!(!two.exists());
}

#[test]
fn what_a_worktree_is_made_from_and_what_deleting_one_with_changes_asks() {
    let Some(t) = TestEnv::new("worktree-more") else {
        return;
    };
    let repo = demo(&t);
    let stderr = |out: &std::process::Output| String::from_utf8_lossy(&out.stderr).into_owned();
    // From a base: another branch's commit, counted against it.
    git(&repo, &["switch", "-q", "-c", "side"]);
    fs::write(repo.join("side.txt"), "side\n").unwrap();
    git(&repo, &["add", "side.txt"]);
    git(&repo, &["commit", "-q", "-m", "side"]);
    git(&repo, &["switch", "-q", "main"]);
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "b",
        "--worktree",
        "--base",
        "side",
    ]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/b")).unwrap();
    assert!(root.join("side.txt").exists());
    assert_eq!(worktree(&t, "b")["base"], "side");
    // Named by where the checkout was before: the branch that names.
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "p",
        "--worktree",
        "--base",
        "@{-1}",
    ]);
    assert_eq!(worktree(&t, "p")["base"], "side");
    let info = t.ok(&["info", "p"]);
    assert!(
        info.contains("worktree     p ← side · even with it"),
        "{info}"
    );
    t.ok(&["rm", "--yes", "--delete-branch", "p"]);
    // From an expression: the commit it names now, which it would name no
    // more in the worktree.
    let first = git(&repo, &["rev-parse", "side~1"]).trim().to_string();
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "e",
        "--worktree",
        "--base",
        "side~1",
    ]);
    assert_eq!(worktree(&t, "e")["base"], json!(first));
    let info = t.ok(&["info", "e"]);
    assert!(
        info.contains(&format!("worktree     e ← {} · even with it", &first[..7])),
        "{info}"
    );
    t.ok(&["rm", "--yes", "--delete-branch", "e"]);
    // A branch there: said, and taken from no base.
    let out = t.run(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "s",
        "--worktree",
        "side",
        "--base",
        "main",
    ]);
    assert!(
        stderr(&out).contains("so it isn't made from anything"),
        "{}",
        stderr(&out)
    );
    git(&repo, &["branch", "old"]);
    let out = t.run(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "o",
        "--worktree",
        "old",
    ]);
    assert!(
        stderr(&out).contains("Branch old is there already: checked out as it is"),
        "{}",
        stderr(&out)
    );
    assert_eq!(worktree(&t, "o")["base"], "main");
    // Named for it, a session isn't given an old branch's name.
    git(&repo, &["branch", "echo-1"]);
    t.ok(&["new", "demo", "--agent", "echo", "--worktree"]);
    assert!(t.session("echo-2", false).unwrap()["worktree"]["branch"] == "echo-2");
    // In one by a directory in it.
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "in",
        "--in",
        root.to_str().unwrap(),
    ]);
    assert_eq!(worktree(&t, "in")["path"], json!(root));
    t.ok(&["rm", "--yes", "in"]);
    // Changes not committed go only when asked to, its branch then asked of.
    fs::write(root.join("new.txt"), "new\n").unwrap();
    let out = t.run(&["rm", "--yes", "--discard-changes", "b"]);
    assert!(
        !out.status.success() && stderr(&out).contains("Its branch b is merged into side"),
        "{}",
        stderr(&out)
    );
    assert!(t.session("b", false).is_some());
    t.ok(&["rm", "--yes", "--discard-changes", "--keep-branch", "b"]);
    assert!(!root.exists());
    assert!(!git(&repo, &["branch", "--list", "b"]).is_empty());
    // The default branch, the checkout on another: deleting doesn't offer
    // to delete it.
    git(&repo, &["switch", "-q", "side"]);
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "m",
        "--worktree",
        "main",
    ]);
    t.ok(&["rm", "--yes", "m"]);
    assert!(!git(&repo, &["branch", "--list", "main"]).is_empty());
    git(&repo, &["switch", "-q", "main"]);
    // A directory for them inside the repository itself, refused; nor a
    // relative one.
    let config = t.config_dir().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("{text}[worktrees]\ndirectory = \"{}\"\n", repo.display()),
    )
    .unwrap();
    let out = t.run(&["new", "demo", "--agent", "echo", "--worktree", "inside"]);
    assert!(
        stderr(&out).contains("a working tree of the repository itself"),
        "{}",
        stderr(&out)
    );
    fs::write(&config, format!("{text}[worktrees]\ndirectory = \"wt\"\n")).unwrap();
    let out = t.run(&["new", "demo", "--agent", "echo", "--worktree", "relative"]);
    assert!(
        stderr(&out).contains("it has to be a whole path"),
        "{}",
        stderr(&out)
    );
    assert!(!git(&repo, &["branch", "--list", "inside", "relative"]).contains("inside"));
}

#[test]
fn a_project_in_a_subdirectory_works_there_in_its_worktrees_and_goes_with_them() {
    let Some(t) = TestEnv::new("worktree-sub") else {
        return;
    };
    let repo = demo(&t);
    fs::create_dir_all(repo.join("api")).unwrap();
    fs::write(repo.join("api/x.txt"), "x\n").unwrap();
    git(&repo, &["add", "api/x.txt"]);
    git(&repo, &["commit", "-q", "-m", "api"]);
    // A branch from before the subdirectory: the session can't work there,
    // and the branch, roster's not to undo, stays.
    git(&repo, &["branch", "older", "HEAD~1"]);
    t.ok(&["project", "add", "api", repo.join("api").to_str().unwrap()]);
    let out = t.run(&[
        "new",
        "api",
        "--agent",
        "echo",
        "--name",
        "o",
        "--worktree",
        "older",
    ]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't in the worktree"));
    assert!(!git(&repo, &["branch", "--list", "older"]).is_empty());
    assert!(!t.path("state/agent-roster/worktrees/api/older").exists());
    // Nor is another session's worktree it can't work in undone: it's theirs.
    t.ok(&[
        "new",
        "demo",
        "--agent",
        "echo",
        "--name",
        "o2",
        "--worktree",
        "older",
    ]);
    let theirs = fs::canonicalize(t.path("state/agent-roster/worktrees/demo/older")).unwrap();
    let out = t.run(&[
        "new", "api", "--agent", "echo", "--name", "o3", "--in", "o2",
    ]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't in the worktree"));
    assert!(theirs.join("a.txt").exists());
    t.ok(&["new", "api", "--agent", "echo", "--name", "w", "--worktree"]);
    let root = fs::canonicalize(t.path("state/agent-roster/worktrees/api/w")).unwrap();
    assert_eq!(
        t.session("w", false).unwrap()["workdir"],
        json!(root.join("api"))
    );
    // Removing the project asks of its archived sessions' worktrees and
    // branches, as deleting does.
    t.ok(&["archive", "--yes", "w"]);
    let out = t.run(&["project", "rm", "--yes", "api"]);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success() && said.contains("--delete-branch"),
        "{said}"
    );
    let out = t.ok(&["project", "rm", "--yes", "--delete-branches", "api"]);
    assert!(
        out.contains("Removed its worktree") && out.contains("Deleted its branch w"),
        "{out}"
    );
    assert!(!root.exists());
}
