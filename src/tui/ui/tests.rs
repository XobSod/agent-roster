use super::*;
use crate::config::Keys;
use crate::keys::Keymap;
use crate::model::Project;
use crate::model::tests::session;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn render(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn app() -> App {
    let agents = vec!["claude".to_string()];
    let mut app = App::new(Keymap::new(&Keys::default()).unwrap(), "/".into(), agents);
    let now = now_ms();
    let mut a = session("a1");
    a.created_at = now - 2 * 86_400_000;
    a.name = "api".into();
    a.project = "roster".into();
    a.title = Some("Fix scrollback".into());
    a.state = State::NeedsInput;
    a.unread = true;
    a.model = Some("opus".into());
    a.effort = Some("high".into());
    a.state_since = Some(now - 120_000);
    let mut b = session("b1");
    b.created_at = now - 2 * 86_400_000;
    b.name = "db".into();
    b.project = "roster".into();
    b.state = State::Working;
    b.state_since = Some(now - 200_000);
    app.set_data(
        vec![Project {
            id: 1,
            name: "roster".into(),
            origin: "/r".into(),
            agent: None,
            model: None,
            effort: None,
        }],
        vec![a, b],
    );
    app.selected = 1;
    app.preview.text = Text::from("agent screen line");
    app
}

#[test]
fn shows_shelves_cards_header_and_hints() {
    let screen = render(&app(), 120, 16);
    assert!(screen.contains("1 needs input · 1 working"), "{screen}");
    assert!(screen.contains("Needs input (1)"), "{screen}");
    // Without project headings, each card names its project first.
    assert!(screen.contains("RO roster"), "{screen}");
    assert!(screen.contains("   Fix scrollback"), "{screen}");
    assert!(screen.contains("Input"), "{screen}");
    assert!(screen.contains("api · opus high"), "{screen}");
    assert!(screen.contains("Working (1)"), "{screen}");
    assert!(screen.contains("3m"), "{screen}");
    // The header: the name was chosen, so it's there, before the title.
    assert!(
        screen.contains("RO roster / api / Fix scrollback"),
        "{screen}"
    );
    assert!(screen.contains("needs input"), "{screen}");
    assert!(screen.contains("agent screen line"), "{screen}");
    assert!(screen.contains("Enter attach"), "{screen}");
}

#[test]
fn the_status_line_is_the_header_with_the_name() {
    let mut s = session("a1");
    s.project = "roster".into();
    s.name = "api".into();
    s.model = Some("opus".into());
    let (left, right) = status_line(&s, "C-q");
    let text = |markup: &str| -> String {
        // Without the markup: what shows.
        let mut out = String::new();
        let mut rest = markup;
        while let Some(i) = rest.find("#[") {
            out.push_str(&rest[..i]);
            rest = &rest[i + rest[i..].find(']').unwrap() + 1..];
        }
        out + rest
    };
    assert_eq!(text(&left), " RO roster / api");
    let right = text(&right);
    assert!(right.starts_with("✱ claude · opus"), "{right}");
    assert!(right.ends_with("   C-q back "), "{right}");
    // The title, yours over the agent's, in bold; a `#` shows as typed.
    s.title = Some("Agent's".into());
    s.title_override = Some("Fix #42".into());
    let (left, _) = status_line(&s, "C-q");
    assert_eq!(text(&left), " RO roster / api / Fix ##42");
    assert!(left.ends_with("#[fg=terminal,bold]Fix ##42"), "{left}");
    assert!(left.contains("#[fg=#7d8296,nobold]roster"), "{left}");
    // A made-up name says nothing: left out, unless there's no title yet.
    s.name = "claude-1".into();
    assert_eq!(text(&status_line(&s, "C-q").0), " RO roster / Fix ##42");
    s.title = None;
    s.title_override = None;
    assert_eq!(text(&status_line(&s, "C-q").0), " RO roster / claude-1");
}

#[test]
fn messages_sit_right_of_the_hints() {
    let mut app = app();
    app.flash("Started roster/api; Enter attaches");
    let screen = render(&app, 120, 16);
    let footer = screen.lines().last().unwrap();
    assert!(footer.starts_with(" Enter attach   s session"), "{footer}");
    assert!(
        footer.ends_with("   Started roster/api; Enter attaches "),
        "{footer}"
    );
    // Where they'd meet, the last hints give way, whole.
    let screen = render(&app, 70, 16);
    let footer = screen.lines().last().unwrap();
    assert!(
        footer.starts_with(" Enter attach   s session   "),
        "{footer}"
    );
    assert!(!footer.contains("help"), "{footer}");
    assert!(
        footer.ends_with(" Started roster/api; Enter attaches "),
        "{footer}"
    );
    // Longer than the line: only the message, cut.
    let screen = render(&app, 30, 16);
    let footer = screen.lines().last().unwrap();
    assert_eq!(footer, " Started roster/api; Enter a… ");
}

#[test]
fn the_text_filter_shows_under_the_title_and_marks_what_matched() {
    let mut app = app();
    for c in "/fix".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let mut terminal = Terminal::new(TestBackend::new(120, 16)).unwrap();
    terminal.draw(|f| draw(f, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let line = |y: u16| -> String { (0..40).map(|x| buffer[(x, y)].symbol()).collect() };
    assert!(line(1).starts_with(" / fix "), "{}", line(1));
    assert!(line(1).contains(" 1 of 2 │"), "{}", line(1));
    let y = (0..16)
        .find(|&y| line(y).contains("Fix scrollback"))
        .unwrap();
    let x = (0..40).find(|&x| buffer[(x, y)].symbol() == "F").unwrap();
    let lit = |x| buffer[(x, y)].modifier.contains(Modifier::UNDERLINED);
    assert!(lit(x) && lit(x + 1) && lit(x + 2) && !lit(x + 3));
    assert!(!render(&app, 120, 16).contains("Working"));
    app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(render(&app, 120, 16).contains("nothing matches"));
}

#[test]
fn grouped_cards_leave_the_project_to_its_heading() {
    let mut app = app();
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    let screen = render(&app, 120, 16);
    // The heading and the header; no project line in the cards.
    assert_eq!(screen.matches("RO roster").count(), 2, "{screen}");
    assert!(screen.contains("RO roster (2)"), "{screen}");
    assert!(screen.contains("   Fix scrollback"), "{screen}");
}

#[test]
fn stopped_rows_name_their_project_too() {
    let mut app = app();
    let mut sessions = app.sessions.clone();
    sessions[1].state = State::Stopped;
    app.set_data(app.projects.clone(), sessions);
    // Unfold the Stopped shelf.
    let stopped = app.entries.len() - 1;
    app.selected = stopped;
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    let screen = render(&app, 120, 16);
    assert!(screen.contains("RO roster / db"), "{screen}");
}

#[test]
fn working_and_archived_sit_at_the_bottom_while_all_fits() {
    let mut app = app();
    let mut sessions = app.sessions.clone();
    let mut archived = session("c1");
    archived.project = "roster".into();
    archived.title = Some("Old work".into());
    archived.deleted_at = Some(now_ms() - 3 * 86_400_000);
    sessions.push(archived);
    app.set_data(app.projects.clone(), sessions);
    let screen = render(&app, 120, 20);
    let lines: Vec<&str> = screen.lines().collect();
    let row = |text: &str| lines.iter().position(|l| l.contains(text)).unwrap();
    // The footer is the last line; Archived (folded) right above it.
    assert_eq!(row("Archived (1)"), lines.len() - 2, "{screen}");
    assert!(row("Working (1)") > row("api · opus high") + 2, "{screen}");
    // Unfolded: project / title, and when it was archived.
    app.selected = app.entries.len() - 1;
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    let screen = render(&app, 120, 20);
    assert!(screen.contains("RO roster / Old work"), "{screen}");
    assert!(screen.contains("archived 3d"), "{screen}");
    // Too long to fit: no gap, it scrolls (here: to the top).
    app.selected = 0;
    let screen = render(&app, 120, 10);
    assert!(screen.contains("Needs input (1)"), "{screen}");
    assert!(!screen.contains("Archived"), "{screen}");
}

#[test]
fn the_projects_view_shows_each_project() {
    let mut app = app();
    app.models.names = [("opus".to_string(), "Opus 5.5".to_string())].into();
    let mut projects = app.projects.clone();
    projects[0].model = Some("opus".into());
    projects.push(Project {
        id: 2,
        name: "scratch".into(),
        origin: "/tmp/scratch".into(),
        agent: None,
        model: None,
        effort: None,
    });
    app.set_data(projects, app.sessions.clone());
    app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    let screen = render(&app, 120, 20);
    assert!(screen.contains("roster  projects"), "{screen}");
    assert!(screen.contains("2 projects"), "{screen}");
    assert!(screen.contains("PROJECT"), "{screen}");
    assert!(screen.contains("RO roster"), "{screen}");
    assert!(screen.contains("2 live"), "{screen}");
    assert!(screen.contains("claude · opus (Opus 5.5)"), "{screen}");
    assert!(screen.contains("/tmp/scratch"), "{screen}");
    assert!(screen.contains("none"), "{screen}");
    // The selected one's details, and the keys that act here.
    assert!(screen.contains("origin    /r"), "{screen}");
    // Its project keys work without "p".
    assert!(
        screen.contains("Enter show its sessions   n new   e edit   d remove   s session"),
        "{screen}"
    );
    assert!(screen.contains("Esc back"), "{screen}");
}

#[test]
fn menu_popup_lists_its_keys() {
    let mut app = app();
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    let screen = render(&app, 120, 16);
    assert!(screen.contains(" session "), "{screen}");
    assert!(screen.contains("n new"), "{screen}");
    assert!(screen.contains("x stop"), "{screen}");
    assert!(screen.contains("d archive / restore"), "{screen}");
    assert!(screen.contains("D delete for good"), "{screen}");
}

#[test]
fn the_picker_shows_conversations_and_where_they_ran() {
    use crate::tui::picker::tests::adoptable;
    let mut app = app();
    let mut open = adoptable("c2", "Busy elsewhere", Some("alpha"));
    open.conversation.open_in = Some(42);
    open.conversation.last_prompt = Some("and the\ntests".into());
    app.pick_conversation(vec![open, adoptable("c1", "Fix the parser", None)]);
    let screen = render(&app, 120, 30);
    assert!(screen.contains("Adopt a conversation"), "{screen}");
    assert!(
        screen.contains("in your projects (1 more elsewhere)"),
        "{screen}"
    );
    assert!(screen.contains("Busy elsewhere"), "{screen}");
    assert!(screen.contains("open in Claude"), "{screen}");
    assert!(!screen.contains("Fix the parser"), "{screen}");
    assert!(screen.contains("/alpha · c2"), "{screen}");
    assert!(screen.contains("last prompt: and the tests"), "{screen}");
    assert!(
        screen.contains("Enter adopt · Tab: all · Esc cancel"),
        "{screen}"
    );
    // Tab: the rest, with the project each would get.
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let screen = render(&app, 120, 30);
    assert!(
        screen.contains("+elsewhere") && screen.contains("Fix the parser"),
        "{screen}"
    );
    // Picked: the form, with what moving it means.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let screen = render(&app, 120, 30);
    assert!(screen.contains("Adopting   Busy elsewhere"), "{screen}");
    assert!(
        screen.contains("it's open in Claude (pid 42), close it there"),
        "{screen}"
    );
    assert!(screen.contains("Enter adopt"), "{screen}");
}

#[test]
fn long_questions_wrap_and_keep_their_keys() {
    assert_eq!(wrapped_lines("one two three", 9), 2);
    assert_eq!(wrapped_lines("one two", 7), 1);
    // A word wider than the line is broken over several.
    assert_eq!(
        wrapped_lines("Archive some-project/a-long-session-name?", 10),
        5
    );
    let mut app = app();
    app.dialog = Some(Dialog::Confirm {
        question: "word ".repeat(40),
        op: super::super::op::Op::Quit,
    });
    let screen = render(&app, 80, 30);
    assert!(screen.contains("y yes    n no"), "{screen}");
}

#[test]
fn help_lists_every_key() {
    let mut app = app();
    app.help = true;
    // Tall enough: one column. Not: two, with nothing cut off.
    for height in [60, 30] {
        let screen = render(&app, 120, height);
        assert!(screen.contains("Session menu (s …)"), "{screen}");
        assert!(screen.contains("s n  new"), "{screen}");
        assert!(screen.contains("v p  projects"), "{screen}");
        assert!(screen.contains("p l  list"), "{screen}");
        assert!(
            screen.contains("in the projects view, without p"),
            "{screen}"
        );
        assert!(screen.contains("C-q  back to roster"), "{screen}");
    }
}

#[test]
fn forms_show_fields_choices_and_hints() {
    let mut app = app();
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let screen = render(&app, 120, 24);
    assert!(screen.contains(" New session "), "{screen}");
    assert!(screen.contains("Project    roster"), "{screen}");
    assert!(screen.contains("▸ Agent      ‹ claude ›"), "{screen}");
    assert!(screen.contains("Agent name automatic"), "{screen}");
    assert!(screen.contains("Effort     default"), "{screen}");
    assert!(
        screen.contains("optional: typed in once the agent is ready"),
        "{screen}"
    );
    assert!(screen.contains("Enter start · Esc cancel"), "{screen}");
    assert!(screen.contains("←→ change"), "{screen}");
    // Then the agent's name.
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let screen = render(&app, 120, 24);
    assert!(screen.contains("▸ Agent name automatic"), "{screen}");
    assert_eq!(tail("/a/very/long/path", 8), "…ng/path");
}

#[test]
fn server_view_shows_the_report() {
    use crate::doctor::{Diagnosis, Level, Line};
    let mut app = app();
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
    let line = |level, text: &str| Line {
        level,
        text: text.into(),
    };
    app.set_report(Diagnosis {
        lines: vec![
            line(Level::Heading, "tmux sessions (1)"),
            line(Level::Problem, "  r-x  -  alive  ← orphan"),
        ],
        orphans: vec!["r-x".into()],
        problems: 1,
    });
    let screen = render(&app, 120, 12);
    assert!(screen.contains("tmux server and database"), "{screen}");
    assert!(screen.contains("tmux sessions (1)"), "{screen}");
    assert!(screen.contains("✗ r-x"), "{screen}");
    assert!(screen.contains("Enter kill orphans"), "{screen}");
    assert!(screen.contains("Esc back"), "{screen}");
    assert!(!screen.contains("Needs input"), "{screen}");
}

#[test]
fn cards_and_forms_show_the_model_claude_reports() {
    let mut app = app();
    let mut sessions = app.sessions.clone();
    sessions[0].running_model = Some("claude-opus-5-5[1m]".into());
    app.set_data(app.projects.clone(), sessions);
    app.models.names = [("opus".to_string(), "Opus 5.5".to_string())].into();
    app.preview.stats = Some(crate::stats::AgentStats {
        prompts: Some(5),
        output_tokens: Some(92_000),
        ..Default::default()
    });
    let screen = render(&app, 120, 16);
    assert!(
        screen.contains("✱ api · Opus 5.5 (1M) high   2d"),
        "{screen}"
    );
    assert!(
        screen.contains("created 2d · 5 prompts · 92k out"),
        "{screen}"
    );
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    // From the name: agent, then model.
    for _ in 0..2 {
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let screen = render(&app, 120, 24);
    assert!(screen.contains("‹ opus ›  Opus 5.5"), "{screen}");
}

#[test]
fn long_titles_are_cut_with_an_ellipsis() {
    assert_eq!(truncate("abcdef", 4), "abc…");
    assert_eq!(truncate("abc", 4), "abc");
    let line: String = fit(
        10,
        vec![Span::raw("long title here")],
        vec![Span::raw("3m")],
    )
    .iter()
    .map(|s| s.content.to_string())
    .collect();
    assert_eq!(line, "long t… 3m");
    // No room at all: nothing, not a "…" past the edge.
    assert_eq!(
        (truncate("abc", 0), tail("abc", 0)),
        (String::new(), String::new())
    );
    assert_eq!(tail("abcdef", 4), "…def");
    // Columns are measured in cells: a wide character takes two.
    assert_eq!(pad("日本", 6), "日本  ");
    assert_eq!(pad("日本語です", 5), "日本…");
}

#[test]
fn long_project_names_keep_their_count_and_fold_mark() {
    let spans = heading(
        Group::Project(1),
        "customer-portal-frontend-migration-2026",
        3,
        true,
        34,
    );
    let line: String = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(line.width(), 34, "{line}");
    assert!(line.contains("… (3) ") && line.ends_with(" ⌄ "), "{line}");
}

#[test]
fn sessions_without_a_screen_say_what_they_were() {
    let app = app();
    let now = now_ms();
    let text = |t: Text| -> String {
        t.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    };
    let mut s = app.sessions[0].clone();
    s.deleted_at = Some(now - 2 * 86_400_000);
    s.conversation_id = Some("c0ffee123456".into());
    s.workdir = "/r/services".into();
    let prompts = [
        "fix the tests".to_string(),
        "and the\nchangelog".to_string(),
    ];
    let shown = text(app.session_info(&s, &prompts, &[], now));
    for part in [
        "Archived 2d ago.",
        "Project    RO roster  /r",
        "Directory  /r/services",
        "Agent      ✱ claude · opus · high",
        "Title      Fix scrollback (Claude's)",
        "Last prompts\n  › fix the tests\n  › and the changelog",
        "Enter restores it; starting it then resumes its conversation (c0ffee12).",
        "s D deletes it for good; its Claude conversation stays and can be adopted again.",
    ] {
        assert!(shown.contains(part), "{part:?} in\n{shown}");
    }
    // Stopped, gone from tmux: how to start it; its own directory isn't repeated.
    s.deleted_at = None;
    s.workdir = "/r".into();
    s.exit_status = Some(1);
    s.state_since = Some(now - 3 * 3_600_000);
    let screen = ["roster: preparing the environment failed".to_string()];
    let shown = text(app.session_info(&s, &[], &screen, now));
    assert!(
        shown.starts_with("Stopped 3h ago: its agent exited with 1."),
        "{shown}"
    );
    assert!(
        shown.contains("Its last screen\n  roster: preparing the environment failed\n"),
        "{shown}"
    );
    // Before the note, which can be long.
    s.note = Some(crate::note::Note {
        text: "a note".into(),
        pin: None,
        at: 0,
    });
    let shown = text(app.session_info(&s, &[], &screen, now));
    assert!(
        shown.find("Its last screen") < shown.find("Note\n"),
        "{shown}"
    );
    assert!(
        !shown.contains("Directory") && !shown.contains("Last prompts"),
        "{shown}"
    );
    assert!(
        shown.contains("s s starts it; it resumes its conversation"),
        "{shown}"
    );
}

#[test]
fn details_name_what_the_mark_doesnt_say() {
    let text = |s: &Session| -> String {
        details(s, &[], Style::new())
            .iter()
            .map(|s| s.content.to_string())
            .collect()
    };
    let mut s = session("a1");
    s.model = Some("opus".into());
    s.effort = Some("high".into());
    // A made-up name only repeats the type; a chosen one says something.
    s.name = "claude-1".into();
    assert_eq!(text(&s), "✱ opus high");
    s.name = "tester".into();
    assert_eq!(text(&s), "✱ tester · opus high");
    s.agent = "pi".into();
    s.name = "pi-1".into();
    assert_eq!(text(&s), "π opus high");
    s.name = "MyName".into();
    assert_eq!(text(&s), "π MyName · opus high");
    // The generic mark doesn't say which agent it is: its type does.
    s.agent = "aider".into();
    (s.model, s.effort) = (None, None);
    s.name = "aider-1".into();
    assert_eq!(text(&s), "◇ aider");
    s.name = "reviewer".into();
    assert_eq!(text(&s), "◇ reviewer · aider");
}

#[test]
fn a_note_shows_its_pinned_lines_above_the_screen() {
    let mut app = app();
    app.sessions[0].note = Some(crate::note::Note {
        text: "TODO\n- [x] one\n- [ ] two\n- [ ] three".into(),
        pin: Some(2),
        at: 0,
    });
    app.set_data(app.projects.clone(), app.sessions.clone());
    app.selected = 1;
    app.preview.screen = true;
    let screen = render(&app, 120, 16);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("✎ - [ ] two"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(lines[at + 1].contains("  - [ ] three"), "{screen}");
    assert!(lines[at + 1].ends_with("3–4 of 4"), "{screen}");
    assert!(lines[at + 2].contains("────"), "{screen}");
    assert!(lines[at + 3].contains("agent screen line"), "{screen}");
    // The card says it has one.
    assert!(screen.contains("✎ 2d"), "{screen}");
    // Not over a session's details (there it's whole), nor without a note.
    app.preview.screen = false;
    assert!(!render(&app, 120, 16).contains("✎ - [ ] two"));
    app.preview.screen = true;
    app.selected = 3;
    assert!(!render(&app, 120, 16).contains("✎ -"));
}

#[test]
fn the_note_editor_sits_above_the_screen() {
    let mut app = app();
    app.sessions[0].note = Some(crate::note::Note {
        text: "first\nsecond\nthird".into(),
        pin: Some(1),
        at: 0,
    });
    app.set_data(app.projects.clone(), app.sessions.clone());
    app.selected = 1;
    let s = app.selected_session().unwrap().clone();
    app.note = Some(crate::tui::note::NoteEditor::new(&s, None));
    app.preview.screen = true;
    let screen = render(&app, 120, 16);
    assert!(screen.contains("╭ Note "), "{screen}");
    assert!(screen.contains(" NORMAL "), "{screen}");
    // The pinned line marked; the cursor starts on it.
    assert!(screen.contains("│▸ second"), "{screen}");
    assert!(screen.contains(" 2/3 "), "{screen}");
    assert!(screen.contains("agent screen line"), "{screen}");
    assert!(screen.contains(":wq save and close"), "{screen}");
    // Typing a command shows it at the bottom.
    app.note
        .as_mut()
        .unwrap()
        .on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
    assert!(render(&app, 120, 16).contains("╰ : "), "{screen}");
}
