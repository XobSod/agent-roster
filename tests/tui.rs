//! The TUI driven like a user would: keys sent to a terminal (a separate tmux
//! server) running `roster`, with a stand-in agent instead of Claude.

mod common;

use common::{Terminal, TestEnv};

#[test]
fn tui_walkthrough() {
    let Some(t) = TestEnv::new("tui") else { return };
    let command = format!("{}; echo TUI-EXIT=$?; sleep 600", t.shell_command());
    let term = Terminal::start(&t, &command, (120, 30));
    term.wait_for("the empty list", "No projects yet");

    // A second TUI says where the first one is, and doesn't start.
    let second = Terminal::start_named(&t, "second", &command, (120, 10));
    second.wait_for("the refusal", "TUI-EXIT=1");
    let screen = second.screen();
    assert!(
        screen.contains("roster is already open (pid ") && screen.contains("roster ls"),
        "{screen}"
    );
    drop(second);

    // The project menu's form: directory first (the name follows it), then the name.
    term.keys(&["p"]);
    term.wait_for("the project menu", "n new");
    term.keys(&["n"]);
    term.wait_for("the new-project form", "New project");
    term.keys(&["C-u"]);
    term.text(t.path("proj").to_str().unwrap());
    term.wait_for("the name taken from the directory", "Name       proj");
    term.keys(&["Down", "C-u"]);
    term.text("demo");
    term.keys(&["Enter"]);
    term.wait_for("the project", "Added project demo");

    // The session menu's form starts on the agent, then its name; its screen
    // shows in the preview.
    term.keys(&["s", "n"]);
    term.wait_for("the new-session form", "New session");
    term.keys(&["Down"]);
    term.text("one");
    term.keys(&["Enter"]);
    term.wait_for("the agent's screen in the preview", "fake agent:");
    term.wait_for("the header", "demo / one");

    // Editing the session: rename it.
    term.keys(&["s", "e"]);
    term.wait_for("the edit form", "Edit session demo/one");
    term.keys(&["C-u"]);
    term.text("first");
    term.keys(&["Enter"]);
    term.wait_for("the rename", "demo / first");
    term.keys(&["s", "e", "C-u"]);
    term.text("one");
    term.keys(&["Enter"]);
    term.wait_for("the rename back", "demo / one");

    // Attach, type to the agent, come back with Ctrl+Q. While attached, a line at
    // the top says where you are, like the preview's header (`#` and a last `;`
    // show as typed).
    t.ok(&["edit", "one", "--title", "Fix #42;"]);
    term.keys(&["Enter"]);
    term.wait_for_gone("the agent full screen", "Enter attach");
    term.wait_for("the line at the top", "C-q back");
    let top = term.screen().lines().next().unwrap().to_string();
    assert!(top.starts_with(" DE demo / one / Fix #42; "), "{top}");
    assert!(top.contains("✱ claude"), "{top}");
    term.text("typed while attached");
    term.keys(&["Enter", "C-q"]);
    term.wait_for("the list again", "Enter attach");
    term.wait_for("the agent's echo in the preview", "typed while attached");
    t.ok(&["edit", "one", "--title", ""]);

    // Stop it: it moves to the Stopped shelf, still selected.
    term.keys(&["s", "x"]);
    term.wait_for("the stop", "Stopped demo/one");
    term.wait_for("the Stopped shelf", "Stopped (1)");
    term.keys(&["s", "d"]);
    term.wait_for("the confirmation", "Archive demo/one?");
    term.keys(&["y"]);
    term.wait_for("the archive", "Archived demo/one");

    // It's on the folded Archived shelf, which gets the selection; Enter restores.
    term.wait_for("the Archived shelf", "Archived (1)");
    term.keys(&["l", "j"]);
    term.wait_for("the archived row", "archived now");
    term.keys(&["Enter"]);
    term.wait_for("the restore", "Restored demo/one");

    // The view menu's server view shows tmux and the database; Esc goes back.
    term.keys(&["v", "t"]);
    term.wait_for("the server view", "tmux server and database");
    term.wait_for("the verdict", "problems (0)");
    term.keys(&["Escape"]);
    term.wait_for("the list again", "Enter attach");

    // Help lists the menus; any key closes it.
    term.keys(&["?"]);
    term.wait_for("the help", "Session menu (s …)");
    term.keys(&["Escape"]);
    term.wait_for_gone("the help closed", "Session menu (s …)");

    // Archived again, then deleted for good.
    term.keys(&["s", "d"]);
    term.wait_for("the confirmation", "Archive demo/one?");
    term.keys(&["y"]);
    term.wait_for("the archive", "Archived demo/one");
    term.keys(&["s", "D"]);
    term.wait_for("the confirmation", "Delete demo/one for good?");
    term.keys(&["y"]);
    term.wait_for("the deletion", "Deleted demo/one for good");

    term.keys(&["q"]);
    term.wait_for("the TUI to exit", "TUI-EXIT=0");
    // tmux's "[detached (from session …)]" isn't left behind, once per attach.
    let screen = term.screen();
    assert!(!screen.contains("[detached"), "{screen}");
    assert!(t.session("one", false).is_none() && t.session("one", true).is_none());
}

#[test]
fn agents_drawing_full_screen_fit_the_preview() {
    let Some(t) = TestEnv::new("fit") else { return };
    // A generic agent too: tracked by its screen, its redraw mustn't look like work.
    let config = t.config_dir().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text += &format!(
        "[agents.app]\ncommand = \"{}\"\nidle_after = 1\n",
        t.path("fake-agent").display()
    );
    std::fs::write(&config, text).unwrap();
    let full = t.path("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join(".fullscreen"), "").unwrap();
    t.ok(&[
        "project",
        "add",
        "classic",
        t.path("proj").to_str().unwrap(),
    ]);
    t.ok(&["project", "add", "full", full.to_str().unwrap()]);
    t.ok(&["new", "classic", "--name", "a"]);
    t.ok(&["new", "full", "--name", "b"]);
    t.ok(&["new", "full", "--name", "c", "--agent", "app"]);
    // Until they've settled (just started, they count as working), and only with
    // `roster ls` before the TUI runs: another roster process doesn't know the
    // TUI resized an agent, so it would take the redraw for work.
    t.wait_for("the agents idle", |t| {
        ["a", "b", "c"]
            .iter()
            .all(|n| t.session(n, false).unwrap()["state"] == "idle")
    });
    let ids: std::collections::HashMap<&str, String> = ["a", "b", "c"]
        .map(|n| {
            (
                n,
                t.session(n, false).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .into();
    let size = |name: &str| -> String {
        let id = &ids[name];
        let out = std::process::Command::new("tmux")
            .args(["-L", &t.socket, "display", "-p", "-t"])
            .arg(format!("=r-{id}:"))
            .arg("#{window_width}x#{window_height}")
            .env_remove("TMUX")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // Started outside a terminal, sessions get 160x48, less a row for the line
    // at the top while attached.
    assert_eq!(size("a"), "160x47");

    // In a 120x30 terminal the preview is 82x26: what's left of the sidebar (36),
    // its divider and a column of space; below the header, statistics and a blank
    // line, above the footer.
    let term = Terminal::start(&t, &t.shell_command(), (120, 30));
    term.wait_for("the list", "Enter attach");
    t.wait_for("the full-screen agents fitted", |_| {
        size("b") == "82x26" && size("c") == "82x26"
    });
    // The agents redrew for their new size; the generic one doesn't count as working.
    for _ in 0..30 {
        let screen = term.screen();
        assert!(!screen.contains("Working ("), "{screen}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(size("a"), "160x47");

    // The terminal resized: so is the preview, and so are they (the list is
    // at its widest, 40).
    term.resize((140, 34));
    t.wait_for("the agents fitted again", |_| {
        size("b") == "98x30" && size("c") == "98x30"
    });

    // Attached, an agent has the terminal's size; back, the preview's again.
    for _ in 0..10 {
        if term.screen().contains("full / b ") {
            break;
        }
        term.keys(&["j"]);
    }
    term.wait_for("b selected", "full / b ");
    term.keys(&["Enter"]);
    term.wait_for_gone("the agent full screen", "Enter attach");
    // (less the line at the top)
    t.wait_for("b at the terminal's size", |_| size("b") == "140x33");
    term.keys(&["C-q"]);
    term.wait_for("the list again", "Enter attach");
    t.wait_for("b fitted again", |_| size("b") == "98x30");
    // The classic agent (normal screen, scrollback in tmux) was never resized.
    assert_eq!(size("a"), "160x47");
}

#[test]
fn pi_offers_the_models_it_lists() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("models") else {
        return;
    };
    // A stand-in pi that only lists its models.
    let pi = t.path("fake-pi");
    std::fs::write(
        &pi,
        "#!/bin/sh\n[ \"$1\" = --list-models ] || exit 1\nprintf 'provider  model         context  max-out\\nfake      fake-model-1  1M       128K\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text += &format!("[agents.pi]\ncommand = \"{}\"\n", pi.display());
    std::fs::write(&config, text).unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);

    let term = Terminal::start(&t, &t.shell_command(), (120, 30));
    term.wait_for("the list", "Enter attach");
    term.keys(&["s", "n"]);
    term.wait_for("the new-session form", "New session");
    // Agent: pi; down past its name to the model, then its first model.
    term.keys(&["Right", "Down", "Down", "Right"]);
    term.wait_for("pi's model", "‹ fake-model-1 ›  1M context");
}
