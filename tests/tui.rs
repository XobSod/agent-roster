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
