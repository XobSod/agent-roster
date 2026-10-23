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
    // Its preview says what it was, as for an archived one, not its last screen.
    term.wait_for("the stopped session's details", "s s starts it");
    assert!(
        !term.screen().contains("Its last screen"),
        "{}",
        term.screen()
    );
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

#[test]
fn settings_are_changed_in_the_tui_and_followed_on_disk() {
    let Some(t) = TestEnv::new("settings") else {
        return;
    };
    let file = t.config_dir().join("config.toml");
    let term = Terminal::start(&t, &t.shell_command(), (120, 40));
    term.wait_for("the list", "Enter attach");
    term.keys(&["v", "s"]);
    term.wait_for("the settings", "roster  settings");
    term.wait_for("a setting's doc", "while attached, a line at the top");
    // The second setting is the line at the top: Enter switches it off.
    term.keys(&["j", "Enter"]);
    term.wait_for("the change", "Set status_line");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("status_line = false"), "{text}");

    // Changed on disk (an editor, a git pull): it's read again and used.
    std::fs::write(
        &file,
        text.replace("status_line = false", "fit_preview = \"never\""),
    )
    .unwrap();
    term.wait_for("the reload", "config.toml changed: its settings are in use");
    // Broken: the settings in use stay, and it says why, once.
    std::fs::write(&file, "fit_preview = \"sometimes\"\n").unwrap();
    term.wait_for(
        "the refusal",
        "config.toml has an error, the settings in use stay",
    );
}

#[test]
fn notes_are_written_pinned_and_shown_in_the_tui() {
    let Some(t) = TestEnv::new("notes") else {
        return;
    };
    // An $EDITOR that ticks the second item, for Ctrl+G.
    let editor = t.path("editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nsed -i 's/second/second, done/' \"$1\"\n",
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&editor)
        .status()
        .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "one"]);
    t.ok(&["config", "set", "notes.escape", "jk"]);
    let command = format!(
        "EDITOR='{}' VISUAL= {}; sleep 600",
        editor.display(),
        t.shell_command()
    );
    let term = Terminal::start(&t, &command, (120, 30));
    term.wait_for("the agent's screen in the preview", "fake agent:");

    // Written in the editor under the header, the agent's screen still below.
    term.keys(&["s", "o"]);
    term.wait_for("the editor", "╭ Note ");
    term.keys(&["i"]);
    term.wait_for("insert mode", " INSERT ");
    term.text("TODO");
    term.keys(&["Enter"]);
    term.text("- [ ] first");
    term.keys(&["Enter"]);
    // j and k typed slowly are text; quickly (notes.escape), they leave
    // insert mode, typing nothing.
    term.text("- [ ] second j");
    std::thread::sleep(std::time::Duration::from_millis(500));
    term.text("k");
    term.wait_for("j and k, typed slowly", "- [ ] second jk");
    term.keys(&["BSpace", "BSpace", "BSpace", "j", "k"]);
    term.wait_for("normal mode", " NORMAL ");
    term.keys(&["Escape", "Escape"]);
    assert!(
        term.screen().contains("│  - [ ] second "),
        "{}",
        term.screen()
    );
    term.wait_for("normal mode, still open", " NORMAL ");
    assert!(term.screen().contains("fake agent:"), "{}", term.screen());
    term.wait_for("unsaved changes", "Note [+]");
    // Pinned on its first item, saved and closed.
    term.keys(&["k", "m", "m"]);
    term.wait_for("the pin", "▸ - [ ] first");
    term.keys(&[":", "w", "q", "Enter"]);
    term.wait_for("the save", "Saved the note of demo/one");
    term.wait_for_gone("the editor closed", "╭ Note ");
    term.wait_for("the pinned lines above the screen", "✎ - [ ] first");
    term.wait_for("where they are in the note", "2–3 of 3");
    assert_eq!(t.ok(&["note", "one"]), "TODO\n- [ ] first\n- [ ] second\n");
    assert_eq!(t.session("one", false).unwrap()["note_pin"], 1);

    // Saved by another meanwhile (an agent, through `roster note`): :w says
    // so and saves nothing; :w! saves yours over it.
    term.keys(&["s", "o"]);
    term.wait_for("the editor again", "╭ Note ");
    t.ok(&["note", "one", "--append", "from an agent"]);
    term.keys(&["m", "m", ":", "w", "Enter"]);
    term.wait_for("the refusal", "changed since you opened it");
    assert!(t.ok(&["note", "one"]).contains("from an agent"));
    term.keys(&[":", "w", "!", "Enter"]);
    term.wait_for("the forced save", "Saved the note of demo/one");
    assert!(!t.ok(&["note", "one"]).contains("from an agent"));
    // Saved now, it's what the next save replaces: no refusal.
    term.keys(&["m", "m", ":", "w", "q", "Enter"]);
    term.wait_for_gone("the editor closed", "╭ Note ");
    assert_eq!(t.session("one", false).unwrap()["note_pin"], 1);

    // Ctrl+G: to $EDITOR and back into the editor, unsaved until :w.
    term.keys(&["s", "o", "C-g"]);
    term.wait_for("the text back from $EDITOR", "second, done");
    term.wait_for("unsaved", "Note [+]");
    term.keys(&[":", "x", "Enter"]);
    term.wait_for_gone("the editor closed", "╭ Note ");
    assert_eq!(
        t.ok(&["note", "one"]),
        "TODO\n- [ ] first\n- [ ] second, done\n"
    );

    // With notes.editor = "external", s o is $EDITOR, saved when it exits.
    t.ok(&["config", "set", "notes.editor", "external"]);
    term.wait_for("the setting in use", "config.toml changed");
    term.keys(&["s", "o"]);
    term.wait_for("the save", "Saved the note of demo/one");
    assert_eq!(
        t.ok(&["note", "one"]),
        "TODO\n- [ ] first\n- [ ] second, done, done\n"
    );
    assert!(!term.screen().contains("╭ Note "));
    assert_eq!(t.session("one", false).unwrap()["note_pin"], 1);
}

#[test]
fn a_stopped_agents_error_shows_in_its_preview() {
    let Some(t) = TestEnv::new("stoperr") else {
        return;
    };
    std::fs::write(t.path("proj/.agent-roster.env"), "TOKEN=\"$(false)\"\n").unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "bad"]);
    t.wait_for("stopped", |t| {
        t.session("bad", false).unwrap()["state"] == "stopped"
    });
    let term = Terminal::start(&t, &format!("{}; sleep 600", t.shell_command()), (120, 30));
    term.wait_for("what it was", "Stopped just now: its agent exited with 1.");
    term.wait_for("how its screen ended", "Its last screen");
    term.wait_for("the error", "roster: preparing the environment failed");
    assert!(!term.screen().contains("Pane is dead"), "{}", term.screen());
}

#[test]
fn a_first_prompt_is_typed_in_while_attached() {
    let Some(t) = TestEnv::new("attachprompt") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    let command = format!(
        "{} new demo --name a --attach --prompt 'first task'; sleep 600",
        t.shell_command()
    );
    let term = Terminal::start(&t, &command, (120, 30));
    term.wait_for("attached to the agent", "fake agent:");
    // (Its id now, while asking refreshes nothing it's ready for.)
    let id = t.session("a", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Ready once it reports its session, as Claude's hooks do; from then on
    // nothing but what `new` left running may refresh, or this proves nothing.
    let payload = format!(
        r#"{{"session_id":"x","transcript_path":"{}"}}"#,
        t.path("x.jsonl").display()
    );
    t.report(&id, "session", &payload);
    term.wait_for("the prompt, typed in while attached", "first task");
    assert!(term.screen().contains("C-q back"), "{}", term.screen());
}

#[test]
fn a_pending_first_prompt_is_typed_in_whichever_way_you_attach() {
    let Some(t) = TestEnv::new("attachpending") else {
        return;
    };
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    // A session whose first prompt is still waiting for its agent: `new`
    // gave up waiting (stopped here before the agent was ready).
    let mut waiting = t
        .command(&["new", "demo", "--name", "a", "--prompt", "pending task"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    t.wait_for("the session", |t| t.session("a", false).is_some());
    waiting.kill().unwrap();
    waiting.wait().unwrap();
    let id = t.session("a", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(t.session("a", false).unwrap()["pending_prompt"], true);
    // Attached with `roster attach`, then the agent is ready: typed in while
    // attached (nothing else here refreshes).
    let command = format!("{} attach a; sleep 600", t.shell_command());
    let term = Terminal::start(&t, &command, (120, 30));
    term.wait_for("attached to the agent", "C-q back");
    let payload = format!(
        r#"{{"session_id":"x","transcript_path":"{}"}}"#,
        t.path("x.jsonl").display()
    );
    t.report(&id, "session", &payload);
    term.wait_for("the prompt, typed in while attached", "pending task");
}

#[test]
fn a_title_is_made_while_the_tui_goes_on() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("asyncretitle") else {
        return;
    };
    // Claude's stand-in, making a title (`claude -p`) once the test lets it:
    // it says it started, then waits for its release.
    let slow = t.path("slow-claude");
    let dir = t.path("titles");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &slow,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = -p ]; then\n\
               case \"$*\" in *'first task'*) w=first ;; *'second task'*) w=second ;; *) w=third ;; esac\n\
               touch '{dir}/started-'$w\n\
               while [ ! -e '{dir}/release-'$w ]; do sleep 0.05; done\n\
               echo 'Slow title'; exit 0\n\
             fi\n\
             exec '{agent}' \"$@\"\n",
            dir = dir.display(),
            agent = t.path("fake-agent").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = t.config_dir().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "shell = \"/bin/sh\"\n[agents.claude]\ncommand = \"{}\"\n",
            slow.display()
        ),
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    // Three sessions with a prompt each, to title.
    let mut ids = std::collections::HashMap::new();
    for (name, task) in [
        ("one", "first task"),
        ("two", "second task"),
        ("three", "third task"),
    ] {
        t.ok(&["new", "demo", "--name", name]);
        let id = t.session(name, false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let transcript = t.path(&format!("{name}.jsonl"));
        std::fs::write(
            &transcript,
            format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{task}\"}}}}\n"),
        )
        .unwrap();
        let payload = format!(
            r#"{{"session_id":"{name}","transcript_path":"{}"}}"#,
            transcript.display()
        );
        t.report(&id, "session", &payload);
        t.report(&id, "idle", "{}");
        ids.insert(name, id);
    }
    let started = |w: &str| dir.join(format!("started-{w}")).exists();

    let term = Terminal::start(&t, &t.shell_command(), (120, 30));
    term.wait_for("the list", "Enter attach");
    term.keys(&["s", "t"]);
    term.wait_for(
        "the question asked",
        "Asking Claude for a title for demo/one",
    );
    term.wait_for("the header saying so", "making a title…");
    t.wait_for("the first title's run", |_| started("first"));
    // Meanwhile the TUI takes keys: a menu opens; the same session again is
    // already being asked; the other one is asked too, both runs going at once.
    term.keys(&["v"]);
    term.wait_for("the view menu", "group by project");
    term.keys(&["Escape", "s", "t"]);
    term.wait_for(
        "the same asked again",
        "Already asking for a title for demo/one",
    );
    term.keys(&["j", "s", "t"]);
    term.wait_for("the other asked", "Asking Claude for a title for demo/two");
    term.keys(&["j", "s", "t"]);
    term.wait_for(
        "the third asked",
        "Asking Claude for a title for demo/three",
    );
    t.wait_for("all three runs at once", |_| {
        ["first", "second", "third"].iter().all(|w| started(w))
    });
    assert!(!term.screen().contains("New title"), "{}", term.screen());

    // The first comes, and is kept.
    std::fs::write(dir.join("release-first"), "").unwrap();
    term.wait_for("the first title", "New title for demo/one: Slow title");
    assert_eq!(
        t.session("one", false).unwrap()["title_override"],
        "Slow title"
    );
    // The second's session got a title of yours meanwhile, which stays.
    t.ok(&["edit", "two", "--title", "Mine"]);
    std::fs::write(dir.join("release-second"), "").unwrap();
    term.wait_for(
        "yours kept",
        "demo/two's title changed meanwhile, so the new one isn't kept",
    );
    assert_eq!(t.session("two", false).unwrap()["title_override"], "Mine");
    // The third's moved to another conversation meanwhile (a /clear): the
    // title made of the one before isn't kept.
    let moved = format!(
        r#"{{"session_id":"three-later","transcript_path":"{}"}}"#,
        t.path("three.jsonl").display()
    );
    t.report(&ids["three"], "session", &moved);
    std::fs::write(dir.join("release-third"), "").unwrap();
    term.wait_for(
        "not kept for another conversation",
        "demo/three is on another conversation now, so the new title isn't kept",
    );
    assert_eq!(
        t.session("three", false).unwrap()["title_override"],
        serde_json::Value::Null
    );
    // Nothing left being made, for any of them.
    for up in [false, true, true] {
        if up {
            term.keys(&["k"]);
        }
        term.wait_for_gone("nothing left being made", "making a title…");
    }
}

#[test]
fn a_title_for_a_session_deleted_meanwhile_is_let_go() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = TestEnv::new("titlegone") else {
        return;
    };
    let slow = t.path("slow-claude");
    let dir = t.path("titles");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &slow,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = -p ]; then\n\
               touch '{dir}/started'\n\
               while [ ! -e '{dir}/release' ]; do sleep 0.05; done\n\
               echo 'Slow title'; touch '{dir}/answered'; exit 0\n\
             fi\n\
             exec '{agent}' \"$@\"\n",
            dir = dir.display(),
            agent = t.path("fake-agent").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        t.config_dir().join("config.toml"),
        format!(
            "shell = \"/bin/sh\"\n[agents.claude]\ncommand = \"{}\"\n",
            slow.display()
        ),
    )
    .unwrap();
    t.ok(&["project", "add", "demo", t.path("proj").to_str().unwrap()]);
    t.ok(&["new", "demo", "--name", "gone"]);
    let id = t.session("gone", false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let transcript = t.path("gone.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"content\":\"a task\"}}\n",
    )
    .unwrap();
    let payload = format!(
        r#"{{"session_id":"g","transcript_path":"{}"}}"#,
        transcript.display()
    );
    t.report(&id, "session", &payload);
    t.report(&id, "idle", "{}");

    let term = Terminal::start(&t, &t.shell_command(), (120, 30));
    term.wait_for("the list", "Enter attach");
    term.keys(&["s", "t"]);
    t.wait_for("the title's run", |_| dir.join("started").exists());
    // Archived and deleted for good while it's made: nothing to keep it on,
    // and nothing to say.
    t.ok(&["archive", "--yes", "gone"]);
    t.ok(&["rm", "--yes", "gone"]);
    std::fs::write(dir.join("release"), "").unwrap();
    t.wait_for("its answer", |_| dir.join("answered").exists());
    std::thread::sleep(std::time::Duration::from_millis(800));
    let screen = term.screen();
    // (Its "Asking…" may still show; no word about it after.)
    assert!(
        !screen.contains("demo/gone:")
            && !screen.contains("no session with id")
            && !screen.contains("isn't kept"),
        "{screen}"
    );
    // The TUI goes on.
    term.keys(&["v"]);
    term.wait_for("the view menu", "group by project");
}
