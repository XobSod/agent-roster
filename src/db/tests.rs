use super::events::*;
use super::*;
use crate::model::tests::session;

fn project(name: &str) -> Project {
    Project {
        id: 0,
        name: name.into(),
        origin: "/tmp".into(),
        agent: None,
        model: None,
        effort: None,
        note: None,
    }
}

/// The schema as version 1 created it.
const V1_SCHEMA: &str = "
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
        origin TEXT NOT NULL, created_at INTEGER NOT NULL);
    CREATE TABLE sessions (id TEXT PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id),
        name TEXT NOT NULL, agent TEXT NOT NULL, workdir TEXT NOT NULL, conversation_id TEXT,
        transcript_path TEXT, state TEXT NOT NULL, unread INTEGER NOT NULL DEFAULT 0,
        exit_status INTEGER, last_event_at INTEGER, screen_hash TEXT, screen_changed_at INTEGER,
        created_at INTEGER NOT NULL, deleted_at INTEGER);
    CREATE UNIQUE INDEX sessions_live_name ON sessions(project_id, name) WHERE deleted_at IS NULL;
    CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
        kind TEXT NOT NULL, at INTEGER NOT NULL, payload TEXT);
    PRAGMA user_version = 1;";

fn db_with_session() -> (Db, Session) {
    let db = Db::open_in_memory().unwrap();
    let project = db.add_project(&project("demo"), 0).unwrap();
    let mut s = session("a1");
    s.project_id = project.id;
    db.insert_session(&s).unwrap();
    (db, s)
}

#[test]
fn a_files_calls_are_saved_in_place_of_what_was_and_a_copy_counts_once() {
    use crate::tokens::tests::call;
    let db = Db::open_in_memory().unwrap();
    let hour = 3_600_000;
    let calls = |input| {
        vec![
            call(hour, "claude-opus-5-5", input, 0, 0),
            call(5 * hour, "claude-opus-5-5", 1, 0, 0),
        ]
    };
    db.save_spent("/a.jsonl", "claude", "c1", 10, 100, &calls(5))
        .unwrap();
    // Read again, grown: in place of what was, not beside it (the call
    // with 5 changed, as Claude's reply does as it's written).
    let mut grown = calls(5);
    grown[0].counts.output = 9;
    db.save_spent("/a.jsonl", "claude", "c1", 20, 200, &grown)
        .unwrap();
    let files = db.spent_files().unwrap();
    assert_eq!(
        (files["/a.jsonl"].modified, files["/a.jsonl"].size),
        (20, 200)
    );
    let mut hours = db.spent_hours(0, 10 * hour).unwrap();
    hours.sort_by_key(|h| h.start);
    assert_eq!(hours.len(), 2);
    assert_eq!(
        (hours[0].agent.as_str(), hours[0].session.as_str()),
        ("claude", "c1")
    );
    assert_eq!(hours[0].spent.tokens.total(), 14);
    assert!(hours[0].spent.cost > 0.0);
    // A fork with a copy of the calls, and one of its own: the copies
    // count once, for the file they were saved from first.
    let mut fork = grown.clone();
    fork.push(call(6 * hour, "vllm/qwen3", 7, 0, 0));
    db.save_spent("/b.jsonl", "claude", "c2", 30, 300, &fork)
        .unwrap();
    let hours = db.spent_hours(0, 10 * hour).unwrap();
    let total: u64 = hours.iter().map(|h| h.spent.tokens.total()).sum();
    assert_eq!(total, 14 + 1 + 7);
    let own = hours.iter().find(|h| h.session == "c2").unwrap();
    assert_eq!((own.model.as_str(), own.spent.unpriced), ("vllm/qwen3", 7));
    // Only the calls from `from` to `to`.
    let hours = db.spent_hours(hour, 2 * hour).unwrap();
    assert_eq!(hours.len(), 1);
    // Forgotten (moved), it's gone, and its copies are no one's until
    // the fork is saved again.
    db.forget_spent("/a.jsonl").unwrap();
    assert!(!db.spent_files().unwrap().contains_key("/a.jsonl"));
    db.save_spent("/b.jsonl", "claude", "c2", 30, 300, &fork)
        .unwrap();
    let hours = db.spent_hours(0, 10 * hour).unwrap();
    assert!(hours.iter().all(|h| h.session == "c2"));
}

#[test]
fn what_was_spent_is_kept_by_number_from_schema_12_on() {
    // A database of schema 10, what was spent kept by its texts, with
    // the one marker of the version it was read with.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();
    for (_, migration) in MIGRATIONS.iter().filter(|(to, _)| *to <= 10) {
        if let Sql(sql) = migration {
            conn.execute_batch(sql).unwrap();
        }
    }
    let hour = 3_600_000;
    conn.execute_batch(&format!(
        "PRAGMA user_version = 10;
         INSERT INTO meta (key, value) VALUES ('spent:version', '4');
         INSERT INTO spent_files VALUES ('/a.jsonl', 'claude', 'c1', 10, 100);
         INSERT INTO spent_files VALUES ('/b.jsonl', 'pi', 'p1', 20, 200);
         INSERT INTO spent VALUES ('claude:m1', '/a.jsonl', 'claude-opus-5-5', {hour}, 5, 100, 0, 9, 0.5, 0.1);
         INSERT INTO spent VALUES ('claude:m2', '/a.jsonl', 'claude-opus-5-5', {}, 1, 0, 0, 0, 0.25, 0.0);
         INSERT INTO spent VALUES ('pi:x@t', '/b.jsonl', 'vllm/qwen3', {hour}, 7, 0, 0, 0, NULL, NULL);",
        hour + 60_000
    ))
    .unwrap();
    let db = Db::init(conn).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    // The same sums.
    let mut hours = db.spent_hours(0, 10 * hour).unwrap();
    hours.sort_by(|a, b| a.agent.cmp(&b.agent));
    let sums: Vec<(&str, &str, &str, u64, f64, u64)> = hours
        .iter()
        .map(|h| {
            let s = &h.spent;
            (
                h.agent.as_str(),
                h.session.as_str(),
                h.model.as_str(),
                s.tokens.total(),
                s.cost,
                s.unpriced,
            )
        })
        .collect();
    assert_eq!(
        sums,
        [
            ("claude", "c1", "claude-opus-5-5", 115, 0.75, 0),
            ("pi", "p1", "vllm/qwen3", 7, 0.0, 7),
        ]
    );
    // Each file with the version the marker said, the marker gone.
    let files = db.spent_files().unwrap();
    assert_eq!(
        (files["/a.jsonl"].version.as_str(), files["/b.jsonl"].size),
        ("4", 200)
    );
    assert_eq!(db.meta("spent:version").unwrap(), None);
    // A call copied into another file still counts once.
    db.save_spent(
        "/c.jsonl",
        "claude",
        "c2",
        30,
        300,
        &[crate::tokens::Call {
            id: "claude:m1".into(),
            at: hour,
            model: "claude-opus-5-5".into(),
            counts: crate::tokens::Counts {
                input: 5,
                ..Default::default()
            },
            ..Default::default()
        }],
    )
    .unwrap();
    let total: u64 = db
        .spent_hours(0, 10 * hour)
        .unwrap()
        .iter()
        .map(|h| h.spent.tokens.total())
        .sum();
    assert_eq!(total, 122);
    // Indexed as a new database is.
    let indexes = |conn: &Connection| -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT sql FROM sqlite_master WHERE tbl_name LIKE 'spent%' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .map(|sql| sql.unwrap().unwrap_or_default())
            .collect()
    };
    let new = Db::open_in_memory().unwrap();
    assert_eq!(indexes(&db.conn), indexes(&new.conn));
}

#[test]
fn calls_sharing_a_hash_as_theyre_indexed_keep_the_first() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SPENT_TABLES).unwrap();
    let tx = conn.transaction().unwrap();
    let add = |call: i64, input: i64| {
        tx.execute(
            "INSERT INTO spent VALUES (?1, 1, 1, 0, ?2, 0, 0, 0, NULL, NULL)",
            params![call, input],
        )
        .unwrap();
    };
    add(1, 10);
    add(2, 20);
    assert_eq!(index_calls_once(&tx).unwrap(), 0);
    tx.execute_batch("DROP INDEX spent_call").unwrap();
    add(1, 11);
    add(2, 21);
    add(2, 22);
    assert_eq!(index_calls_once(&tx).unwrap(), 3);
    let inputs: Vec<i64> = tx
        .prepare("SELECT input FROM spent ORDER BY call")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|i| i.unwrap())
        .collect();
    assert_eq!(inputs, [10, 20]);
    // Indexed: one more is refused.
    assert!(
        tx.execute(
            "INSERT INTO spent VALUES (1, 1, 1, 0, 0, 0, 0, 0, NULL, NULL)",
            []
        )
        .is_err()
    );
}

#[test]
fn free_room_left_behind_goes_back_to_the_disk_at_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.db");
    drop(Db::open(&path).unwrap());
    // A file mostly free, as a roster ended before giving back what the
    // migration left would leave it.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE filler (x BLOB);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000)
         INSERT INTO filler SELECT zeroblob(4000) FROM n;
         DROP TABLE filler;",
    )
    .unwrap();
    let free = |conn: &Connection| -> i64 {
        conn.pragma_query_value(None, "freelist_count", |r| r.get(0))
            .unwrap()
    };
    assert!(free(&conn) > 2000, "{}", free(&conn));
    drop(conn);
    // Another roster has it open meanwhile (the TUI, for days).
    let other = Connection::open(&path).unwrap();
    let _: i64 = other
        .query_row("SELECT count(*) FROM meta", [], |r| r.get(0))
        .unwrap();
    let db = Db::open(&path).unwrap();
    assert!(free(&db.conn) < 100, "{}", free(&db.conn));
    assert!(!db.mostly_free().unwrap());
    // The log of giving it back gave its room back too.
    let log = std::fs::metadata(dir.path().join("roster.db-wal")).map_or(0, |m| m.len());
    assert!(log < 1 << 20, "{log}");
    drop(other);
}

#[test]
fn rosters_making_the_database_at_once_both_open_it() {
    // Over and over, as they collide only now and then.
    for _ in 0..30 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roster.db");
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let opening: Vec<_> = (0..2)
            .map(|_| {
                let (path, start) = (path.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    Db::open(&path).map(drop)
                })
            })
            .collect();
        for open in opening {
            open.join().unwrap().unwrap();
        }
    }
}

#[test]
fn a_calls_id_is_kept_as_the_same_hash_everywhere() {
    // FNV-1a's, as any implementation of it gives.
    assert_eq!(call_key(""), 0xcbf2_9ce4_8422_2325_u64 as i64);
    assert_eq!(call_key("a"), 0xaf63_dc4c_8601_ec8c_u64 as i64);
    assert_ne!(call_key("claude:m1"), call_key("claude:m2"));
}

#[test]
fn migrates_a_version_1_database() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO projects (name, origin, created_at) VALUES ('p', '/p', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, project_id, name, agent, workdir, state, created_at)
         VALUES ('old', 1, 'n', 'claude', '/p', 'idle', 0)",
        [],
    )
    .unwrap();
    let db = Db::init(conn).unwrap();
    let s = db.session("old").unwrap();
    assert_eq!((s.state, s.baseline_at, s.title), (State::Idle, None, None));
    let p = db.project("p").unwrap().unwrap();
    assert_eq!((p.model, p.note), (None, None));
    let version: i64 = db
        .conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn an_upgraded_session_that_had_turns_has_begun_its_conversation() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();
    let tx = conn.transaction().unwrap();
    for (_, migration) in MIGRATIONS.iter().filter(|(to, _)| *to < 15) {
        match migration {
            Sql(sql) => tx.execute_batch(sql).unwrap(),
            Code(migrate) => migrate(&tx).unwrap(),
        }
    }
    tx.execute_batch(
        "PRAGMA user_version = 14;
         INSERT INTO projects (name, origin, created_at) VALUES ('p', '/p', 0);
         INSERT INTO sessions (id, project_id, name, agent, workdir, state, created_at, turns)
         VALUES ('used', 1, 'a', 'claude', '/p', 'stopped', 0, 3),
                ('new', 1, 'b', 'claude', '/p', 'stopped', 0, 0);",
    )
    .unwrap();
    tx.commit().unwrap();
    let db = Db::init(conn).unwrap();
    assert!(db.session("used").unwrap().conversation_begun);
    assert!(!db.session("new").unwrap().conversation_begun);
}

#[test]
fn a_save_of_what_changed_leaves_what_another_roster_saved() {
    let (db, s) = db_with_session();
    // A refresh loaded it; an edit saves a name and a title meanwhile.
    let loaded = db.session(&s.id).unwrap();
    let mut edited = db.session(&s.id).unwrap();
    edited.name = "renamed".into();
    edited.title_override = Some("New title".into());
    db.save_changes(&loaded, &edited).unwrap();
    // The refresh saves what it changed: the screen, the state.
    let mut refreshed = loaded.clone();
    refreshed.screen_hash = Some("h2".into());
    refreshed.set_state(State::Idle, 50);
    db.save_changes(&loaded, &refreshed).unwrap();
    let saved = db.session(&s.id).unwrap();
    assert_eq!(saved.name, "renamed");
    assert_eq!(saved.title_override.as_deref(), Some("New title"));
    assert_eq!(saved.screen_hash.as_deref(), Some("h2"));
    assert_eq!(saved.state, State::Idle);
    // Nothing changed, nothing written.
    db.save_changes(&saved, &saved).unwrap();
}

#[test]
fn a_save_of_what_changed_writes_every_column_save_session_does() {
    let (db, s) = db_with_session();
    let before = db.session(&s.id).unwrap();
    let mut after = before.clone();
    after.name = "n".into();
    after.conversation_id = Some("c".into());
    after.transcript_path = Some("/t".into());
    after.conversation_begun = true;
    after.state = State::NeedsInput;
    after.unread = true;
    after.exit_status = Some(3);
    after.last_event_at = Some(1);
    after.screen_hash = Some("h".into());
    after.screen_changed_at = Some(2);
    after.deleted_at = Some(3);
    after.baseline_at = Some(4);
    after.title = Some("t".into());
    after.model = Some("m".into());
    after.effort = Some("e".into());
    after.state_since = Some(5);
    after.pending_prompt = Some("p".into());
    after.started_at = Some(12);
    after.session_at = Some(13);
    after.running_model = Some("rm".into());
    after.running_effort = Some("re".into());
    after.title_override = Some("to".into());
    after.turns = 6;
    after.worked_ms = 7;
    after.fork_from = Some("f".into());
    after.asked_by = Some("a1".into());
    after.given_prompt = Some("g".into());
    after.subagents = [(
        "a2".to_string(),
        crate::model::Subagent {
            kind: Some("Explore".into()),
            started: 8,
            heard: 9,
        },
    )]
    .into();
    after.shells = [(
        "b1".to_string(),
        crate::model::Shell {
            what: Some("Run the tests".into()),
            started: 10,
            from: 11,
        },
    )]
    .into();
    after.awaiting = true;
    db.save_changes(&before, &after).unwrap();
    assert_eq!(db.session(&s.id).unwrap(), after);
}

#[test]
fn shells_their_transcript_says_have_ended_end() {
    let (mut db, mut s) = db_with_session();
    let shell = crate::model::Shell {
        what: None,
        started: 1,
        from: 10,
    };
    s.state = crate::model::State::Working;
    s.state_since = Some(1);
    s.last_event_at = Some(2);
    s.awaiting = true;
    s.shells = [("b1".to_string(), shell.clone()), ("b2".to_string(), shell)].into();
    db.save_session(&s).unwrap();
    // Read on with no end in it: how far, kept.
    db.settle_shells(&mut |_| (Vec::new(), Some(500))).unwrap();
    let after = db.session(&s.id).unwrap();
    assert!(after.shells.values().all(|shell| shell.from == 500));
    db.settle_shells(&mut |_| (vec![("b1".into(), 30)], None))
        .unwrap();
    let after = db.session(&s.id).unwrap();
    assert_eq!(after.shells.keys().collect::<Vec<_>>(), ["b2"]);
    assert_eq!(after.state, crate::model::State::Working);
    db.settle_shells(&mut |_| (vec![("b2".into(), 40)], None))
        .unwrap();
    let after = db.session(&s.id).unwrap();
    assert!(after.shells.is_empty());
    assert_eq!(
        (after.state, after.unread, after.state_since),
        (crate::model::State::Idle, true, Some(40))
    );
    // None left running: not asked.
    db.settle_shells(&mut |_| panic!("asked of a session with no shells"))
        .unwrap();
}

#[test]
fn subagents_their_agent_says_are_finished_are_stopped() {
    let (mut db, mut s) = db_with_session();
    let now = crate::util::now_ms();
    let at = |heard| crate::model::Subagent {
        kind: None,
        started: heard,
        heard,
    };
    s.state = crate::model::State::Working;
    s.awaiting = true;
    s.subagents = [("a".to_string(), at(now)), ("b".to_string(), at(now))].into();
    db.save_session(&s).unwrap();
    let only =
        |done: &'static [&'static str]| move |_: &Session, id: &str, _: i64| done.contains(&id);
    db.settle_subagents(now, &only(&[])).unwrap();
    assert_eq!(db.session(&s.id).unwrap().subagents.len(), 2);
    db.settle_subagents(now, &only(&["a"])).unwrap();
    let after = db.session(&s.id).unwrap();
    assert_eq!(after.subagents.keys().collect::<Vec<_>>(), ["b"]);
    assert_eq!(after.state, crate::model::State::Working);
    db.settle_subagents(now, &only(&["b"])).unwrap();
    let after = db.session(&s.id).unwrap();
    assert!(after.subagents.is_empty());
    assert_eq!(
        (after.state, after.unread),
        (crate::model::State::Idle, true)
    );
}

#[test]
fn subagents_kept_as_an_older_build_kept_them_are_read() {
    let old = subagents_from(r#"{"b1":1000}"#);
    assert_eq!(
        (old["b1"].started, old["b1"].heard, old["b1"].kind.clone()),
        (1000, 1000, None)
    );
    assert!(subagents_from("not json").is_empty());
    let new = subagents_text(&old).unwrap();
    assert_eq!(subagents_from(&new), old);
}

#[test]
fn a_title_made_is_kept_only_if_nothing_changed_meanwhile() {
    let (db, mut s) = db_with_session();
    s.conversation_id = Some("c1".into());
    db.save_session(&s).unwrap();
    assert_eq!(
        db.set_title_if(&s.id, "Made", None, Some("c1")).unwrap(),
        Some(true)
    );
    assert_eq!(
        db.session(&s.id).unwrap().title_override.as_deref(),
        Some("Made")
    );
    // Given a title, or on another conversation, since: not kept.
    assert_eq!(
        db.set_title_if(&s.id, "Again", None, Some("c1")).unwrap(),
        Some(false)
    );
    assert_eq!(
        db.set_title_if(&s.id, "Again", Some("Made"), Some("c2"))
            .unwrap(),
        Some(false)
    );
    assert_eq!(
        db.session(&s.id).unwrap().title_override.as_deref(),
        Some("Made")
    );
    assert_eq!(db.set_title_if("nobody", "x", None, None).unwrap(), None);
}

#[test]
fn a_first_prompt_not_typed_in_is_put_back() {
    let (mut db, mut s) = db_with_session();
    s.pending_prompt = Some("fix it".into());
    db.save_session(&s).unwrap();
    let taken = db.take_pending_prompt(&s.id).unwrap().unwrap();
    db.restore_pending_prompt(&s.id, &taken).unwrap();
    assert_eq!(
        db.session(&s.id).unwrap().pending_prompt.as_deref(),
        Some("fix it")
    );
    // Not over one given meanwhile.
    db.take_pending_prompt(&s.id).unwrap();
    s.pending_prompt = Some("newer".into());
    db.save_session(&s).unwrap();
    db.restore_pending_prompt(&s.id, "fix it").unwrap();
    assert_eq!(
        db.session(&s.id).unwrap().pending_prompt.as_deref(),
        Some("newer")
    );
}

#[test]
fn a_note_is_saved_apart_from_the_rest() {
    let (db, s) = db_with_session();
    let note = Note {
        text: "TODO\n- one".into(),
        pin: Some(1),
        at: 7,
    };
    let whose = Whose::Session(s.id.clone());
    db.set_note(&whose, Some(&note)).unwrap();
    assert_eq!(db.session(&s.id).unwrap().note, Some(note.clone()));
    // A save of a copy read before (refresh's) leaves it.
    db.save_session(&s).unwrap();
    assert_eq!(db.session(&s.id).unwrap().note, Some(note.clone()));
    // Only over what's expected there.
    let newer = Note {
        text: "newer".into(),
        pin: None,
        at: 8,
    };
    assert!(!db.set_note_if(&whose, None, Some(&newer)).unwrap());
    assert!(db.set_note_if(&whose, Some(&note), Some(&newer)).unwrap());
    assert_eq!(db.session(&s.id).unwrap().note, Some(newer.clone()));
    let nobody = Whose::Session("nobody".into());
    assert!(db.set_note_if(&nobody, None, None).is_err());
    db.set_note(&whose, None).unwrap();
    assert_eq!(db.session(&s.id).unwrap().note, None);
    assert!(db.set_note(&nobody, None).is_err());

    // A project's, the same way; an edit of the project, as it was read
    // before, leaves it.
    let p = db.project(&s.project).unwrap().unwrap();
    let whose = Whose::Project(p.id);
    assert!(db.set_note_if(&whose, None, Some(&note)).unwrap());
    assert!(!db.set_note_if(&whose, None, Some(&newer)).unwrap());
    let edited = db.change_project(p.id, |_| Ok(p.clone())).unwrap();
    assert_eq!(edited.note, Some(note.clone()));
    let p = db.project(&s.project).unwrap().unwrap();
    assert_eq!(p.note, Some(note));
    assert_eq!(db.session(&s.id).unwrap().note, None);
    assert!(db.set_note_if(&Whose::Project(99), None, None).is_err());
    assert!(db.set_note(&Whose::Project(99), None).is_err());
}

#[test]
fn a_removed_projects_id_and_note_go_to_no_other() {
    let mut db = Db::open_in_memory().unwrap();
    db.add_project(&project("first"), 0).unwrap();
    let old = db.add_project(&project("old"), 0).unwrap();
    db.remove_project(&old).unwrap();
    // The next has an id of its own, though the removed one was highest.
    let new = db.add_project(&project("new"), 0).unwrap();
    assert!(new.id > old.id);
    // A note opened on the removed one isn't saved, forced or not.
    let note = Note {
        text: "for old".into(),
        pin: None,
        at: 1,
    };
    let old = Whose::Project(old.id);
    assert!(db.set_note_if(&old, None, Some(&note)).is_err());
    assert!(db.set_note(&old, Some(&note)).is_err());
    assert_eq!(db.project("new").unwrap().unwrap().note, None);
}

#[test]
fn an_upgraded_databases_project_ids_arent_given_again() {
    // From before the highest id given was kept: none recorded.
    let older = |meta: &str| {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO projects (name, origin, created_at) VALUES ('first', '/f', 0);
             INSERT INTO projects (name, origin, created_at) VALUES ('old', '/o', 0);
             {meta}"
        ))
        .unwrap();
        Db::init(conn).unwrap()
    };
    let mut db = older("");
    let old = db.project("old").unwrap().unwrap();
    // Its highest removed before any is added: the next isn't given it.
    db.remove_project(&old).unwrap();
    let new = db.add_project(&project("new"), 0).unwrap();
    assert_eq!(new.id, old.id + 1);
    let note = Note {
        text: "for old".into(),
        pin: None,
        at: 1,
    };
    let old = Whose::Project(old.id);
    assert!(db.set_note_if(&old, None, Some(&note)).is_err());
    assert!(db.set_note(&old, Some(&note)).is_err());
    assert_eq!(db.project("new").unwrap().unwrap().note, None);
    // A higher one recorded is kept.
    let db = older("INSERT INTO meta (key, value) VALUES ('last_project_id', '10');");
    assert_eq!(db.add_project(&project("new"), 0).unwrap().id, 11);
}

#[test]
fn a_project_is_changed_as_it_is_now() {
    let db = Db::open_in_memory().unwrap();
    let mut p = project("demo");
    p.model = Some("opus".into());
    p.effort = Some("high".into());
    let p = db.add_project(&p, 0).unwrap();
    assert_eq!(
        db.project("demo").unwrap().unwrap().model.as_deref(),
        Some("opus")
    );
    // Its effort changed by another since `p` was read: kept.
    db.change_project(p.id, |now| {
        Ok(Project {
            effort: Some("low".into()),
            ..now.clone()
        })
    })
    .unwrap();
    let rename = |now: &Project| {
        Ok(Project {
            name: "renamed".into(),
            model: None,
            ..now.clone()
        })
    };
    let changed = db.change_project(p.id, rename).unwrap();
    let saved = db.project("renamed").unwrap().unwrap();
    assert_eq!((saved.model, saved.effort.as_deref()), (None, Some("low")));
    assert_eq!(changed.effort.as_deref(), Some("low"));
    // A name taken, or the project gone: nothing changed.
    db.add_project(&project("other"), 0).unwrap();
    let taken = db.change_project(p.id, |now| {
        Ok(Project {
            name: "other".into(),
            ..now.clone()
        })
    });
    assert_eq!(
        taken.unwrap_err().to_string(),
        "project \"other\" already exists"
    );
    assert!(db.project("renamed").unwrap().is_some());
    let gone = db.change_project(99, |now| Ok(now.clone()));
    assert_eq!(
        gone.unwrap_err().to_string(),
        "the project was removed meanwhile"
    );
}

#[test]
fn folds_events_once() {
    let (mut db, s) = db_with_session();
    db.insert_event(&s.id, "working", 10, None).unwrap();
    db.insert_event(&s.id, "idle", 20, Some(r#"{"session_id":"conv"}"#))
        .unwrap();
    db.insert_event("unknown-session", "idle", 30, None)
        .unwrap();
    assert_eq!(db.fold_events(100).unwrap(), 3);
    let folded = db.session(&s.id).unwrap();
    assert_eq!((folded.state, folded.unread), (State::Idle, true));
    assert_eq!(folded.conversation_id.as_deref(), Some("conv"));

    // Nothing new: a second fold changes nothing, even after the user read it.
    let mut read = folded.clone();
    read.unread = false;
    db.save_session(&read).unwrap();
    assert_eq!(db.fold_events(200).unwrap(), 0);
    assert!(!db.session(&s.id).unwrap().unread);
}

#[test]
fn prompts_count_as_working_and_are_found_later() {
    let (mut db, s) = db_with_session();
    db.insert_event(&s.id, "prompt", 10, None).unwrap();
    db.fold_events(100).unwrap();
    assert_eq!(db.session(&s.id).unwrap().state, State::Working);
    // Since when you attached: one at 10, none after it.
    assert!(db.prompted_since(&s.id, 5).unwrap());
    assert!(!db.prompted_since(&s.id, 11).unwrap());
    // Working alone (a tool, say) isn't a prompt.
    db.insert_event(&s.id, "working", 20, None).unwrap();
    assert!(!db.prompted_since(&s.id, 15).unwrap());
    assert!(!db.prompted_since("other", 0).unwrap());
}

#[test]
fn learns_what_model_aliases_turn_out_to_be() {
    let (mut db, s) = db_with_session();
    let mut opus = db.session(&s.id).unwrap();
    opus.model = Some("opus".into());
    db.save_session(&opus).unwrap();
    let started = r#"{"model":"claude-opus-5-5","source":"startup"}"#;
    db.insert_event(&s.id, "session", 10, Some(started))
        .unwrap();
    db.fold_events(100).unwrap();
    assert_eq!(db.model_names().unwrap()["opus"], "Opus 5.5");
    assert_eq!(
        db.session(&s.id).unwrap().running_model.as_deref(),
        Some("claude-opus-5-5")
    );
}

#[test]
fn learns_other_agents_models_apart_and_the_windows_they_report() {
    let (mut db, claude) = db_with_session();
    let mut pi = session("b1");
    pi.project_id = claude.project_id;
    pi.agent = "pi".into();
    db.insert_session(&pi).unwrap();
    let started = r#"{"model":"claude-opus-4-8","context_window":1000000,"started":true}"#;
    db.insert_event(&pi.id, "session", 10, Some(started))
        .unwrap();
    db.insert_event(
        &claude.id,
        "session",
        10,
        Some(r#"{"model":"claude-opus-5-5","source":"startup"}"#),
    )
    .unwrap();
    // A model switched to later is what it runs, not what "default" means.
    db.insert_event(&pi.id, "info", 20, Some(r#"{"model":"claude-haiku-4-5"}"#))
        .unwrap();
    db.fold_events(100).unwrap();
    let names = db.model_names().unwrap();
    assert_eq!(
        (names["default"].as_str(), names["pi:default"].as_str()),
        ("Opus 5.5", "Opus 4.8")
    );
    assert_eq!(
        db.session(&pi.id).unwrap().running_model.as_deref(),
        Some("claude-haiku-4-5")
    );
    assert_eq!(
        db.learned_window("claude-opus-4-8").unwrap(),
        Some(1_000_000)
    );
    assert_eq!(db.learned_window("claude-haiku-4-5").unwrap(), None);
}

#[test]
fn learns_codexs_models_from_its_start_only() {
    let (mut db, claude) = db_with_session();
    let mut s = session("c1");
    s.project_id = claude.project_id;
    s.agent = "codex".into();
    db.insert_session(&s).unwrap();
    // As codex 0.164 reports them: a resume and a clear in it first.
    for (at, source) in [(10, "resume"), (11, "clear")] {
        let payload =
            format!(r#"{{"session_id":"01a1","model":"gpt-6.1-sol","source":"{source}"}}"#);
        db.insert_event(&s.id, "session", at, Some(&payload))
            .unwrap();
    }
    db.fold_events(100).unwrap();
    assert!(!db.model_names().unwrap().contains_key("codex:default"));
    let started = r#"{"session_id":"01a2","model":"gpt-6.1-sol","source":"startup"}"#;
    db.insert_event(&s.id, "session", 20, Some(started))
        .unwrap();
    db.fold_events(100).unwrap();
    assert!(db.model_names().unwrap().contains_key("codex:default"));
}

#[test]
fn backfills_models_from_events_processed_earlier() {
    let (mut db, s) = db_with_session();
    db.insert_event(
        &s.id,
        "session",
        10,
        Some(r#"{"model":"claude-opus-5-5[1m]","source":"startup"}"#),
    )
    .unwrap();
    db.fold_events(100).unwrap();
    assert_eq!(db.model_names().unwrap()["default"], "Opus 5.5 (1M)");
    // As if an older roster processed it: the model wasn't kept.
    let mut lost = db.session(&s.id).unwrap();
    lost.running_model = None;
    db.save_session(&lost).unwrap();
    assert_eq!(db.backfill_models_from_events().unwrap(), 1);
    assert_eq!(
        db.session(&s.id).unwrap().running_model.as_deref(),
        Some("claude-opus-5-5[1m]")
    );
}

#[test]
fn models_are_learned_from_starts_with_what_they_asked_for() {
    let (mut db, mut s) = db_with_session();
    s.model = Some("opus".into());
    db.save_session(&s).unwrap();
    let report = |db: &mut Db, kind: &str, at: i64, payload: &str| {
        db.insert_event(&s.id, kind, at, Some(payload)).unwrap();
        db.fold_events(at + 1).unwrap();
    };
    report(
        &mut db,
        "session",
        10,
        r#"{"model":"claude-opus-5-5","source":"startup"}"#,
    );
    assert_eq!(db.model_names().unwrap()["opus"], "Opus 5.5");
    // Edited since, what it reports later isn't what the new one means.
    let mut edited = db.session(&s.id).unwrap();
    edited.model = Some("haiku".into());
    db.save_session(&edited).unwrap();
    report(&mut db, "idle", 20, r#"{"model":"claude-opus-5-5"}"#);
    // Nor what a conversation cleared or compacted in the agent reports
    // (after a switch inside it, say).
    for (at, source) in [(30, "clear"), (31, "compact")] {
        let payload = format!(r#"{{"model":"claude-sonnet-5-5","source":"{source}"}}"#);
        report(&mut db, "session", at, &payload);
    }
    assert!(!db.model_names().unwrap().contains_key("haiku"));
    // Its next start does.
    report(
        &mut db,
        "session",
        40,
        r#"{"model":"claude-haiku-5-5","source":"startup"}"#,
    );
    let names = db.model_names().unwrap();
    assert_eq!(
        (names["haiku"].as_str(), names["opus"].as_str()),
        ("Haiku 5.5", "Opus 5.5")
    );
}

#[test]
fn a_project_isnt_removed_with_a_session_restored_or_started_meanwhile() {
    let restored = "UPDATE sessions SET deleted_at = NULL WHERE id = 'a1'";
    let started = "INSERT INTO sessions (id, project_id, name, agent, workdir, state, created_at)
         VALUES ('b1', 1, 'new', 'claude', '/tmp', 'working', 2)";
    for meanwhile in [restored, started] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roster.db");
        let mut db = Db::open(&path).unwrap();
        let demo = db.add_project(&project("demo"), 0).unwrap();
        let mut s = session("a1");
        s.project_id = demo.id;
        db.insert_session(&s).unwrap();
        s.deleted_at = Some(1);
        db.save_session(&s).unwrap();
        db.insert_event(&s.id, "idle", 1, None).unwrap();
        // Asked to confirm: only an archived session goes with it.
        assert_eq!(db.purged_with(&demo).unwrap().len(), 1);
        // Meanwhile another roster writes, committing a moment later.
        let other = Connection::open(&path).unwrap();
        other
            .execute_batch(&format!("BEGIN IMMEDIATE; {meanwhile}"))
            .unwrap();
        let committing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            other.execute_batch("COMMIT").unwrap();
        });
        let removed = db.remove_project(&demo);
        committing.join().unwrap();
        assert!(removed.is_err(), "{meanwhile}: {removed:?}");
        assert!(db.project("demo").unwrap().is_some(), "{meanwhile}");
        assert!(db.session("a1").is_ok(), "{meanwhile}");
        assert_eq!(db.events_after(0, Some("a1"), 10).unwrap().len(), 1);
    }
}

#[test]
fn the_file_gives_back_what_pruning_frees() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.db");
    // A file from before: no auto-vacuum.
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (x)")
        .unwrap();
    let mut db = Db::open(&path).unwrap();
    let mode: i64 = db
        .conn
        .pragma_query_value(None, "auto_vacuum", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, AUTO_VACUUM_INCREMENTAL);
    let project = db.add_project(&project("demo"), 0).unwrap();
    let mut s = session("a1");
    s.project_id = project.id;
    db.insert_session(&s).unwrap();
    let big = format!("{{\"x\":\"{}\"}}", "y".repeat(100_000));
    for at in 0..20 {
        db.insert_event(&s.id, "working", at, Some(&big)).unwrap();
    }
    db.fold_events(0).unwrap();
    let pages = |db: &Db| -> i64 {
        db.conn
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap()
    };
    let before = pages(&db);
    // A day later the events are pruned, and the file shrinks.
    db.insert_event(&s.id, "idle", EVENT_RETENTION_MS * 2, None)
        .unwrap();
    db.fold_events(EVENT_RETENTION_MS * 2).unwrap();
    let free: i64 = db
        .conn
        .pragma_query_value(None, "freelist_count", |r| r.get(0))
        .unwrap();
    assert!(pages(&db) < before / 10, "{} of {before}", pages(&db));
    assert_eq!(free, 0);
}

#[test]
fn prunes_old_applied_events_only() {
    let (mut db, s) = db_with_session();
    db.insert_event(&s.id, "working", 0, None).unwrap();
    db.insert_event(&s.id, "idle", EVENT_RETENTION_MS + 50, None)
        .unwrap();
    db.fold_events(EVENT_RETENTION_MS + 100).unwrap();
    let left = db.events_after(0, None, 10).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].kind, "idle");
}

#[test]
fn live_session_names_are_unique_per_project() {
    let (db, s) = db_with_session();
    let mut twin = session("a2");
    twin.project_id = s.project_id;
    twin.name = s.name.clone();
    assert!(db.insert_session(&twin).is_err());

    // Once the first is deleted, the name is free again.
    let mut deleted = db.session(&s.id).unwrap();
    deleted.deleted_at = Some(1);
    db.save_session(&deleted).unwrap();
    db.insert_session(&twin).unwrap();
}

#[test]
fn project_removal_refuses_live_sessions() {
    let (mut db, s) = db_with_session();
    let project = db.project("demo").unwrap().unwrap();
    assert!(db.remove_project(&project).is_err());
    let mut deleted = db.session(&s.id).unwrap();
    deleted.deleted_at = Some(1);
    db.save_session(&deleted).unwrap();
    assert_eq!(db.remove_project(&project).unwrap().len(), 1);
    assert!(db.project("demo").unwrap().is_none());
}

/// As a sandbox keeps an agent's command from writing outside its folder
/// (codex's, running `roster note`): the database there, but its folder not
/// to be written, so no `-wal` beside it.
#[test]
fn a_database_that_cant_be_written_says_where_and_why_it_may_be() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roster.db");
    drop(Db::open(&path).unwrap());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let refused = |inside| {
        let conn = Connection::open(&path).unwrap();
        let e = Db::init(conn)
            .err()
            .expect("written in a folder that can't be");
        format!("{:#}", super::unwritable(e, &path, inside))
    };
    let (inside, outside) = (refused(true), refused(false));
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let place = path.display().to_string();
    assert!(
        inside.starts_with(&format!(
            "can't write {place} from inside an agent, whose sandbox"
        )),
        "{inside}"
    );
    assert!(
        outside.starts_with(&format!("can't write {place}: ")),
        "{outside}"
    );
    // Any other error as it is.
    let e = super::unwritable(anyhow::anyhow!("from a newer roster"), &path, true);
    assert_eq!(e.to_string(), "from a newer roster");
}
