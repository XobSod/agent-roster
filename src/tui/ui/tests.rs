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

#[test]
fn the_list_beside_an_attached_agent_is_the_tuis_a_row_up() {
    let mut app = app();
    still(&mut app);
    let now = now_ms();
    // Limits known: the TUI's sidebar gives them its last row.
    app.limits.agents = vec![crate::tui::limits::Said {
        agent: "claude".into(),
        usage: Some(crate::usage::Usage {
            plan: None,
            windows: vec![crate::usage::Window {
                minutes: 300,
                used: 40.0,
                resets_at: Some(now + 3_600_000),
            }],
            notes: Vec::new(),
            at: now,
        }),
        error: None,
    }];
    let (width, height) = (120, 16);
    let side = sidebar_width(width);
    let drawn = |size: (u16, u16), draw: &dyn Fn(&mut Frame)| {
        let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
        terminal.draw(|f| draw(f)).unwrap();
        terminal.backend().buffer().clone()
    };
    let tui = drawn((width, height), &|f| draw(f, &app));
    // Under the status line, which shows the top line.
    let attached = drawn((side + 1, height - 1), &|f| {
        draw_attached_sidebar(f, &app, false, "C-q")
    });
    let row = |buffer: &ratatui::buffer::Buffer, y: u16, width: u16| {
        (0..width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    // Every row of the TUI's sidebar but its top line, the limits' too, a
    // row up; the back key where the TUI's hints are.
    for y in 1..height - 1 {
        assert_eq!(row(&attached, y - 1, side), row(&tui, y, side), "row {y}");
    }
    assert!(
        row(&tui, height - 2, side).contains('%'),
        "{}",
        row(&tui, height - 2, side)
    );
    assert!(row(&attached, height - 2, side).starts_with(" C-q back"));
}

#[test]
fn the_list_beside_an_attached_agent_is_faint_but_for_its_session() {
    let mut app = app();
    still(&mut app);
    let (width, height) = (37, 12);
    let drawn = |draw: &dyn Fn(&mut Frame)| {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f)).unwrap();
        terminal.backend().buffer().clone()
    };
    let normal = drawn(&|f| {
        let area = Rect::new(0, 0, width - 1, height - 1);
        draw_sidebar(f, &app, area, now_ms(), false);
    });
    let attached = drawn(&|f| draw_attached_sidebar(f, &app, false, "C-q"));
    let row = |y: u16| {
        (0..width)
            .map(|x| attached[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    // Its top line left to the status line; the back key at the bottom.
    assert!(!row(0).contains("roster"), "{}", row(0));
    assert!(
        row(height - 1).starts_with(" C-q back"),
        "{}",
        row(height - 1)
    );
    let mut selected = 0;
    for y in 0..height - 1 {
        for x in 0..width - 1 {
            let (was, is) = (&normal[(x, y)], &attached[(x, y)]);
            assert_eq!(was.symbol(), is.symbol());
            if was.bg == SELECTED_BG {
                selected += 1;
                assert_eq!((is.fg, is.bg), (was.fg, ATTACHED_BG), "({x}, {y})");
            } else {
                assert_eq!(is.fg, faded(was.fg), "({x}, {y})");
            }
        }
    }
    assert!(selected > 0);
}

/// Nothing in `app` that moves between two draws a moment apart: a working
/// card's spinner turns every 120 ms.
fn still(app: &mut App) {
    let mut sessions = app.sessions.clone();
    for s in sessions.iter_mut().filter(|s| s.state == State::Working) {
        s.state = State::Idle;
    }
    app.set_data(app.projects.clone(), sessions);
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
    // Started with it, as a launch records.
    a.running_effort = a.effort.clone();
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
            note: None,
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
    app.set_grouped(true);
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
fn stopped_and_archived_sessions_can_be_whole_cards() {
    let mut app = app();
    let mut sessions = app.sessions.clone();
    for s in &mut sessions {
        s.state = State::Stopped;
    }
    sessions[0].state_since = Some(now_ms() - 3 * 3_600_000);
    sessions[1].deleted_at = Some(now_ms() - 2 * 86_400_000);
    app.set_data(app.projects.clone(), sessions);
    app.set_grouped(true);
    app.set_stopped_cards(crate::config::StoppedCards::Unfolded);
    // The Archived shelf unfolded.
    app.selected = (app.entries.iter())
        .position(|e| {
            matches!(
                e,
                Entry::Heading {
                    group: Group::Shelf(crate::tui::list::Shelf::Archived),
                    ..
                }
            )
        })
        .unwrap();
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    assert!(!app.entries.iter().any(|e| matches!(e, Entry::Row(_))));
    let screen = render(&app, 120, 24);
    // Each a card: its title and state, then its name; the archived one
    // with its project, as it's under no project's heading.
    assert!(screen.contains("stopped 3h"), "{screen}");
    assert!(screen.contains("archived 2d"), "{screen}");
    assert!(screen.contains("✱ api · opus high"), "{screen}");
    assert!(screen.contains("✱ db"), "{screen}");
    // The project's heading, and the archived card's own line.
    assert_eq!(screen.matches("RO roster").count(), 2, "{screen}");
    // Folded, a line each again.
    app.set_stopped_cards(crate::config::StoppedCards::Folded);
    assert!(app.entries.iter().any(|e| matches!(e, Entry::Row(_))));
}

#[test]
fn a_worktree_sessions_card_says_its_branch_and_how_it_stands() {
    let mut app = app();
    let mut sessions = app.sessions.clone();
    sessions[0].worktree = Some(crate::model::Worktree {
        root: "/w/roster/fix-auth".into(),
        repo: "/r".into(),
        branch: "fix-auth".into(),
        base: Some("main".into()),
        made: true,
        set_up: true,
        trusted_in: None,
    });
    app.set_data(app.projects.clone(), sessions);
    // Nothing to say yet: its branch alone, between its title and its agent.
    let screen = render(&app, 120, 24);
    // The list's side of each line.
    let lines: Vec<&str> = (screen.lines())
        .map(|l| l.split('│').next().unwrap_or(""))
        .collect();
    let at = lines
        .iter()
        .position(|l| l.contains("⎇ fix-auth ← main"))
        .unwrap();
    assert!(lines[at - 1].contains("Fix scrollback"), "{screen}");
    assert!(lines[at + 1].contains("api"), "{screen}");
    assert!(!lines[at].contains("ahead"), "{screen}");
    // Once read: how far ahead, what's changed; the preview says it whole.
    app.worktrees.standing.insert(
        "a1".into(),
        crate::engine::Standing {
            ahead: 3,
            behind: 0,
            changed: 2,
            diff: Some((4, 120, 30)),
        },
    );
    app.selected = (app.entries.iter())
        .position(|e| matches!(e, Entry::Card(s) if s.id == "a1"))
        .unwrap();
    let screen = render(&app, 160, 24);
    assert!(screen.contains("3 ahead · 2 changed"), "{screen}");
    assert!(
        screen.contains(
            "⎇ fix-auth ← main · 3 ahead · 2 files changed, not committed · +120 −30 in 4 files"
        ),
        "{screen}"
    );
}

#[test]
fn rows_keep_their_projects_note_mark_with_a_long_name() {
    let mut app = app();
    let mut projects = app.projects.clone();
    projects[0].name = "customer-portal-frontend".into();
    projects[0].note = Some(crate::note::Note {
        text: "backlog".into(),
        pin: None,
        at: 0,
    });
    let mut sessions = app.sessions.clone();
    for s in &mut sessions {
        s.project = "customer-portal-frontend".into();
        s.state = State::Stopped;
        s.note = None;
    }
    sessions[1].deleted_at = Some(now_ms());
    app.set_data(projects, sessions);
    // As wide as a sidebar's: the name is cut, its mark and the state stay.
    // Each with how long ago: stopped, or archived.
    let mut sessions = app.sessions.clone();
    sessions[0].state_since = Some(now_ms() - 3 * 3_600_000);
    sessions[1].deleted_at = Some(now_ms() - 2 * 86_400_000);
    app.set_data(app.projects.clone(), sessions);
    let now = now_ms();
    for (s, state) in app.sessions.iter().zip(["stopped 3h", "archived 2d"]) {
        let line: String = row(&app, s, 33, now)
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert!(line.contains("… ▤"), "{line}");
        assert!(line.contains(state), "{line}");
        assert!(line.width() <= 33, "{line}");
    }
    // One whose agent exited with an error says so, and when, its note
    // marked; the header too.
    let mut failed = app.sessions[0].clone();
    failed.exit_status = Some(137);
    failed.note = app.projects[0].note.clone();
    let line: String = row(&app, &failed, 33, now)
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    assert!(line.contains("✎ exit 137 · 3h"), "{line}");
    assert!(line.width() <= 33, "{line}");
    assert_eq!(state_words(&failed, now).content, "exited 137 · 3h");
    assert_eq!(state_words(&app.sessions[0], now).content, "stopped 3h");
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
        note: None,
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
    assert!(screen.contains("a archive / restore"), "{screen}");
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
        figures: None,
        item: None,
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
    assert!(
        screen.contains("tmux server, database and processes"),
        "{screen}"
    );
    assert!(screen.contains("tmux sessions (1)"), "{screen}");
    assert!(screen.contains("✗ r-x"), "{screen}");
    assert!(screen.contains("Enter kill orphans"), "{screen}");
    assert!(screen.contains("Esc back"), "{screen}");
    assert!(!screen.contains("Needs input"), "{screen}");

    // A session's line, folded, its figures between what names it and the
    // rest; the cursor's line kept in view, far down a long report.
    use crate::doctor::{Item, What};
    let mut lines = vec![Line {
        level: Level::Ok,
        text: "r-1  demo/api  alive".into(),
        figures: Some((15, "  312 MB  CPU  99%  ".into())),
        item: Some(Item {
            what: What::Session("s1".into()),
            depth: 0,
            children: true,
        }),
    }];
    lines.extend((0..30).map(|i| line(Level::Info, &format!("  line {i}"))));
    // Back in the view, the cursor at its top.
    for c in [KeyCode::Esc, KeyCode::Char('v'), KeyCode::Char('t')] {
        app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
    }
    app.set_report(Diagnosis {
        lines,
        orphans: Vec::new(),
        problems: 0,
    });
    let screen = render(&app, 120, 12);
    assert!(
        screen.contains("✓ ▸ r-1  demo/api    312 MB  CPU  99%  alive"),
        "{screen}"
    );
    app.report_cursor = 28;
    let screen = render(&app, 120, 12);
    assert!(
        screen.contains("line 27") && !screen.contains("r-1"),
        "{screen}"
    );
}

#[test]
fn a_session_using_too_much_memory_is_marked() {
    let mut app = app();
    let gb = 1u64 << 30;
    app.memory_limit = Some(gb);
    let id = app.sessions[0].id.clone();
    let totals = |memory| crate::procs::Totals {
        memory,
        cpu: None,
        processes: 3,
        heaviest: ("cargo".into(), memory),
    };
    app.set_resources([(id.clone(), totals(2 * gb + gb / 2))].into());
    let at = (app.entries.iter())
        .position(|e| e.session().is_some_and(|s| s.id == id))
        .unwrap();
    app.selected = at;
    let screen = render(&app, 140, 30);
    // On its card, in its header, and in a line under the summary.
    assert!(screen.matches("⚠ 2.5 GB").count() >= 2, "{screen}");
    assert!(screen.contains("⚠ 1 session over 1.0 GB"), "{screen}");
    // Back just under the limit: still marked, not flapping; well under it,
    // not.
    app.set_resources([(id.clone(), totals(gb - gb / 20))].into());
    assert!(render(&app, 140, 30).contains("⚠ 1 session over"));
    app.set_resources([(id.clone(), totals(gb / 2))].into());
    assert!(!render(&app, 140, 30).contains('⚠'));
    // The limit raised: what was marked under the old one isn't.
    app.set_resources([(id.clone(), totals(2 * gb))].into());
    app.set_memory_limit(Some(3 * gb));
    assert!(!render(&app, 140, 30).contains('⚠'));
    app.set_memory_limit(Some(gb));
    // Two over; with no limit, none.
    let other = app.sessions[1].id.clone();
    app.set_resources([(id.clone(), totals(2 * gb)), (other, totals(3 * gb))].into());
    assert!(render(&app, 140, 30).contains("⚠ 2 sessions over 1.0 GB"));
    app.memory_limit = None;
    assert!(!render(&app, 140, 30).contains('⚠'));
}

#[test]
fn what_an_archived_session_left_using_too_much_memory_shows() {
    let mut app = app();
    let gb = 1u64 << 30;
    app.memory_limit = Some(gb);
    let mut sessions = app.sessions.clone();
    sessions[0].state = State::Stopped;
    sessions[0].deleted_at = Some(now_ms());
    let id = sessions[0].id.clone();
    app.set_data(app.projects.clone(), sessions);
    let totals = crate::procs::Totals {
        memory: 2 * gb,
        cpu: None,
        processes: 1,
        heaviest: ("node".into(), 2 * gb),
    };
    app.set_resources([(id.clone(), totals)].into());
    // Its shelf folded: marked on it.
    let screen = render(&app, 140, 30);
    let shelf = screen.lines().find(|l| l.contains("Archived (1)")).unwrap();
    assert!(shelf.split('│').next().unwrap().contains("⚠"), "{screen}");
    // Unfolded: on its row.
    let s = app.sessions.iter().find(|s| s.id == id).unwrap();
    let line: String = row(&app, s, 40, now_ms())
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    assert!(line.contains("⚠ 2.0 GB"), "{line}");
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
    let line = |noted| -> String {
        let name = "customer-portal-frontend-migration-2026";
        let spans = heading(Group::Project(1), name, 3, true, noted, false, 34);
        spans.iter().map(|s| s.content.to_string()).collect()
    };
    let (plain, noted) = (line(false), line(true));
    assert_eq!(plain.width(), 34, "{plain}");
    assert!(
        plain.contains("… (3) ") && plain.ends_with(" ⌄ "),
        "{plain}"
    );
    // With a note, its mark after the name, which is cut for it.
    assert_eq!(noted.width(), 34, "{noted}");
    assert!(
        noted.contains("… ▤ (3) ") && noted.ends_with(" ⌄ "),
        "{noted}"
    );
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
    // Resumed, as Claude has written it.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("c0ffee123456.jsonl");
    std::fs::write(&transcript, "").unwrap();
    s.transcript_path = Some(transcript.to_string_lossy().into_owned());
    s.workdir = "/r/services".into();
    let prompts = [
        "fix the tests".to_string(),
        "and the\nchangelog".to_string(),
    ];
    let shown = text(app.session_info(&s, &prompts, &[], now));
    for part in [
        "Archived 2d ago.",
        "Project       RO roster  /r",
        "Directory     /r/services",
        "Agent         ✱ claude · opus · high",
        "Title         Fix scrollback (Claude's)",
        "Conversation  c0ffee12 (resumed when started)",
        "Last prompts\n  › fix the tests\n  › and the changelog",
    ] {
        assert!(shown.contains(part), "{part:?} in\n{shown}");
    }
    // No keys: the footer and the menus have them.
    assert!(
        !shown.contains("restores it") && !shown.contains("for good"),
        "{shown}"
    );
    // Stopped, gone from tmux: its own directory isn't repeated.
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
    assert!(!shown.contains("starts it"), "{shown}");
    // One it can't go on with: none; one its agent never wrote (stopped
    // before a prompt, or since removed), which it starts anew; or an agent
    // that doesn't resume.
    s.conversation_id = None;
    let shown = text(app.session_info(&s, &[], &[], now));
    assert!(
        shown.contains("Conversation  a new one when started"),
        "{shown}"
    );
    s.conversation_id = Some("c0ffee123456".into());
    std::fs::remove_file(&transcript).unwrap();
    let shown = text(app.session_info(&s, &[], &[], now));
    assert!(
        shown.contains("Conversation  a new one when started"),
        "{shown}"
    );
    // Adopted as a copy and not written yet: the original, forked.
    s.fork_from = Some("0a1b2c3d4e5f".into());
    let shown = text(app.session_info(&s, &[], &[], now));
    assert!(
        shown.contains("Conversation  a fork of 0a1b2c3d when started"),
        "{shown}"
    );
    // Once the copy is written, it's the copy that's resumed.
    std::fs::write(&transcript, "").unwrap();
    let shown = text(app.session_info(&s, &[], &[], now));
    assert!(
        shown.contains("Conversation  c0ffee12 (resumed when started)"),
        "{shown}"
    );
    s.fork_from = None;
    s.agent = "aider".into();
    let shown = text(app.session_info(&s, &[], &[], now));
    assert!(
        shown.contains("Conversation  a new one when started"),
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
    // Started with it, as a launch records.
    s.running_effort = s.effort.clone();
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
    (s.model, s.effort, s.running_effort) = (None, None, None);
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
    // In its box, from the row under the statistics.
    assert!(lines[at - 1].contains("╭────"), "{screen}");
    assert!(lines[at - 2].contains("created 2d"), "{screen}");
    assert!(lines[at + 1].contains("  - [ ] three"), "{screen}");
    assert!(lines[at + 1].ends_with("3–4 of 4 │"), "{screen}");
    assert!(lines[at + 2].contains("╰────"), "{screen}");
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
fn a_card_says_how_many_subagents_are_at_work() {
    let mut app = app();
    let now = crate::util::now_ms();
    app.sessions[0].state = crate::model::State::Working;
    app.sessions[0].state_since = Some(now - 65_000);
    let heard = |at| crate::model::Subagent {
        kind: None,
        started: at,
        heard: at,
    };
    app.sessions[0].subagents = [
        ("b1".to_string(), heard(now)),
        ("b2".to_string(), heard(now - 1000)),
    ]
    .into();
    app.set_data(app.projects.clone(), app.sessions.clone());
    let at = (app.entries.iter())
        .position(|e| matches!(e, Entry::Card(s) if !s.subagents.is_empty()))
        .unwrap();
    app.selected = at;
    let screen = render(&app, 120, 16);
    assert!(screen.contains("↳2 "), "{screen}");
    assert!(screen.contains("working 1m, 2 subagents"), "{screen}");
    // One unheard of for long isn't counted.
    let mut sessions = app.sessions.clone();
    sessions[0].subagents.insert(
        "b2".into(),
        heard(now - crate::model::SUBAGENT_QUIET_MS - 1000),
    );
    app.set_data(app.projects.clone(), sessions);
    let screen = render(&app, 120, 16);
    assert!(screen.contains("↳1 "), "{screen}");
    assert!(screen.contains("working 1m, 1 subagent"), "{screen}");
    assert!(!screen.contains("subagents"), "{screen}");
}

#[test]
fn a_card_unfolds_to_show_its_subagents() {
    use crate::config::CardSubagents;
    let mut app = app();
    let now = crate::util::now_ms();
    let at = |started: i64, kind: &str| crate::model::Subagent {
        kind: Some(kind.into()),
        started: now - started,
        heard: now,
    };
    let mut sessions = app.sessions.clone();
    sessions[0].state = crate::model::State::Working;
    sessions[0].running_model = Some("claude-opus-5-5[1m]".into());
    sessions[0].subagents = [
        ("b2".to_string(), at(185_000, "general-purpose")),
        ("b1".to_string(), at(725_000, "Explore")),
        ("b3".to_string(), at(5_000, "fork")),
    ]
    .into();
    app.set_data(app.projects.clone(), sessions);
    app.selected = (app.entries.iter())
        .position(|e| matches!(e, Entry::Card(s) if !s.subagents.is_empty()))
        .unwrap();
    let info = |task: &str, model: &str| crate::agent::SubagentInfo {
        task: Some(task.into()),
        model: Some(model.into()),
        settled: true,
        file: None,
    };
    app.subagent_info = [
        (
            "b1".to_string(),
            info(
                "Find where the tmux calls are made",
                "claude-haiku-4-5-20251001",
            ),
        ),
        ("b3".to_string(), info("Fix it", "inherit")),
    ]
    .into();
    let key = |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    // Folded: how many, and that it opens, on its details' line.
    let screen = render(&app, 120, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let card = (lines.iter())
        .position(|l| l.contains("↳3 › "))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(lines[card].contains("✱ api"), "{screen}");
    assert!(!lines[card - 1].contains("↳3"), "{screen}");
    assert!(!screen.contains("↳ haiku"), "{screen}");
    // Unfolded: a line each, as they started: the model, what it's at cut
    // to fit, how long.
    key(&mut app, 'l');
    let screen = render(&app, 120, 24);
    assert!(screen.contains("↳3 ⌄ "), "{screen}");
    let lines: Vec<&str> = screen.lines().collect();
    let first = (lines.iter())
        .position(|l| l.contains("↳ haiku · Find where"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(
        lines[first].contains("…") && lines[first].contains(" 12m"),
        "{screen}"
    );
    // Not read yet: its kind; its parent's model, the session's.
    assert!(
        lines[first + 1].contains("↳ general-purpose") && lines[first + 1].contains(" 3m"),
        "{screen}"
    );
    assert!(
        lines[first + 2].contains("↳ opus · Fix it") && lines[first + 2].contains(" 5s"),
        "{screen}"
    );
    // What it's doing, once it says, in place of what it was given.
    app.subagent_doing = [("b3".to_string(), "Run the tests".to_string())].into();
    let screen = render(&app, 120, 24);
    assert!(screen.contains("↳ opus · Run the tests"), "{screen}");
    app.subagent_doing.clear();
    // h folds it, then its shelf.
    key(&mut app, 'h');
    assert!(!render(&app, 120, 24).contains("↳ haiku"));
    assert!(
        app.entries
            .iter()
            .any(|e| matches!(e, Entry::Card(s) if !s.subagents.is_empty()))
    );
    key(&mut app, 'h');
    assert!(
        !app.entries
            .iter()
            .any(|e| matches!(e, Entry::Card(s) if !s.subagents.is_empty()))
    );
    key(&mut app, 'l');
    app.selected = (app.entries.iter())
        .position(|e| matches!(e, Entry::Card(s) if !s.subagents.is_empty()))
        .unwrap();
    // Always unfolded: the lines say how many; the count only: no lines.
    app.card_subagents = CardSubagents::Unfolded;
    let screen = render(&app, 120, 24);
    assert!(!screen.contains("↳3"), "{screen}");
    assert!(screen.contains("↳ haiku · Find where"), "{screen}");
    // The header still says how many.
    assert!(screen.contains("3 subagents"), "{screen}");
    app.card_subagents = CardSubagents::Count;
    let screen = render(&app, 120, 24);
    assert!(
        screen.contains("↳3 ") && !screen.contains("↳ haiku"),
        "{screen}"
    );
    key(&mut app, 'l');
    assert!(!render(&app, 120, 24).contains("↳ haiku"));
}

#[test]
fn a_card_on_its_shells_alone_is_working_and_unread() {
    let mut app = app();
    let now = crate::util::now_ms();
    let mut sessions = app.sessions.clone();
    let s = sessions.iter_mut().find(|s| s.id == "b1").unwrap();
    s.title = Some("Speed up the build".into());
    s.state_since = Some(now - 60_000);
    s.unread = true;
    s.awaiting = true;
    let shell = crate::model::Shell {
        what: Some("Run the full test suite".into()),
        started: now - 180_000,
        from: 0,
    };
    s.shells = [("b9".to_string(), shell)].into();
    app.set_data(app.projects.clone(), sessions);
    app.selected = (app.entries.iter())
        .position(|e| matches!(e, Entry::Card(s) if !s.shells.is_empty()))
        .unwrap();
    let screen = render(&app, 120, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let card = (lines.iter())
        .position(|l| l.contains("$1 › "))
        .unwrap_or_else(|| panic!("{screen}"));
    // On the Working shelf, marked unread beside its spinner, its shell
    // counted; the header says what it's at.
    let shelf = (lines[..card].iter())
        .rev()
        .find(|l| l.contains(" (1)"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(shelf.contains("Working"), "{screen}");
    assert!(
        lines[card - 1].contains("Speed up the build") && lines[card - 1].contains("● "),
        "{screen}"
    );
    assert!(screen.contains("working 1m, 1 shell"), "{screen}");
    // Unfolded: what it does, how long it's been running; still so once
    // what's at work is read again, as after every key.
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    crate::tui::transcripts::read_subagents(&mut app);
    let screen = render(&app, 120, 24);
    let line = (screen.lines())
        .find(|l| l.contains("$ Run the full test suite"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(line.contains(" 3m"), "{screen}");
    // Read, it's working only.
    let mut sessions = app.sessions.clone();
    sessions.iter_mut().find(|s| s.id == "b1").unwrap().unread = false;
    app.set_data(app.projects.clone(), sessions);
    let screen = render(&app, 120, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let card = (lines.iter())
        .position(|l| l.contains("$1 ⌄ "))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(!lines[card - 1].contains("● "), "{screen}");
}

#[test]
fn only_a_card_with_subagents_at_work_folds_and_unfolds() {
    use crate::config::CardSubagents;
    let mut app = app();
    let now = crate::util::now_ms();
    let at = |started: i64, heard: i64| crate::model::Subagent {
        kind: Some("Explore".into()),
        started: now - started,
        heard: now - heard,
    };
    let mut sessions = app.sessions.clone();
    sessions[0].state = crate::model::State::Working;
    // Two started at once, after their file names; one long unheard of.
    sessions[0].subagents = [
        ("b2".to_string(), at(60_000, 0)),
        ("b1".to_string(), at(60_000, 0)),
        (
            "b0".to_string(),
            at(30_000, crate::model::SUBAGENT_QUIET_MS + 1),
        ),
    ]
    .into();
    app.set_data(app.projects.clone(), sessions);
    let card = |app: &App| {
        (app.entries.iter()).position(|e| matches!(e, Entry::Card(s) if !s.subagents.is_empty()))
    };
    app.selected = card(&app).unwrap();
    let info = |task: &str| crate::agent::SubagentInfo {
        task: Some(task.into()),
        ..Default::default()
    };
    app.subagent_info = [
        ("b1".to_string(), info("first")),
        ("b2".to_string(), info("second")),
    ]
    .into();
    let key = |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    key(&mut app, 'l');
    let screen = render(&app, 120, 24);
    assert!(screen.contains("↳2 ⌄"), "{screen}");
    let lines: Vec<&str> = screen.lines().collect();
    let first = (lines.iter())
        .position(|l| l.contains("↳ first"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(lines[first + 1].contains("↳ second"), "{screen}");
    assert!(!lines[first + 2].contains("↳ "), "{screen}");
    // Always unfolded, or the count only: l and h are the shelf's.
    for mode in [CardSubagents::Unfolded, CardSubagents::Count] {
        app.unfolded.clear();
        app.card_subagents = mode;
        key(&mut app, 'l');
        assert!(app.unfolded.is_empty());
        app.selected = card(&app).unwrap();
        key(&mut app, 'h');
        assert!(card(&app).is_none(), "{mode:?}");
        key(&mut app, 'l');
        app.selected = card(&app).unwrap();
    }
    // Unfolded once, its subagents gone since: h folds its shelf.
    app.card_subagents = CardSubagents::Folded;
    let mut sessions = app.sessions.clone();
    let id = sessions[0].id.clone();
    sessions[0].subagents.clear();
    app.set_data(app.projects.clone(), sessions);
    app.unfolded.insert(id.clone());
    app.selected = (app.entries.iter())
        .position(|e| matches!(e, Entry::Card(s) if s.id == id))
        .unwrap();
    key(&mut app, 'h');
    assert!(
        !app.entries
            .iter()
            .any(|e| matches!(e, Entry::Card(s) if s.id == id))
    );
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
    let editor = crate::tui::note::NoteEditor::new(&s.noted(), None);
    let side = crate::tui::note::Side::Session;
    let open = crate::tui::note::OpenNotes::new(Some(s.id.clone()), s.project_id, side, editor);
    app.note = Some(open);
    app.preview.screen = true;
    let screen = render(&app, 120, 16);
    // Whose it is, and the project's to show with ].
    assert!(screen.contains(&format!("╭ ✎ {} ", s.label())), "{screen}");
    assert!(screen.contains(" [ ▤ project "), "{screen}");
    assert!(screen.contains(" NORMAL "), "{screen}");
    // The pinned line marked; the cursor starts on it.
    assert!(screen.contains("│✎ second"), "{screen}");
    assert!(screen.contains(" 2/3 "), "{screen}");
    assert!(screen.contains("agent screen line"), "{screen}");
    assert!(screen.contains(":wq save and close"), "{screen}");
    // Typing a command shows it at the bottom.
    app.note
        .as_mut()
        .unwrap()
        .shown_mut()
        .on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
    assert!(render(&app, 120, 16).contains("╰ : "), "{screen}");
}

#[test]
fn a_shelf_shows_its_sessions_at_a_glance() {
    let mut app = app();
    let now = now_ms();
    let mut sessions = app.sessions.clone();
    sessions[0].note = Some(crate::note::Note {
        text: "TODO\n- [ ] check the fork path\n- [ ] README".into(),
        pin: Some(1),
        at: 0,
    });
    app.set_data(app.projects.clone(), sessions);
    // The Needs input heading, with a prompt read from its session's transcript.
    app.selected = 0;
    let prompts = std::collections::HashMap::from([(
        "a1".to_string(),
        "fix the scrollback\nand its tests".to_string(),
    )]);
    let size = (82, 13);
    app.preview.text = app.group_info(
        crate::tui::list::Group::Shelf(crate::tui::list::Shelf::NeedsInput),
        &prompts,
        size,
        now,
    );
    let screen = render(&app, 120, 16);
    for part in [
        "Waiting for you: a question, a permission, or a turn you haven't read.",
        "Questions and permissions come first, then turns to read; within each, the one",
        "waiting longest is on top (order.needs_input).",
        "  ✱ RO roster / api / Fix scrollback",
        "needs input 2m",
        "     › fix the scrollback and its tests",
        // The pinned line, not the note's first.
        "     ✎ - [ ] check the fork path",
    ] {
        assert!(screen.contains(part), "{part:?} in\n{screen}");
    }
    // The working one is on its own shelf, not this one.
    assert!(!screen.contains("working 3m"), "{screen}");
}

#[test]
fn a_project_shows_what_it_is_and_its_sessions() {
    let mut app = app();
    let now = now_ms();
    let mut sessions = app.sessions.clone();
    for i in 0..4 {
        let mut s = session(&format!("x{i}"));
        s.project = "roster".into();
        s.name = format!("extra-{i}");
        s.state = State::Idle;
        s.state_since = Some(now - 60_000);
        sessions.push(s);
    }
    app.grouped = true;
    app.set_data(app.projects.clone(), sessions);
    app.selected = 0;
    let group = crate::tui::list::Group::Project(1);
    app.preview.text = app.group_info(group, &Default::default(), (82, 13), now);
    let screen = render(&app, 120, 16);
    for part in [
        "  Origin     /r",
        "  Env file   /r/.agent-roster.env (none)",
        "  Defaults   claude for its new sessions",
        // Its project is the heading's: not said again.
        "  ✱ api / Fix scrollback",
        "  ✱ extra-3",
        "working 3m",
    ] {
        assert!(screen.contains(part), "{part:?} in\n{screen}");
    }
    // What doesn't fit is counted.
    app.preview.text = app.group_info(group, &Default::default(), (82, 8), now);
    let screen = render(&app, 120, 16);
    assert!(screen.contains("     … and 3 more"), "{screen}");
    assert!(!screen.contains("extra-3"), "{screen}");
    // All of them when they just fit: no row is kept for more.
    let count = app.group_members(group).len();
    let text = app.group_info(group, &Default::default(), (82, 4 + count as u16), now);
    let lines: Vec<String> = text.lines.iter().map(|l| l.to_string()).collect();
    assert!(lines.iter().any(|l| l.contains("extra-3")), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains("more")), "{lines:#?}");
}

#[test]
fn a_groups_preview_fits_any_size() {
    let mut app = app();
    let now = now_ms();
    let mut sessions = app.sessions.clone();
    sessions[0].note = Some(crate::note::Note {
        text: "a note with a long first line to be cut short\nmore".into(),
        pin: None,
        at: 0,
    });
    app.set_data(app.projects.clone(), sessions);
    let prompts = std::collections::HashMap::from([("a1".to_string(), "x".repeat(200))]);
    let shelf = crate::tui::list::Group::Shelf(crate::tui::list::Shelf::NeedsInput);
    let empty = crate::tui::list::Group::Shelf(crate::tui::list::Shelf::Stopped);
    for group in [shelf, empty, crate::tui::list::Group::Project(1)] {
        for width in [20, 30, 82] {
            for height in 0..16 {
                let text = app.group_info(group, &prompts, (width, height), now);
                assert!(
                    text.lines.len() <= height as usize,
                    "{group:?} in {width}x{height}: {:#?}",
                    text.lines
                );
                for line in &text.lines {
                    assert!(
                        line.width() <= width as usize,
                        "{group:?} in {width}x{height}: {line}"
                    );
                }
            }
        }
    }
    let lines = |app: &App, size| -> Vec<String> {
        let text = app.group_info(shelf, &prompts, size, now);
        text.lines.iter().map(|l| l.to_string()).collect()
    };
    // Narrow, what the shelf is goes on over lines.
    let narrow = lines(&app, (30, 20));
    assert_eq!(
        narrow[..3],
        [
            "Waiting for you: a question, a",
            "permission, or a turn you",
            "haven't read."
        ],
        "{narrow:#?}"
    );
    // What the shelf is shows when its session fits below it (4 lines and
    // its 3), else the session first.
    assert!(lines(&app, (82, 7))[0].starts_with("Waiting for you"));
    assert!(lines(&app, (82, 6))[0].contains("Fix scrollback"));
    // An empty one's, when "No sessions here." fits below it too.
    let empty_shelf = |app: &App, size| -> Vec<String> {
        let text = app.group_info(empty, &prompts, size, now);
        text.lines.iter().map(|l| l.to_string()).collect()
    };
    assert_eq!(empty_shelf(&app, (82, 4)), ["  No sessions here."]);
    assert!(empty_shelf(&app, (82, 5))[0].starts_with("Their agents aren't running"));
    // Short, the session shows before what the shelf is.
    let short = lines(&app, (82, 4));
    assert!(short[0].contains("Fix scrollback"), "{short:#?}");
    assert!(
        !short.iter().any(|l| l.contains("Waiting for you")),
        "{short:#?}"
    );
    // Too short for one whole: a line each, then how many.
    let tiny = lines(&app, (82, 1));
    assert!(tiny[0].contains("Fix scrollback"), "{tiny:#?}");
    let mut sessions = app.sessions.clone();
    for id in ["a2", "a3"] {
        let mut other = sessions[0].clone();
        other.id = id.into();
        sessions.push(other);
    }
    app.set_data(app.projects.clone(), sessions);
    assert_eq!(lines(&app, (82, 1)), ["  3 sessions"]);
    let two = lines(&app, (82, 2));
    assert!(
        two[0].contains("Fix scrollback") && two[1] == "     … and 2 more",
        "{two:#?}"
    );
    let three = lines(&app, (82, 3));
    assert!(
        three.iter().all(|l| l.contains("Fix scrollback")),
        "{three:#?}"
    );
}

#[test]
fn wrap_breaks_between_words() {
    use crate::tui::ui::wrap;
    assert_eq!(wrap("one two three", 7), ["one two", "three"]);
    // A word wider than the width is cut.
    assert_eq!(wrap("a verylongword b", 6), ["a", "veryl…", "b"]);
    assert_eq!(wrap("  ", 5), Vec::<String>::new());
    assert_eq!(wrap("żółć gęś", 4), ["żółć", "gęś"]);
}

#[test]
fn a_groups_sessions_say_what_they_wait_for() {
    let mut app = app();
    let now = now_ms();
    let at = |s: &mut Session, state, ago_ms: i64| {
        s.state = state;
        s.state_since = Some(now - ago_ms);
    };
    let mut turn = session("t");
    turn.name = "claude-3".into();
    turn.title = Some("Read me".into());
    turn.unread = true;
    at(&mut turn, State::Idle, 60_000);
    let mut idle = session("i");
    at(&mut idle, State::Idle, 120_000);
    let mut clean = session("c");
    at(&mut clean, State::Stopped, 3_600_000);
    clean.exit_status = Some(0);
    let mut crashed = session("x");
    at(&mut crashed, State::Stopped, 60_000);
    crashed.exit_status = Some(1);
    let mut old = session("o");
    at(&mut old, State::Stopped, 0);
    old.deleted_at = Some(now - 2 * 86_400_000);
    app.set_data(app.projects.clone(), vec![turn, idle, clean, crashed, old]);
    let shelf = |app: &App, shelf| {
        let group = crate::tui::list::Group::Shelf(shelf);
        let text = app.group_info(group, &Default::default(), (82, 20), now);
        text.lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    use crate::tui::list::Shelf;
    let input = shelf(&app, Shelf::NeedsInput);
    // A name of its agent's making isn't said beside the title.
    assert!(
        input.contains("demo / Read me") && !input.contains("claude-3"),
        "{input}"
    );
    assert!(input.contains("turn to read 1m"), "{input}");
    assert!(shelf(&app, Shelf::Idle).contains("idle 2m"));
    let stopped = shelf(&app, Shelf::Stopped);
    assert!(
        stopped.contains("stopped 1h") && stopped.contains("exited 1 · 1m"),
        "{stopped}"
    );
    let archived = shelf(&app, Shelf::Archived);
    assert!(archived.contains("archived 2d"), "{archived}");
    // No keys: the footer and the menus have them.
    assert!(
        !archived.contains("Enter") && !archived.contains("s D"),
        "{archived}"
    );
}

#[test]
fn a_blank_pinned_line_shows_the_next() {
    let mut app = app();
    let now = now_ms();
    let mut sessions = app.sessions.clone();
    sessions[0].note = Some(crate::note::Note {
        text: "TODO\n\n- [ ] the next one".into(),
        pin: Some(1),
        at: 0,
    });
    app.set_data(app.projects.clone(), sessions);
    let shelf = crate::tui::list::Group::Shelf(crate::tui::list::Shelf::NeedsInput);
    let text = app.group_info(shelf, &Default::default(), (82, 13), now);
    let lines: Vec<String> = text.lines.iter().map(|l| l.to_string()).collect();
    assert!(
        lines.contains(&"     ✎ - [ ] the next one".to_string()),
        "{lines:#?}"
    );
}

/// What `agent` said of its account's limits: a five-hour and a weekly window.
fn said(agent: &str, five_hour: f64, weekly: f64) -> crate::tui::limits::Said {
    use crate::usage::{Usage, Window};
    let now = now_ms();
    let window = |minutes, used, resets_in| Window {
        minutes,
        used,
        resets_at: Some(now + resets_in),
    };
    crate::tui::limits::Said {
        agent: agent.into(),
        usage: Some(Usage {
            plan: Some("pro".into()),
            windows: vec![
                window(300, five_hour, 3_600_000),
                window(10_080, weekly, 86_400_000),
            ],
            notes: vec!["2 full resets left".into()],
            at: now,
        }),
        error: None,
    }
}

#[test]
fn the_sidebar_ends_with_whats_left_of_the_accounts_limits() {
    let mut app = app();
    // Nothing said yet: no line.
    let screen = render(&app, 100, 12);
    assert!(!screen.contains('%'), "{screen}");
    // Each window with its pace: four fifths of five hours gone, and six
    // sevenths of a week; Claude's used faster, then about as fast, codex's
    // slower.
    app.limits.agents = vec![said("claude", 90.0, 85.0), said("codex", 7.0, 13.0)];
    let screen = render(&app, 100, 12);
    let last = screen.lines().nth(10).unwrap();
    assert!(
        last.starts_with(" claude 10%▲ 15%◆  codex 93%▼ 87%▼"),
        "{screen}"
    );
    // Too long for their names: their marks.
    app.limits.agents = vec![said("claude", 100.0, 0.0), said("codex", 0.0, 0.0)];
    let screen = render(&app, 100, 12);
    let last = screen.lines().nth(10).unwrap();
    assert!(last.starts_with(" ✱ 0%✕ 100%▼  ⌬ 100%▼ 100%▼"), "{screen}");
    // Known long ago: what was left then, without a pace.
    let mut old = said("claude", 90.0, 85.0);
    old.usage.as_mut().unwrap().at -= 60 * 60_000;
    app.limits.agents = vec![old];
    let screen = render(&app, 100, 12);
    let last = screen.lines().nth(10).unwrap();
    assert!(last.starts_with(" claude 10% 15% "), "{screen}");
    app.sidebar_usage = false;
    let screen = render(&app, 100, 12);
    assert!(!screen.contains('%'), "{screen}");
}

#[test]
fn the_sidebars_line_drops_whole_windows_rather_than_cut_one() {
    let mut app = app();
    app.limits.agents = vec![said("claude", 0.0, 0.0), said("codex", 0.0, 0.0)];
    let now = now_ms();
    let line = |width| usage_line(&app, width, now).map(|l| l.to_string());
    let named = " claude 100%▼ 100%▼  codex 100%▼ 100%▼";
    assert_eq!(line(named.width()).as_deref(), Some(named));
    // Marks for names, then each one's shortest window, then fewer agents.
    assert_eq!(
        line(named.width() - 1).as_deref(),
        Some(" ✱ 100%▼ 100%▼  ⌬ 100%▼ 100%▼")
    );
    assert_eq!(line(28).as_deref(), Some(" ✱ 100%▼  ⌬ 100%▼"));
    assert_eq!(line(16).as_deref(), Some(" ✱ 100%▼"));
    assert_eq!(line(7), None);
    // Whatever the width, nothing is cut short.
    for width in 0..60 {
        if let Some(text) = line(width) {
            assert!(text.width() <= width && !text.contains('…'), "{text}");
        }
    }
}

#[test]
fn the_usage_view_shows_each_window_and_why_an_agent_couldnt_say() {
    let mut app = app();
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(app.mode, Mode::Usage);
    // An agent asked once, which couldn't say this time.
    let mut codex = said("codex", 7.0, 13.0);
    codex.error = Some("codex's server didn't answer".into());
    app.limits.agents = vec![said("claude", 31.0, 52.0), codex];
    let screen = render(&app, 100, 20);
    for shown in [
        "roster  usage",
        "✱ Claude  pro",
        "5 hours  ",
        "Weekly   ",
        " 69% left  ▼ slow",
        " 48% left  ▼ slow",
        "resets ",
        ", in ",
        "2 full resets left",
        "⌬ Codex  pro",
        "can't tell now: codex's server didn't answer",
        "Enter ask again",
        "Esc back",
    ] {
        assert!(screen.contains(shown), "{shown:?} in\n{screen}");
    }
    assert!(!screen.contains("Needs input"), "{screen}");
    // Enter asks again.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.limits.due(false));
}

#[test]
fn with_account_limits_off_the_usage_view_says_how_to_ask() {
    let mut app = app();
    app.account_limits = false;
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    let screen = render(&app, 160, 20);
    assert!(
        screen.contains("account_limits is off") && !screen.contains("Asking the agents"),
        "{screen}"
    );
    assert!(!screen.contains("ask again"), "{screen}");
    // Enter says why it doesn't ask.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let screen = render(&app, 160, 20);
    assert!(
        screen.contains("Roster doesn't ask your accounts"),
        "{screen}"
    );
}

#[test]
fn the_usage_views_tabs_show_what_was_spent_in_each_period() {
    use crate::tokens::tests::{call, hours_of};
    use crate::tokens::{Period, Report};
    let mut app = app();
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    // Right of the limits, what was spent in the last day.
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(app.usage_period, Some(Period::Day));
    let screen = render(&app, 100, 30);
    assert!(
        screen.contains("Reading the agents' conversations…"),
        "{screen}"
    );
    let now = now_ms();
    let hours = hours_of(vec![
        (
            "claude",
            "a",
            vec![call(now - 1000, "claude-opus-5-5", 100, 1_000_000, 2_000)],
        ),
        (
            "pi",
            "b",
            vec![call(now - 2000, "vllm/qwen3", 50_000, 0, 500)],
        ),
    ]);
    app.tokens
        .summed(Period::Day, Report::of(&hours, now - 86_400_000, now, 24));
    let screen = render(&app, 100, 30);
    for shown in [
        "Limits  │  Tokens  24h  7 days",
        // $4 × 0.0001 + $0.20 × 1 + $20 × 0.002.
        "1.05M tokens · 2 sessions · $0.24 at API prices",
        "the cache saved $3.80",
        "■ input 50.1k  ■ cache read 1.00M  ■ cache write 0  ■ output 2.50k",
        "50.5k of them are of models without a known price",
        "✱ Claude  1 session    1.00M   95.2%      $0.24",
        "unpriced",
        "each bar is an hour, the tallest 1.00M",
        "Model",
        "claude-opus-5-5",
        "vllm/qwen3",
        "Enter read again   h/l tabs",
    ] {
        assert!(screen.contains(shown), "{shown:?} in\n{screen}");
    }
    // Enter reads again; back left, the limits.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.tokens.due(true));
    app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    assert_eq!(app.usage_period, None);
    let screen = render(&app, 100, 30);
    assert!(screen.contains("Enter ask again"), "{screen}");
}

#[test]
fn notes_are_boxed_side_by_side_or_stacked_with_a_line_between() {
    let note = |text: &str| crate::note::Note {
        text: text.into(),
        pin: None,
        at: 0,
    };
    let (project, session) = (note("backlog\nnext"), note("fix it"));
    let text = |lines: Vec<Line>| -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    };
    // Side by side: the project's left, the line between them meeting the
    // box; each line as wide as the preview.
    let both = text(note_bands(Some(&project), Some(&session), 65, 2));
    assert_eq!(both.len(), 4, "{both:?}");
    assert!(both[0].starts_with("╭──") && both[0].contains('┬') && both[0].ends_with('╮'));
    assert!(
        both[1].starts_with("│ ▤ backlog") && both[1].contains(" │ ✎ fix it"),
        "{both:?}"
    );
    assert!(
        both[2].starts_with("│   next") && both[2].ends_with(" │"),
        "{both:?}"
    );
    assert!(both[3].starts_with("╰──") && both[3].contains('┴') && both[3].ends_with('╯'));
    assert!(both.iter().all(|l| l.width() == 65), "{both:?}");
    let at = |l: &str, c: char| l.chars().position(|x| x == c);
    assert_eq!(at(&both[0], '┬'), at(&both[1].replacen('│', " ", 1), '│'));
    // Too narrow: stacked, the line between them across the box.
    let stacked = text(note_bands(Some(&project), Some(&session), 40, 2));
    assert_eq!(stacked.len(), 6, "{stacked:?}");
    assert_eq!(stacked[3], format!("├{}┤", "─".repeat(38)));
    assert!(stacked[4].starts_with("│ ✎ fix it"), "{stacked:?}");
    assert!(stacked.iter().all(|l| l.width() == 40), "{stacked:?}");
    // One alone: the whole width.
    let alone = text(note_bands(None, Some(&session), 65, 2));
    assert_eq!(alone.len(), 3);
    assert!(alone[1].starts_with("│ ✎ fix it") && alone[1].width() == 65);
    assert!(note_bands(None, None, 65, 2).is_empty());
    // As many lines of each as notes.lines says, the box as tall as the
    // longer.
    let three = text(note_bands(Some(&note("a\nb\nc\nd")), Some(&session), 65, 3));
    assert_eq!(three.len(), 5, "{three:?}");
    assert!(
        three[3].starts_with("│   c ") && three[3].contains("1–3 of 4"),
        "{three:?}"
    );
    let one = text(note_bands(Some(&project), Some(&session), 65, 1));
    assert_eq!(one.len(), 3, "{one:?}");
}

#[test]
fn a_projects_note_is_marked_after_its_name() {
    let mut app = app();
    let note = || {
        Some(crate::note::Note {
            text: "backlog".into(),
            pin: None,
            at: 0,
        })
    };
    let mut projects = app.projects.clone();
    projects[0].note = note();
    let mut sessions = app.sessions.clone();
    sessions[0].note = note();
    app.set_data(projects, sessions);
    // On shelves, each card's project line; the session's own by its age.
    let screen = render(&app, 120, 16);
    assert_eq!(screen.matches("RO roster ▤").count(), 2, "{screen}");
    assert!(screen.contains("✎ 2d"), "{screen}");
    // Grouped by project: its heading, the cards having no project line.
    app.set_grouped(true);
    let screen = render(&app, 120, 16);
    assert!(screen.contains("RO roster ▤ (2)"), "{screen}");
    assert_eq!(screen.matches('▤').count(), 2, "heading and box: {screen}");
    // In the projects view, in its column, the directory after it.
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    let screen = render(&app, 120, 16);
    assert!(screen.contains("RO roster ▤  /r"), "{screen}");
}
