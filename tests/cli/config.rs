//! Roster's settings and config.toml, and the tmux config they make.

use std::fs;
use std::process::Command;

use crate::common::TestEnv;

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
fn a_setting_roster_no_longer_has_is_said_not_refused() {
    let Some(t) = TestEnv::new("gone") else {
        return;
    };
    let file = t.config_dir().join("config.toml");
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str("[keys.view]\ngroup = \"g\"\n");
    fs::write(&file, &text).unwrap();
    // The key that grouped by project: every command still runs, and says
    // what replaced it.
    let out = t.run(&["ls"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("keys.view.group is gone") && err.contains("group_by"),
        "{err}"
    );
    // Once per command, though config set reads the file again.
    let out = t.run(&["config", "set", "group_by", "project"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(err.matches("keys.view.group is gone").count(), 1, "{err}");
    // A config mended with --edit that still has it is saved: said, not
    // failed.
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str("no_such_setting = 1\n");
    fs::write(&file, &text).unwrap();
    let mends = t.path("mends.sh");
    fs::write(&mends, "#!/bin/sh\nsed -i '/no_such_setting/d' \"$1\"\n").unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&mends)
        .status()
        .unwrap();
    let out = t
        .command(&["config", "--edit"])
        .env("EDITOR", &mends)
        .env_remove("VISUAL")
        .output()
        .unwrap();
    let (said, err) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{said}{err}");
    assert!(said.contains("config.toml saved"), "{said}");
    assert!(err.contains("keys.view.group is gone"), "{err}");
}
