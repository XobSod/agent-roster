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

    // The session menu's form starts on the name; its screen shows in the preview.
    term.keys(&["s", "n"]);
    term.wait_for("the new-session form", "New session");
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

    // Attach, type to the agent, come back with Ctrl+Q.
    term.keys(&["Enter"]);
    term.wait_for_gone("the agent full screen", "Enter attach");
    term.text("typed while attached");
    term.keys(&["Enter", "C-q"]);
    term.wait_for("the list again", "Enter attach");
    term.wait_for("the agent's echo in the preview", "typed while attached");

    // Stop it: it moves to the folded Stopped shelf, which gets the selection.
    term.keys(&["s", "x"]);
    term.wait_for("the stop", "Stopped demo/one");
    term.wait_for("the Stopped shelf", "Stopped (1)");
    term.keys(&["l", "j"]);
    term.keys(&["s", "d"]);
    term.wait_for("the confirmation", "Delete demo/one?");
    term.keys(&["y"]);
    term.wait_for("the delete", "Deleted demo/one");

    // The view menu shows deleted sessions; Enter restores.
    term.keys(&["v", "d"]);
    term.wait_for("the deleted view", "deleted sessions");
    term.keys(&["Enter"]);
    term.wait_for("the restore", "Restored demo/one");

    // Help lists the menus; any key closes it.
    term.keys(&["?"]);
    term.wait_for("the help", "Session menu (s …)");
    term.keys(&["Escape"]);
    term.wait_for_gone("the help closed", "Session menu (s …)");

    term.keys(&["q"]);
    term.wait_for("the TUI to exit", "TUI-EXIT=0");
    assert_eq!(t.session("one", false).unwrap()["state"], "stopped");
}
