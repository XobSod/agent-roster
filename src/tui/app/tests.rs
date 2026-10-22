use super::*;
use crate::config::Keys;
use crate::model::tests::session;

fn project(id: i64, name: &str) -> Project {
    Project {
        id,
        name: name.into(),
        origin: format!("/{name}").into(),
        agent: None,
        model: None,
        effort: None,
    }
}

fn with(id: &str, project: (i64, &str), state: State, unread: bool, since: i64) -> Session {
    let mut s = session(id);
    s.project_id = project.0;
    s.project = project.1.into();
    s.state = state;
    s.unread = unread;
    s.state_since = Some(since);
    s
}

/// alpha: an idle session; beta: an unread one and a working one; one deleted.
fn app() -> App {
    let agents = vec![("claude".to_string(), true)];
    let mut app = App::new(
        Keymap::new(&Keys::default()).unwrap(),
        "/work".into(),
        agents,
    );
    let mut gone = with("s3", (1, "alpha"), State::Stopped, false, 1);
    gone.deleted_at = Some(5);
    app.set_data(
        vec![project(1, "alpha"), project(2, "beta")],
        vec![
            with("s1", (1, "alpha"), State::Idle, false, 10),
            with("s2", (2, "beta"), State::Idle, true, 20),
            with("s4", (2, "beta"), State::Working, false, 30),
            gone,
        ],
    );
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn chr(c: char) -> KeyEvent {
    let shift = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    KeyEvent::new(KeyCode::Char(c), shift)
}

fn keys(app: &mut App, keys: &str) -> Option<Op> {
    let mut op = None;
    for c in keys.chars() {
        op = app.on_key(chr(c));
    }
    op
}

fn select(app: &mut App, id: &str) {
    app.selected = app
        .entries
        .iter()
        .position(|e| e.key() == EntryKey::Session(id.into()))
        .unwrap();
}

fn shown(app: &App) -> Vec<EntryKey> {
    app.entries.iter().map(Entry::key).collect()
}

#[test]
fn starts_on_shelves_with_archived_folded() {
    let app = app();
    assert_eq!(
        shown(&app),
        [
            EntryKey::Group(Group::Shelf(Shelf::NeedsInput)),
            EntryKey::Session("s2".into()),
            EntryKey::Group(Group::Shelf(Shelf::Idle)),
            EntryKey::Session("s1".into()),
            EntryKey::Group(Group::Shelf(Shelf::Working)),
            EntryKey::Session("s4".into()),
            EntryKey::Group(Group::Shelf(Shelf::Archived)),
        ]
    );
}

#[test]
fn enter_attaches_and_h_l_fold() {
    let mut app = app();
    // The first session is selected at first, not the heading above it.
    assert_eq!(app.selected_session().unwrap().id, "s2");
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Op::Attach("s2".into()))
    );
    // h on a session folds its shelf and selects the heading; l unfolds.
    keys(&mut app, "h");
    assert_eq!(
        app.selected_key(),
        Some(EntryKey::Group(Group::Shelf(Shelf::NeedsInput)))
    );
    assert!(!shown(&app).contains(&EntryKey::Session("s2".into())));
    keys(&mut app, "l");
    assert!(shown(&app).contains(&EntryKey::Session("s2".into())));
    // Enter on a heading folds or unfolds it too.
    app.on_key(key(KeyCode::Enter));
    assert!(!shown(&app).contains(&EntryKey::Session("s2".into())));
}

#[test]
fn the_list_wraps_around() {
    let mut app = app();
    assert_eq!(app.selected, 1);
    keys(&mut app, "k");
    assert_eq!(app.selected, 0);
    keys(&mut app, "k");
    assert_eq!(app.selected, app.entries.len() - 1);
    keys(&mut app, "j");
    assert_eq!(app.selected, 0);
    // The projects view too.
    keys(&mut app, "pl");
    app.project_row = 0;
    keys(&mut app, "k");
    assert_eq!(app.project_row, app.projects.len() - 1);
    keys(&mut app, "j");
    assert_eq!(app.project_row, 0);
}

#[test]
fn back_from_a_session_given_a_task_the_cursor_keeps_its_row() {
    let mut app = app();
    // s2 needs input; answered, it goes to work, off its shelf.
    select(&mut app, "s2");
    let row = app.selected;
    app.attaching("s2");
    app.detached(true);
    let mut sessions = app.sessions.clone();
    let s2 = sessions.iter_mut().find(|s| s.id == "s2").unwrap();
    (s2.state, s2.unread, s2.state_since) = (State::Working, false, Some(50));
    app.set_data(app.projects.clone(), sessions);
    // Its row now holds the next one waiting.
    assert_eq!(app.selected_session().unwrap().id, "s1");
    assert_eq!(app.selected, row);
    // Later moves are followed as before.
    let mut sessions = app.sessions.clone();
    sessions.iter_mut().find(|s| s.id == "s1").unwrap().unread = true;
    app.set_data(app.projects.clone(), sessions);
    assert_eq!(app.selected_session().unwrap().id, "s1");
}

#[test]
fn back_from_only_reading_the_cursor_stays_on_the_session() {
    let mut app = app();
    // s2's finished turn, read without a prompt: read, it moves to Idle.
    select(&mut app, "s2");
    app.attaching("s2");
    app.detached(false);
    let mut sessions = app.sessions.clone();
    sessions.iter_mut().find(|s| s.id == "s2").unwrap().unread = false;
    app.set_data(app.projects.clone(), sessions);
    assert_eq!(app.selected_session().unwrap().id, "s2");
    // Another one landing above it meanwhile changes nothing either.
    select(&mut app, "s1");
    app.attaching("s1");
    app.detached(false);
    let mut sessions = app.sessions.clone();
    let s4 = sessions.iter_mut().find(|s| s.id == "s4").unwrap();
    (s4.state, s4.unread, s4.state_since) = (State::NeedsInput, true, Some(60));
    app.set_data(app.projects.clone(), sessions);
    assert_eq!(app.selected_session().unwrap().id, "s1");
}

#[test]
fn selection_follows_a_session_to_its_new_shelf() {
    let mut app = app();
    select(&mut app, "s4");
    // s4 finishes its turn: it moves to "Needs input" and stays selected,
    // after s2, which has waited longer.
    let mut sessions = app.sessions.clone();
    let s4 = sessions.iter_mut().find(|s| s.id == "s4").unwrap();
    s4.state = State::Idle;
    s4.unread = true;
    s4.state_since = Some(40);
    app.set_data(app.projects.clone(), sessions);
    assert_eq!(app.selected_session().unwrap().id, "s4");
    assert_eq!(app.selected, 2);
}

#[test]
fn a_session_moving_into_a_folded_shelf_selects_that_shelf() {
    let mut app = app();
    select(&mut app, "s1");
    // Archived: onto the folded Archived shelf.
    let mut sessions = app.sessions.clone();
    let s1 = sessions.iter_mut().find(|s| s.id == "s1").unwrap();
    s1.state = State::Stopped;
    s1.deleted_at = Some(50);
    app.set_data(app.projects.clone(), sessions);
    assert_eq!(
        app.selected_key(),
        Some(EntryKey::Group(Group::Shelf(Shelf::Archived)))
    );
    // Newest first.
    keys(&mut app, "lj");
    assert_eq!(app.selected_session().unwrap().id, "s1");
}

#[test]
fn menus_open_with_their_key_and_reuse_letters() {
    let mut app = app();
    select(&mut app, "s1");
    assert_eq!(keys(&mut app, "s"), None);
    assert_eq!(app.menu, Some(Menu::Session));
    assert_eq!(keys(&mut app, "x"), Some(Op::Stop("s1".into())));
    assert_eq!(app.menu, None);
    assert_eq!(keys(&mut app, "su"), Some(Op::SetUnread("s1".into(), true)));
    // An unbound key closes the menu without doing anything.
    assert_eq!(keys(&mut app, "sz"), None);
    assert_eq!((app.menu, app.dialog.is_none()), (None, true));
    // s n opens the new-session form on the selected session's project.
    keys(&mut app, "sn");
    let Some(Dialog::Form(form)) = &app.dialog else {
        panic!("no form")
    };
    assert_eq!(form.title, "New session");
    assert!(form.fields.iter().any(|f| f.value == "alpha"));
    app.on_key(key(KeyCode::Esc));
    assert!(app.dialog.is_none());
    // p n opens the new-project form; s e and p e the edit forms.
    keys(&mut app, "pn");
    assert!(matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "New project"));
    app.on_key(key(KeyCode::Esc));
    keys(&mut app, "se");
    assert!(matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "Edit session alpha/s-s1"));
    app.on_key(key(KeyCode::Esc));
    keys(&mut app, "pe");
    assert!(matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "Edit project alpha"));
}

#[test]
fn archive_and_working_restart_ask_first() {
    let mut app = app();
    select(&mut app, "s1");
    assert_eq!(keys(&mut app, "sd"), None);
    assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
    assert_eq!(keys(&mut app, "y"), Some(Op::Archive("s1".into())));
    // Only archived sessions can be deleted for good.
    assert_eq!(keys(&mut app, "sD"), None);
    assert!(app.dialog.is_none() && app.message().unwrap().error);
    assert_eq!(keys(&mut app, "sr"), Some(Op::Restart("s1".into())));
    select(&mut app, "s4");
    assert_eq!(keys(&mut app, "sr"), None);
    assert_eq!(keys(&mut app, "y"), Some(Op::Restart("s4".into())));
}

#[test]
fn archived_sessions_restore_or_go_for_good() {
    let mut app = app();
    // Grouped by project, the archived ones still come last, on their shelf.
    keys(&mut app, "vg");
    assert!(app.grouped);
    assert_eq!(shown(&app)[0], EntryKey::Group(Group::Project(1)));
    let archived = EntryKey::Group(Group::Shelf(Shelf::Archived));
    assert_eq!(shown(&app).last(), Some(&archived));
    app.selected = app.entries.len() - 1;
    keys(&mut app, "l");
    select(&mut app, "s3");
    let restore = Some(Op::Restore {
        id: "s3".into(),
        name: None,
    });
    assert_eq!(app.hints()[0], ("Enter".to_string(), "restore"));
    assert_eq!(app.on_key(key(KeyCode::Enter)), restore);
    assert_eq!(keys(&mut app, "sd"), restore);
    // Nothing else works on it.
    keys(&mut app, "sx");
    assert!(app.message().unwrap().error);
    // Deleting for good asks first.
    assert_eq!(keys(&mut app, "sD"), None);
    let Some(Dialog::Confirm { question, .. }) = &app.dialog else {
        panic!("no confirmation")
    };
    assert!(question.contains("for good"), "{question}");
    assert_eq!(keys(&mut app, "y"), Some(Op::Delete("s3".into())));
}

#[test]
fn the_projects_view_lists_and_manages_projects() {
    let mut app = app();
    select(&mut app, "s4");
    // p l opens it on the selection's project.
    keys(&mut app, "pl");
    assert_eq!((app.mode, app.project_row), (Mode::Projects, 1));
    assert_eq!(app.hints()[0], ("Enter".to_string(), "show its sessions"));
    assert_eq!(app.hints()[1], ("n".to_string(), "new"));
    keys(&mut app, "k");
    assert_eq!(app.project_row, 0);
    // The project menu acts on the row, without opening it too.
    for chord in ["pe", "e"] {
        keys(&mut app, chord);
        assert!(
            matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "Edit project alpha"),
            "{chord}"
        );
        app.on_key(key(KeyCode::Esc));
    }
    keys(&mut app, "n");
    assert!(matches!(&app.dialog, Some(Dialog::Form(f)) if f.title == "New project"));
    app.on_key(key(KeyCode::Esc));
    // alpha has a live session.
    keys(&mut app, "d");
    assert!(app.dialog.is_none() && app.message().unwrap().error);
    // Session actions don't work here.
    keys(&mut app, "sx");
    assert!(app.message().unwrap().error);
    assert_eq!(app.mode, Mode::Projects);
    // Enter goes back to the sessions, only that project's.
    keys(&mut app, "j");
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert_eq!((app.mode, app.filter), (Mode::Live, Some(2)));
    // v p opens it too, and closes it again.
    keys(&mut app, "vp");
    assert_eq!(app.mode, Mode::Projects);
    keys(&mut app, "vp");
    assert_eq!(app.mode, Mode::Live);
    // A new (or renamed) project is selected once it shows up.
    keys(&mut app, "pl");
    app.select_project("gamma");
    let mut projects = app.projects.clone();
    projects.push(project(3, "gamma"));
    app.set_data(projects, app.sessions.clone());
    assert_eq!(app.project_row, 2);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Live);
    // On the session list, the same keys don't act on projects.
    assert_eq!(keys(&mut app, "e"), None);
    assert!(app.dialog.is_none());
}

#[test]
fn a_chat_opens_the_new_session_form_on_the_chats_project() {
    let mut app = app();
    assert_eq!(keys(&mut app, "sc"), Some(Op::NewChat));
    // The run loop makes sure the project exists, then:
    let chats = project(3, "chats");
    let mut projects = app.projects.clone();
    projects.push(chats.clone());
    app.open_chat_form(&projects, &chats);
    let Some(Dialog::Form(form)) = &app.dialog else {
        panic!("no form")
    };
    assert_eq!(form.title, "New chat");
    assert!(form.fields.iter().any(|f| f.value == "chats"));
    // Straight to the prompt.
    assert_eq!(form.fields[form.focus].label, "Prompt");
    app.on_key(key(KeyCode::Esc));

    // Its directory can't be edited here.
    app.set_data(projects, app.sessions.clone());
    app.chat_project = Some(3);
    keys(&mut app, "pl");
    app.project_row = 2;
    keys(&mut app, "e");
    let Some(Dialog::Form(form)) = &app.dialog else {
        panic!("no form")
    };
    assert_eq!(form.title, "Edit project chats (built-in, for chats)");
    assert!(
        !form
            .fields
            .iter()
            .any(|f| f.label == "Directory" && f.hint() == "Tab completes")
    );
    assert!(
        app.project_info(&project(3, "chats")).lines[0]
            .to_string()
            .starts_with("built-in")
    );
}

#[test]
fn project_filter_and_removal() {
    let mut app = app();
    select(&mut app, "s4");
    keys(&mut app, "pf");
    assert_eq!(app.filter, Some(2));
    assert!(!shown(&app).contains(&EntryKey::Session("s1".into())));
    keys(&mut app, "pf");
    assert_eq!(app.filter, None);
    // beta still has sessions.
    keys(&mut app, "pd");
    assert!(app.dialog.is_none() && app.message().unwrap().error);
}

#[test]
fn agents_that_cant_resume_ask_before_a_new_conversation() {
    let mut app = app();
    app.agents.push(("aider".into(), false));
    let mut generic = with("g1", (1, "alpha"), State::Stopped, false, 1);
    generic.agent = "aider".into();
    let mut sessions = app.sessions.clone();
    sessions.push(generic);
    app.folded.clear();
    app.set_data(app.projects.clone(), sessions);
    select(&mut app, "g1");
    for (chord, op) in [
        ("ss", Op::Start("g1".into())),
        ("sr", Op::Restart("g1".into())),
    ] {
        assert_eq!(keys(&mut app, chord), None);
        let Some(Dialog::Confirm { question, .. }) = &app.dialog else {
            panic!("no confirmation")
        };
        assert!(question.contains("can't resume"), "{question}");
        assert_eq!(keys(&mut app, "y"), Some(op));
    }
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert_eq!(keys(&mut app, "y"), Some(Op::Attach("g1".into())));
}

#[test]
fn server_view_scrolls_kills_orphans_and_goes_back() {
    use crate::doctor::{Diagnosis, Level, Line};
    let mut app = app();
    select(&mut app, "s1");
    keys(&mut app, "vt");
    assert_eq!(app.mode, Mode::Server);
    let line = |text: &str| Line {
        level: Level::Info,
        text: text.into(),
    };
    app.set_report(Diagnosis {
        lines: vec![line("a"), line("b")],
        orphans: vec!["r-x".into()],
        problems: 1,
    });
    keys(&mut app, "jjj");
    assert_eq!(app.report_scroll, 1);
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
    assert_eq!(keys(&mut app, "y"), Some(Op::KillOrphans));
    keys(&mut app, "sx");
    assert!(app.message().unwrap().error);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Live);
    // The selection is where it was.
    assert_eq!(app.selected_session().unwrap().id, "s1");
}

#[test]
fn adopting_picks_a_conversation_then_fills_in_a_form() {
    use crate::tui::picker::tests::adoptable;
    let mut app = app();
    assert_eq!(keys(&mut app, "sa"), Some(Op::FindConversations));
    let mut open = adoptable("c2", "Busy elsewhere", Some("alpha"));
    open.conversation.open_in = Some(42);
    let mut deleted = adoptable("c3", "Old one", Some("alpha"));
    deleted.archived = Some(with("s3", (1, "alpha"), State::Stopped, false, 1));
    app.pick_conversation(vec![adoptable("c1", "Fix the parser", None), open, deleted]);
    assert!(matches!(&app.dialog, Some(Dialog::Picker(p)) if !p.all));

    // The one in a project: its project's form; submitted, it asks first.
    app.on_key(key(KeyCode::Enter));
    let Some(Dialog::Form(form)) = &app.dialog else {
        panic!("no form")
    };
    assert_eq!(form.title, "Adopt a conversation");
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    let Some(Dialog::Confirm { question, op }) = &app.dialog else {
        panic!("no confirmation")
    };
    assert!(question.contains("open in Claude (pid 42)"), "{question}");
    assert!(matches!(
        op,
        Op::Adopt {
            force: true,
            fork: false,
            ..
        }
    ));
    app.on_key(key(KeyCode::Esc));

    // Outside the projects (Tab): a new project, named after its directory.
    app.pick_conversation(vec![adoptable("c1", "Fix the parser", None)]);
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        None,
        "the form opens first"
    );
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Op::Adopt {
            conversation: "c1".into(),
            project: "elsewhere".into(),
            name: None,
            model: None,
            effort: None,
            fork: false,
            force: false,
        })
    );

    // A deleted session's: restore it instead.
    let mut deleted = adoptable("c3", "Old one", Some("alpha"));
    deleted.archived = Some(with("s3", (1, "alpha"), State::Stopped, false, 1));
    app.pick_conversation(vec![deleted]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        keys(&mut app, "y"),
        Some(Op::Restore {
            id: "s3".into(),
            name: None
        })
    );

    // Its name taken meanwhile: restoring it asks for another.
    let mut taken = with("s5", (1, "alpha"), State::Idle, false, 1);
    taken.name = "s-s3".into();
    let mut sessions = app.sessions.clone();
    sessions.push(taken);
    app.set_data(app.projects.clone(), sessions);
    let mut deleted = adoptable("c3", "Old one", Some("alpha"));
    deleted.archived = Some(with("s3", (1, "alpha"), State::Stopped, false, 1));
    app.pick_conversation(vec![deleted]);
    assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    assert!(matches!(&app.dialog, Some(Dialog::Input { prompt, .. })
        if prompt.contains("already has a session named s-s3")));
    app.on_key(key(KeyCode::Esc));

    app.pick_conversation(Vec::new());
    assert!(app.dialog.is_none() && !app.message().unwrap().error);
}

#[test]
fn help_opens_and_any_key_closes_it() {
    let mut app = app();
    keys(&mut app, "?");
    assert!(app.help);
    assert_eq!(keys(&mut app, "q"), None);
    assert!(!app.help);
    assert_eq!(keys(&mut app, "q"), Some(Op::Quit));
}

#[test]
fn quit_only_quits_from_the_session_list() {
    let mut app = app();
    for view in ["pl", "vt"] {
        keys(&mut app, view);
        assert_ne!(app.mode, Mode::Live);
        assert!(!app.hints().iter().any(|(_, what)| *what == "quit"));
        assert_eq!(keys(&mut app, "q"), None, "{view}");
        assert_eq!(app.mode, Mode::Live, "{view}");
    }
    assert!(app.hints().contains(&("q".to_string(), "quit")));
    assert_eq!(keys(&mut app, "q"), Some(Op::Quit));
}

#[test]
fn hints_follow_the_keymap() {
    let app = app();
    let hints = app.hints();
    assert_eq!(hints[0], ("Enter".to_string(), "attach"));
    assert!(hints.contains(&("s".to_string(), "session")));
    assert!(hints.contains(&("?".to_string(), "help")));
}

/// `app()` with titles: s1 "Tree depot", s2 "Auth rework", s4 "Report",
/// archived s3 "Old idea".
fn titled() -> App {
    let mut app = app();
    let mut sessions = app.sessions.clone();
    for (s, title) in sessions
        .iter_mut()
        .zip(["Tree depot", "Auth rework", "Report", "Old idea"])
    {
        s.title = Some(title.into());
    }
    app.set_data(app.projects.clone(), sessions);
    app
}

fn session_key(id: &str) -> EntryKey {
    EntryKey::Session(id.into())
}

#[test]
fn slash_filters_as_you_type_and_enter_keeps_it() {
    let mut app = titled();
    keys(&mut app, "/");
    assert_eq!(app.hints()[0], ("Enter".to_string(), "keep"));
    // Keys go to the filter, `q` too; the cursor goes to the best match,
    // not the first.
    assert_eq!(keys(&mut app, "rep"), None);
    assert_eq!(
        shown(&app),
        [
            EntryKey::Group(Group::Shelf(Shelf::Idle)),
            session_key("s1"),
            EntryKey::Group(Group::Shelf(Shelf::Working)),
            session_key("s4"),
        ]
    );
    assert_eq!(app.selected_key(), Some(session_key("s4")));
    app.on_key(key(KeyCode::Up));
    assert_eq!(
        app.selected_key(),
        Some(EntryKey::Group(Group::Shelf(Shelf::Working)))
    );
    app.on_key(key(KeyCode::Enter));
    assert!(app.typing.is_none());
    // Kept: the list stays filtered, and keys are keys again.
    keys(&mut app, "j");
    assert_eq!(app.selected_key(), Some(session_key("s4")));
    assert!(app.hints().contains(&("Esc".to_string(), "clear filter")));
    // Esc clears it.
    app.on_key(key(KeyCode::Esc));
    assert_eq!((app.search.as_str(), app.hits.is_none()), ("", true));
    assert!(shown(&app).contains(&session_key("s2")));
    assert_eq!(app.selected_key(), Some(session_key("s4")));
}

#[test]
fn esc_while_typing_puts_back_the_filter_and_the_cursor() {
    let mut app = titled();
    select(&mut app, "s2");
    keys(&mut app, "/auth");
    app.on_key(key(KeyCode::Enter));
    keys(&mut app, "/q");
    assert_eq!(app.search, "authq");
    assert!(app.hits.as_ref().unwrap().is_empty());
    assert!(app.entries.is_empty());
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.search, "auth");
    assert_eq!(
        shown(&app),
        [
            EntryKey::Group(Group::Shelf(Shelf::NeedsInput)),
            session_key("s2")
        ]
    );
    assert_eq!(app.selected_key(), Some(session_key("s2")));
    // Blank, Enter turns it off.
    keys(&mut app, "/");
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    keys(&mut app, " ");
    app.on_key(key(KeyCode::Enter));
    assert_eq!((app.search.as_str(), app.hits.is_none()), ("", true));
}

#[test]
fn filtering_opens_every_group_and_folds_its_own() {
    let mut app = titled();
    assert!(!shown(&app).contains(&session_key("s3")));
    keys(&mut app, "/old");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.selected_key(), Some(session_key("s3")));
    keys(&mut app, "h");
    assert_eq!(
        shown(&app),
        [EntryKey::Group(Group::Shelf(Shelf::Archived))]
    );
    // The list's own folds are as they were, and the next filter starts open.
    app.on_key(key(KeyCode::Esc));
    keys(&mut app, "l");
    assert!(shown(&app).contains(&session_key("s3")));
    keys(&mut app, "h/old");
    assert!(shown(&app).contains(&session_key("s3")));
}

#[test]
fn errors_stay_until_a_key_is_pressed() {
    let mut app = app();
    app.flash("Started");
    keys(&mut app, "j");
    assert_eq!(app.message().map(|m| m.text.as_str()), Some("Started"));
    app.fail("Broken");
    keys(&mut app, "j");
    assert!(app.message().is_none());
}
