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

    // Add a project: its name, then its directory (prefilled with the cwd).
    term.keys(&["P"]);
    term.text("demo");
    term.keys(&["Enter"]);
    term.wait_for("the directory prompt", "Origin directory of demo");
    term.keys(&["C-u"]);
    term.text(t.path("proj").to_str().unwrap());
    term.keys(&["Enter"]);
    term.wait_for("the project", "Added project demo");

    // A new session; its screen shows in the preview.
    term.keys(&["n"]);
    term.text("one");
    term.keys(&["Enter"]);
    term.wait_for("the agent's screen in the preview", "fake agent:");

    // Attach, type to the agent, come back with Ctrl+Q.
    term.keys(&["Enter"]);
    term.wait_for_gone("the agent full screen", "Enter attach");
    term.text("typed while attached");
    term.keys(&["Enter", "C-q"]);
    term.wait_for("the list again", "Enter attach");
    term.wait_for("the agent's echo in the preview", "typed while attached");

    // Stop doesn't ask; delete does; the deleted view restores.
    term.keys(&["d"]);
    term.wait_for("the stop", "Stopped demo/one");
    term.keys(&["D"]);
    term.wait_for("the confirmation", "Delete demo/one?");
    term.keys(&["y"]);
    term.wait_for("the delete", "Deleted demo/one");
    term.keys(&["X"]);
    term.wait_for("the deleted view", "deleted sessions");
    term.keys(&["Enter"]);
    term.wait_for("the restore", "Restored demo/one");

    term.keys(&["q"]);
    term.wait_for("the TUI to exit", "TUI-EXIT=0");
    assert_eq!(t.session("one", false).unwrap()["state"], "stopped");
}
