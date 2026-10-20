//! The SQLite state database: projects, sessions and reported
//! events, in WAL mode so hooks can insert events while the CLI or TUI reads.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::model::{Event, Project, Session, State};

const SCHEMA_VERSION: i64 = 6;

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    origin     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    agent      TEXT,
    model      TEXT,
    effort     TEXT
);
CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    project_id        INTEGER NOT NULL REFERENCES projects(id),
    name              TEXT NOT NULL,
    agent             TEXT NOT NULL,
    workdir           TEXT NOT NULL,
    conversation_id   TEXT,
    transcript_path   TEXT,
    state             TEXT NOT NULL,
    unread            INTEGER NOT NULL DEFAULT 0,
    exit_status       INTEGER,
    last_event_at     INTEGER,
    screen_hash       TEXT,
    screen_changed_at INTEGER,
    created_at        INTEGER NOT NULL,
    deleted_at        INTEGER,
    baseline_at       INTEGER,
    title             TEXT,
    model             TEXT,
    effort            TEXT,
    state_since       INTEGER,
    pending_prompt    TEXT,
    running_model     TEXT,
    running_effort    TEXT,
    title_override    TEXT,
    turns             INTEGER NOT NULL DEFAULT 0,
    worked_ms         INTEGER NOT NULL DEFAULT 0,
    fork_from         TEXT
);
CREATE UNIQUE INDEX sessions_live_name ON sessions(project_id, name) WHERE deleted_at IS NULL;
CREATE TABLE events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    kind       TEXT NOT NULL,
    at         INTEGER NOT NULL,
    payload    TEXT
);
";

const SESSION_COLUMNS: &str =
    "s.id, s.project_id, p.name, s.name, s.agent, s.workdir, s.conversation_id,
    s.transcript_path, s.state, s.unread, s.exit_status, s.last_event_at, s.screen_hash,
    s.screen_changed_at, s.created_at, s.deleted_at, s.baseline_at, s.title, s.model, s.effort,
    s.state_since, s.pending_prompt, s.running_model, s.running_effort, s.title_override, s.turns,
    s.worked_ms, s.fork_from";

const PROJECT_COLUMNS: &str = "id, name, origin, agent, model, effort";

/// What each schema version adds to the one before.
const MIGRATIONS: &[(i64, &str)] = &[
    (2, "ALTER TABLE sessions ADD COLUMN baseline_at INTEGER;"),
    (
        3,
        "ALTER TABLE projects ADD COLUMN agent TEXT;
         ALTER TABLE projects ADD COLUMN model TEXT;
         ALTER TABLE projects ADD COLUMN effort TEXT;
         ALTER TABLE sessions ADD COLUMN title TEXT;
         ALTER TABLE sessions ADD COLUMN model TEXT;
         ALTER TABLE sessions ADD COLUMN effort TEXT;
         ALTER TABLE sessions ADD COLUMN state_since INTEGER;
         ALTER TABLE sessions ADD COLUMN pending_prompt TEXT;",
    ),
    (
        4,
        "ALTER TABLE sessions ADD COLUMN running_model TEXT;
         ALTER TABLE sessions ADD COLUMN running_effort TEXT;",
    ),
    (
        5,
        "ALTER TABLE sessions ADD COLUMN title_override TEXT;
         ALTER TABLE sessions ADD COLUMN turns INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE sessions ADD COLUMN worked_ms INTEGER NOT NULL DEFAULT 0;",
    ),
    (6, "ALTER TABLE sessions ADD COLUMN fork_from TEXT;"),
];

/// Applied events older than this are deleted.
const EVENT_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn =
            Connection::open(path).with_context(|| format!("can't open {}", path.display()))?;
        Db::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Db> {
        Db::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => conn.execute_batch(SCHEMA)?,
            v if v > SCHEMA_VERSION => {
                bail!("the database is from a newer roster (schema {v}); upgrade roster")
            }
            v => {
                for (_, sql) in MIGRATIONS.iter().filter(|(to, _)| *to > v) {
                    conn.execute_batch(sql)?;
                }
            }
        }
        if version != SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Db { conn })
    }

    // Projects

    /// Inserts `project` (its id is ignored) and returns it with its new id.
    pub fn add_project(&self, project: &Project, now: i64) -> Result<Project> {
        let name = &project.name;
        let origin_str = path_str(&project.origin)?;
        self.conn
            .execute(
                "INSERT INTO projects (name, origin, created_at, agent, model, effort)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    name,
                    origin_str,
                    now,
                    project.agent,
                    project.model,
                    project.effort
                ],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    anyhow::anyhow!("project {name:?} already exists")
                }
                e => e.into(),
            })?;
        Ok(Project {
            id: self.conn.last_insert_rowid(),
            ..project.clone()
        })
    }

    pub fn save_project(&self, p: &Project) -> Result<()> {
        self.conn
            .execute(
                "UPDATE projects SET name = ?2, origin = ?3, agent = ?4, model = ?5, effort = ?6
                 WHERE id = ?1",
                params![
                    p.id,
                    p.name,
                    path_str(&p.origin)?,
                    p.agent,
                    p.model,
                    p.effort
                ],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    anyhow::anyhow!("project {:?} already exists", p.name)
                }
                e => e.into(),
            })?;
        Ok(())
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects ORDER BY name"
        ))?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn project(&self, name: &str) -> Result<Option<Project>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE name = ?1"),
                [name],
                project_from_row,
            )
            .optional()?)
    }

    /// Removes a project and its archived sessions. Returns the purged sessions.
    pub fn remove_project(&mut self, project: &Project) -> Result<Vec<Session>> {
        let sessions: Vec<Session> = self
            .sessions(true)?
            .into_iter()
            .filter(|s| s.project_id == project.id)
            .collect();
        if let Some(live) = sessions.iter().find(|s| s.deleted_at.is_none()) {
            bail!(
                "project {:?} still has session {:?}; archive its sessions first",
                project.name,
                live.name
            );
        }
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sessions WHERE project_id = ?1", [project.id])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", [project.id])?;
        tx.commit()?;
        Ok(sessions)
    }

    // Sessions

    pub fn insert_session(&self, s: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, project_id, name, agent, workdir, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                s.id,
                s.project_id,
                s.name,
                s.agent,
                path_str(&s.workdir)?,
                s.state.as_str(),
                s.created_at
            ],
        )?;
        save_session(&self.conn, s)
    }

    /// Removes an archived session and its events for good.
    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM events WHERE session_id = ?1", [id])?;
        let n = tx.execute(
            "DELETE FROM sessions WHERE id = ?1 AND deleted_at IS NOT NULL",
            [id],
        )?;
        if n == 0 {
            bail!("no archived session {id}");
        }
        tx.commit()?;
        Ok(())
    }

    /// All sessions, oldest first within each project; archived ones (`deleted_at`
    /// set: the column keeps its first name) only if asked.
    pub fn sessions(&self, include_archived: bool) -> Result<Vec<Session>> {
        let filter = if include_archived {
            ""
        } else {
            "WHERE s.deleted_at IS NULL"
        };
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions s JOIN projects p ON p.id = s.project_id {filter}
             ORDER BY p.name, s.created_at, s.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn session(&self, id: &str) -> Result<Session> {
        load_session(&self.conn, id)?.with_context(|| format!("no session with id {id}"))
    }

    pub fn save_session(&self, s: &Session) -> Result<()> {
        save_session(&self.conn, s)
    }

    /// Takes a session's pending first prompt, so that only one process types it.
    pub fn take_pending_prompt(&mut self, id: &str) -> Result<Option<String>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prompt: Option<String> = tx
            .query_row(
                "SELECT pending_prompt FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if prompt.is_some() {
            tx.execute(
                "UPDATE sessions SET pending_prompt = NULL WHERE id = ?1",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(prompt)
    }

    // Events

    /// Applies events reported since the last fold, in one transaction, so any
    /// number of roster processes can fold concurrently. Returns how many were applied.
    pub fn fold_events(&mut self, now: i64) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let applied: i64 = tx
            .query_row(
                "SELECT value FROM meta WHERE key = 'applied_event'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let events = events_after(&tx, applied, None, i64::MAX)?;
        let Some(last) = events.last().map(|e| e.id) else {
            return Ok(0);
        };
        let mut touched: HashMap<String, Session> = HashMap::new();
        for e in &events {
            if !touched.contains_key(&e.session_id) {
                match load_session(&tx, &e.session_id)? {
                    Some(s) => touched.insert(e.session_id.clone(), s),
                    None => continue,
                };
            }
            if let Some(s) = touched.get_mut(&e.session_id) {
                s.apply(e);
                // Learn what the model asked for (or the default) turned out to be.
                let model = e
                    .payload
                    .as_deref()
                    .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
                if let Some(model) = model.as_ref().and_then(|v| v.get("model")?.as_str()) {
                    let asked = s.model.clone().unwrap_or_else(|| "default".into());
                    tx.execute(
                        "INSERT INTO meta (key, value) VALUES (?1, ?2)
                         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                        [format!("model:{asked}"), model.to_string()],
                    )?;
                }
            }
        }
        for s in touched.values() {
            save_session(&tx, s)?;
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('applied_event', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [last.to_string()],
        )?;
        tx.execute(
            "DELETE FROM events WHERE id <= ?1 AND at < ?2",
            params![last, now - EVENT_RETENTION_MS],
        )?;
        tx.commit()?;
        Ok(events.len())
    }

    /// Events with id > `after`, oldest first, optionally for one session.
    pub fn events_after(
        &self,
        after: i64,
        session: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Event>> {
        events_after(&self.conn, after, session, limit)
    }

    /// Reported events not applied yet, and all events kept.
    pub fn event_counts(&self) -> Result<(i64, i64)> {
        let applied: i64 = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'applied_event'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let waiting = self.conn.query_row(
            "SELECT count(*) FROM events WHERE id > ?1",
            [applied],
            |r| r.get(0),
        )?;
        let kept = self
            .conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        Ok((waiting, kept))
    }

    /// Whether a prompt was submitted to session `id` since `at` (events are
    /// kept a day).
    pub fn prompted_since(&self, id: &str, at: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = ?1 AND kind = 'prompt' AND at >= ?2)",
                params![id, at],
                |r| r.get(0),
            )?)
    }

    /// A value roster keeps for itself.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// What each model alias (and "default") last turned out to be, as Claude
    /// names it: {"opus": "Opus 5.5", "default": "Opus 5.5 (1M)"}.
    pub fn model_names(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM meta WHERE key LIKE 'model:%'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut names = HashMap::new();
        for row in rows {
            let (key, model) = row?;
            names.insert(
                key["model:".len()..].to_string(),
                crate::util::model_name(&model),
            );
        }
        Ok(names)
    }

    /// Fills in the reported model of sessions that have none yet, from the newest
    /// hook event that named one (events are kept for a day). For sessions whose
    /// start was processed before roster recorded models.
    pub fn backfill_models_from_events(&self) -> Result<usize> {
        let newest = "SELECT json_extract(e.payload, '$.model') FROM events e
             WHERE e.session_id = sessions.id AND json_valid(e.payload)
               AND json_extract(e.payload, '$.model') IS NOT NULL
             ORDER BY e.id DESC LIMIT 1";
        Ok(self.conn.execute(
            &format!("UPDATE sessions SET running_model = ({newest}) WHERE running_model IS NULL AND ({newest}) IS NOT NULL"),
            [],
        )?)
    }

    /// Learns what each alias (or the default) turned out to be from sessions
    /// that know both; the most recent session wins. The default only from a
    /// model Claude reported on starting: a resumed conversation keeps its own.
    pub fn learn_model_names(&self) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value)
             SELECT 'model:' || coalesce(model, 'default'), running_model FROM sessions s
             WHERE running_model IS NOT NULL
               AND (model IS NOT NULL OR EXISTS (
                 SELECT 1 FROM events e WHERE e.session_id = s.id AND json_valid(e.payload)
                   AND json_extract(e.payload, '$.model') = s.running_model))
             ORDER BY coalesce(state_since, created_at)",
            [],
        )?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// The id of the `n`th newest event (to show the last few), or 0.
    pub fn event_id_before_last(&self, n: i64, session: Option<&str>) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM events WHERE ?1 IS NULL OR session_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2",
                params![session, n],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    #[cfg(test)]
    pub fn insert_event(
        &self,
        session: &str,
        kind: &str,
        at: i64,
        payload: Option<&str>,
    ) -> Result<()> {
        insert_event(&self.conn, session, kind, at, payload)
    }
}

/// Inserts one event. Used by `roster report` on its own minimal connection.
pub fn insert_event(
    conn: &Connection,
    session: &str,
    kind: &str,
    at: i64,
    payload: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO events (session_id, kind, at, payload) VALUES (?1, ?2, ?3, ?4)",
        params![session, kind, at, payload],
    )?;
    Ok(())
}

fn events_after(
    conn: &Connection,
    after: i64,
    session: Option<&str>,
    limit: i64,
) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, kind, at, payload FROM events
         WHERE id > ?1 AND (?2 IS NULL OR session_id = ?2) ORDER BY id LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![after, session, limit], |r| {
        Ok(Event {
            id: r.get(0)?,
            session_id: r.get(1)?,
            kind: r.get(2)?,
            at: r.get(3)?,
            payload: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn load_session(conn: &Connection, id: &str) -> Result<Option<Session>> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions s JOIN projects p ON p.id = s.project_id WHERE s.id = ?1"
    );
    Ok(conn.query_row(&sql, [id], session_from_row).optional()?)
}

fn save_session(conn: &Connection, s: &Session) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET name = ?2, conversation_id = ?3, transcript_path = ?4, state = ?5, unread = ?6,
             exit_status = ?7, last_event_at = ?8, screen_hash = ?9, screen_changed_at = ?10, deleted_at = ?11,
             baseline_at = ?12, title = ?13, model = ?14, effort = ?15, state_since = ?16,
             pending_prompt = ?17, running_model = ?18, running_effort = ?19, title_override = ?20,
             turns = ?21, worked_ms = ?22, fork_from = ?23
         WHERE id = ?1",
        params![
            s.id,
            s.name,
            s.conversation_id,
            s.transcript_path,
            s.state.as_str(),
            s.unread,
            s.exit_status,
            s.last_event_at,
            s.screen_hash,
            s.screen_changed_at,
            s.deleted_at,
            s.baseline_at,
            s.title,
            s.model,
            s.effort,
            s.state_since,
            s.pending_prompt,
            s.running_model,
            s.running_effort,
            s.title_override,
            s.turns,
            s.worked_ms,
            s.fork_from,
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            anyhow::anyhow!("project {:?} already has a session named {:?}", s.project, s.name)
        }
        e => e.into(),
    })?;
    Ok(())
}

fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        origin: r.get::<_, String>(2)?.into(),
        agent: r.get(3)?,
        model: r.get(4)?,
        effort: r.get(5)?,
    })
}

fn session_from_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        project_id: r.get(1)?,
        project: r.get(2)?,
        name: r.get(3)?,
        agent: r.get(4)?,
        workdir: r.get::<_, String>(5)?.into(),
        conversation_id: r.get(6)?,
        transcript_path: r.get(7)?,
        state: State::parse(&r.get::<_, String>(8)?),
        unread: r.get(9)?,
        exit_status: r.get(10)?,
        last_event_at: r.get(11)?,
        screen_hash: r.get(12)?,
        screen_changed_at: r.get(13)?,
        created_at: r.get(14)?,
        deleted_at: r.get(15)?,
        baseline_at: r.get(16)?,
        title: r.get(17)?,
        model: r.get(18)?,
        effort: r.get(19)?,
        state_since: r.get(20)?,
        pending_prompt: r.get(21)?,
        running_model: r.get(22)?,
        running_effort: r.get(23)?,
        title_override: r.get(24)?,
        turns: r.get(25)?,
        worked_ms: r.get(26)?,
        fork_from: r.get(27)?,
    })
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", p.display()))
}

#[cfg(test)]
mod tests {
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
        assert_eq!(p.model, None);
        let version: i64 = db
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn project_defaults_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let mut p = project("demo");
        p.model = Some("opus".into());
        p.effort = Some("high".into());
        let mut p = db.add_project(&p, 0).unwrap();
        assert_eq!(
            db.project("demo").unwrap().unwrap().model.as_deref(),
            Some("opus")
        );
        p.name = "renamed".into();
        p.effort = None;
        db.save_project(&p).unwrap();
        let saved = db.project("renamed").unwrap().unwrap();
        assert_eq!((saved.model.as_deref(), saved.effort), (Some("opus"), None));
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
        db.insert_event(&s.id, "session", 10, Some(r#"{"model":"claude-opus-5-5"}"#))
            .unwrap();
        db.fold_events(100).unwrap();
        assert_eq!(db.model_names().unwrap()["opus"], "Opus 5.5");
        assert_eq!(
            db.session(&s.id).unwrap().running_model.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn backfills_models_from_events_processed_earlier() {
        let (mut db, s) = db_with_session();
        db.insert_event(
            &s.id,
            "session",
            10,
            Some(r#"{"model":"claude-opus-5-5[1m]"}"#),
        )
        .unwrap();
        db.fold_events(100).unwrap();
        // As if an older roster processed it: the model wasn't kept.
        let mut lost = db.session(&s.id).unwrap();
        lost.running_model = None;
        db.save_session(&lost).unwrap();
        db.conn
            .execute("DELETE FROM meta WHERE key LIKE 'model:%'", [])
            .unwrap();
        assert_eq!(db.backfill_models_from_events().unwrap(), 1);
        db.learn_model_names().unwrap();
        assert_eq!(
            db.session(&s.id).unwrap().running_model.as_deref(),
            Some("claude-opus-5-5[1m]")
        );
        assert_eq!(db.model_names().unwrap()["default"], "Opus 5.5 (1M)");
        // A model only read from a conversation (say, an adopted one) isn't the default.
        let mut adopted = session("b");
        adopted.project_id = s.project_id;
        adopted.running_model = Some("claude-haiku-5-5".into());
        adopted.state_since = Some(i64::MAX);
        db.insert_session(&adopted).unwrap();
        db.learn_model_names().unwrap();
        assert_eq!(db.model_names().unwrap()["default"], "Opus 5.5 (1M)");
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
}
