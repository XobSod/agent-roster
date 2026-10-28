//! The SQLite state database: projects, sessions, reported events, and what
//! the agents spent, in WAL mode so hooks can insert events while the CLI or
//! TUI reads.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, named_params, params,
};

use crate::model::{Event, Project, Session, State};
use crate::note::{Note, Whose};

const SCHEMA_VERSION: i64 = 13;

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    origin     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    agent      TEXT,
    model      TEXT,
    effort     TEXT,
    note       TEXT,
    note_pin   INTEGER,
    note_at    INTEGER
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
    fork_from         TEXT,
    note              TEXT,
    note_pin          INTEGER,
    note_at           INTEGER,
    asked_by          TEXT,
    given_prompt      TEXT,
    subagents         TEXT,
    awaiting_subagents INTEGER NOT NULL DEFAULT 0
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

/// What was spent: the agents' files, by number, each as it was when saved
/// and with the version of roster's reading and prices it was saved with;
/// the models, by number; and each call, by its id's hash (`call_key`), of
/// its file and model by number: about 106 bytes a call, where repeating
/// the texts took 393. Their indexes are `SPENT_INDEXES`.
const SPENT_TABLES: &str = "
CREATE TABLE spent_files (
    id       INTEGER PRIMARY KEY,
    path     TEXT NOT NULL UNIQUE,
    agent    TEXT NOT NULL,
    session  TEXT NOT NULL,
    modified INTEGER NOT NULL,
    size     INTEGER NOT NULL,
    version  TEXT NOT NULL DEFAULT ''
);
CREATE TABLE spent_models (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE spent (
    call        INTEGER NOT NULL,
    file        INTEGER NOT NULL,
    model       INTEGER NOT NULL,
    at          INTEGER NOT NULL,
    input       INTEGER NOT NULL,
    cache_read  INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    output      INTEGER NOT NULL,
    cost        REAL,
    saved       REAL
);
";

/// What was spent has each call once, by its id's hash: a call copied into
/// another file is still one call.
const SPENT_CALLS_ONCE: &str = "CREATE UNIQUE INDEX spent_call ON spent(call);";

/// What was spent, found by time and by file.
const SPENT_INDEXES: &str = "
CREATE INDEX spent_at ON spent(at);
CREATE INDEX spent_file ON spent(file);
";

const SESSION_COLUMNS: &str =
    "s.id, s.project_id, p.name AS project, s.name, s.agent, s.workdir, s.conversation_id,
    s.transcript_path, s.state, s.unread, s.exit_status, s.last_event_at, s.screen_hash,
    s.screen_changed_at, s.created_at, s.deleted_at, s.baseline_at, s.title, s.model, s.effort,
    s.state_since, s.pending_prompt, s.running_model, s.running_effort, s.title_override, s.turns,
    s.worked_ms, s.fork_from, s.asked_by, s.given_prompt, s.subagents, s.awaiting_subagents,
    s.note, s.note_pin, s.note_at";

const PROJECT_COLUMNS: &str = "id, name, origin, agent, model, effort, note, note_pin, note_at";

/// How a schema version comes from the one before: SQL, or for one that
/// has to compute what it keeps, code.
enum Migration {
    Sql(&'static str),
    Code(fn(&Transaction) -> rusqlite::Result<()>),
}

use Migration::{Code, Sql};

/// What each schema version adds to the one before.
const MIGRATIONS: &[(i64, Migration)] = &[
    (
        2,
        Sql("ALTER TABLE sessions ADD COLUMN baseline_at INTEGER;"),
    ),
    (
        3,
        Sql("ALTER TABLE projects ADD COLUMN agent TEXT;
         ALTER TABLE projects ADD COLUMN model TEXT;
         ALTER TABLE projects ADD COLUMN effort TEXT;
         ALTER TABLE sessions ADD COLUMN title TEXT;
         ALTER TABLE sessions ADD COLUMN model TEXT;
         ALTER TABLE sessions ADD COLUMN effort TEXT;
         ALTER TABLE sessions ADD COLUMN state_since INTEGER;
         ALTER TABLE sessions ADD COLUMN pending_prompt TEXT;"),
    ),
    (
        4,
        Sql("ALTER TABLE sessions ADD COLUMN running_model TEXT;
         ALTER TABLE sessions ADD COLUMN running_effort TEXT;"),
    ),
    (
        5,
        Sql("ALTER TABLE sessions ADD COLUMN title_override TEXT;
         ALTER TABLE sessions ADD COLUMN turns INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE sessions ADD COLUMN worked_ms INTEGER NOT NULL DEFAULT 0;"),
    ),
    (6, Sql("ALTER TABLE sessions ADD COLUMN fork_from TEXT;")),
    (
        7,
        Sql("ALTER TABLE sessions ADD COLUMN note TEXT;
         ALTER TABLE sessions ADD COLUMN note_pin INTEGER;
         ALTER TABLE sessions ADD COLUMN note_at INTEGER;"),
    ),
    (
        8,
        Sql("CREATE TABLE spent_files (
             path     TEXT PRIMARY KEY,
             agent    TEXT NOT NULL,
             session  TEXT NOT NULL,
             modified INTEGER NOT NULL,
             size     INTEGER NOT NULL
         );
         CREATE TABLE spent (
             id          TEXT PRIMARY KEY,
             path        TEXT NOT NULL,
             model       TEXT NOT NULL,
             at          INTEGER NOT NULL,
             input       INTEGER NOT NULL,
             cache_read  INTEGER NOT NULL,
             cache_write INTEGER NOT NULL,
             output      INTEGER NOT NULL,
             cost        REAL,
             saved       REAL
         );
         CREATE INDEX spent_at ON spent(at);
         CREATE INDEX spent_path ON spent(path);"),
    ),
    (
        9,
        Sql("ALTER TABLE projects ADD COLUMN note TEXT;
         ALTER TABLE projects ADD COLUMN note_pin INTEGER;
         ALTER TABLE projects ADD COLUMN note_at INTEGER;"),
    ),
    (10, Sql("ALTER TABLE sessions ADD COLUMN asked_by TEXT;")),
    (
        11,
        Sql("ALTER TABLE sessions ADD COLUMN given_prompt TEXT;"),
    ),
    (12, Code(spent_by_number)),
    (
        13,
        Sql("ALTER TABLE sessions ADD COLUMN subagents TEXT;
         ALTER TABLE sessions ADD COLUMN awaiting_subagents INTEGER NOT NULL DEFAULT 0;"),
    ),
];

/// Schema 12: what was spent refers to its file and model by number, and to
/// its call by its id's hash, rather than repeating their texts; each file
/// keeps the version it was saved with, where one marker (`spent:version`)
/// said it for all of them, and has it now. Once, as roster first opens the
/// database: seconds for a million calls, while hooks can't write their
/// reports, so the calls are copied first and indexed after (a third of the
/// time indexing as they go took), and the room the texts took goes back to
/// the disk after. Meanwhile the file takes up to twice its room.
fn spent_by_number(tx: &Transaction) -> rusqlite::Result<()> {
    // Said where it takes long enough to be noticed.
    let many: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM spent LIMIT 1 OFFSET 99999)",
        [],
        |r| r.get(0),
    )?;
    if many {
        eprintln!("roster: updating its database, once (a few seconds)…");
    }
    tx.execute_batch(
        "ALTER TABLE spent RENAME TO spent_old;
         ALTER TABLE spent_files RENAME TO spent_files_old;
         DROP INDEX spent_at;
         DROP INDEX spent_path;",
    )?;
    tx.execute_batch(SPENT_TABLES)?;
    tx.execute_batch(
        "INSERT INTO spent_files (path, agent, session, modified, size, version)
             SELECT path, agent, session, modified, size,
                 COALESCE((SELECT value FROM meta WHERE key = 'spent:version'), '')
             FROM spent_files_old;
         INSERT INTO spent_models (name) SELECT DISTINCT model FROM spent_old;",
    )?;
    {
        let mut read = tx.prepare(
            "SELECT s.id, f.id, m.id, s.at, s.input, s.cache_read, s.cache_write, s.output,
                 s.cost, s.saved
             FROM spent_old s JOIN spent_files f ON f.path = s.path
                 JOIN spent_models m ON m.name = s.model",
        )?;
        // Unindexed yet: each as it comes, two calls whose ids share a hash
        // both.
        let mut write = tx.prepare(
            "INSERT INTO spent (call, file, model, at, input, cache_read,
                 cache_write, output, cost, saved)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let mut rows = read.query([])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            write.execute(params![
                call_key(&id),
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<f64>>(9)?,
            ])?;
        }
    }
    // Dropped before indexing, for the indexes to take the room it leaves.
    tx.execute_batch(
        "DROP TABLE spent_old;
         DROP TABLE spent_files_old;
         DELETE FROM meta WHERE key = 'spent:version';",
    )?;
    let dropped = index_calls_once(tx)?;
    match dropped {
        0 => {}
        1 => eprintln!("roster: a call whose id's hash another's had is left out"),
        n => eprintln!("roster: {n} calls whose ids' hashes others had are left out"),
    }
    tx.execute_batch(SPENT_INDEXES)
}

/// Gives the calls copied unindexed their unique index; how many were left
/// out, sharing their id's hash with one before them: the first copied is
/// kept, as it would have been saved first (a chance in hundreds of
/// thousands, for ten million calls).
fn index_calls_once(tx: &Transaction) -> rusqlite::Result<usize> {
    match tx.execute_batch(SPENT_CALLS_ONCE) {
        Err(e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) => {
            let dropped = tx.execute(
                "DELETE FROM spent
                 WHERE rowid NOT IN (SELECT min(rowid) FROM spent GROUP BY call)",
                [],
            )?;
            tx.execute_batch(SPENT_CALLS_ONCE)?;
            Ok(dropped)
        }
        done => done.map(|()| 0),
    }
}

/// A call's id as roster keeps it: its 64-bit hash (FNV-1a, the same on
/// every machine and in every version), which tells calls apart as the id
/// does in an eighth of the room: of ten million calls, two sharing one is
/// a chance in hundreds of thousands.
fn call_key(id: &str) -> i64 {
    crate::util::fnv1a_64(id.as_bytes()) as i64
}

/// SQLite's `auto_vacuum` mode in which `PRAGMA incremental_vacuum` gives
/// free pages back.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// Applied events older than this are deleted.
const EVENT_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

pub struct Db {
    conn: Connection,
}

/// One of the agents' files, its calls to save as of its `modified` time
/// (ms) and `size`.
pub struct SpentSave {
    pub path: String,
    pub agent: String,
    pub session: String,
    pub modified: i64,
    pub size: u64,
    pub calls: Vec<crate::tokens::Call>,
}

/// One of the agents' files whose calls are saved, as it was then.
#[derive(Clone, Debug, PartialEq)]
pub struct SpentFile {
    pub agent: String,
    /// The conversation it's of.
    pub session: String,
    /// In ms.
    pub modified: i64,
    pub size: u64,
    /// Of roster's reading and prices, as it was saved
    /// (`tokens::Prices::version`); "" before each file had one.
    pub version: String,
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

    fn init(mut conn: Connection) -> Result<Db> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // In one transaction, with the version read inside it: two rosters
        // starting at once don't both migrate, and an interrupted migration
        // leaves the database as it was.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                tx.execute_batch(SCHEMA)?;
                tx.execute_batch(SPENT_TABLES)?;
                tx.execute_batch(SPENT_CALLS_ONCE)?;
                tx.execute_batch(SPENT_INDEXES)?;
            }
            v if v > SCHEMA_VERSION => {
                bail!("the database is from a newer roster (schema {v}); upgrade roster")
            }
            v => {
                for (_, migration) in MIGRATIONS.iter().filter(|(to, _)| *to > v) {
                    match migration {
                        Sql(sql) => tx.execute_batch(sql)?,
                        Code(migrate) => migrate(&tx)?,
                    }
                }
            }
        }
        if version != SCHEMA_VERSION {
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        // Not a schema change: older rosters use the index without knowing it.
        tx.execute_batch("CREATE INDEX IF NOT EXISTS events_session ON events(session_id);")?;
        // The highest project id given (`add_project`) is at least the
        // highest there is, before any can be removed: a database from
        // before it was kept has its projects all the same.
        let highest: Option<i64> =
            tx.query_row("SELECT max(id) FROM projects", [], |r| r.get(0))?;
        let kept = last_project_id(&tx)?;
        if let Some(highest) = highest.filter(|&h| h > kept) {
            set_meta(&tx, "last_project_id", &highest.to_string())?;
        }
        tx.commit()?;
        // Pages that pruning old events frees go back to the disk as they're
        // freed (`fold_events`), so the file doesn't stay at its largest (54
        // MB, most of it free, found in real use). Switching an existing file
        // to that takes one VACUUM, done once (73 ms for that file, well
        // within what hooks wait). Older rosters open it just the same.
        let incremental: i64 = conn.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
        if incremental != AUTO_VACUUM_INCREMENTAL {
            conn.pragma_update(None, "auto_vacuum", AUTO_VACUUM_INCREMENTAL)?;
            // Busy (another roster at it): the next start tries again.
            conn.execute_batch("VACUUM").ok();
        }
        let db = Db { conn };
        // Much of the file free (what schema 12 left of what was spent
        // before, three times as much): it goes back to the disk, whenever
        // it's found, so a roster ended before it was done leaves it to the
        // next. Busy (another roster at it): the next start tries again.
        if db.mostly_free().unwrap_or(false) {
            db.give_back_free_pages_slowly().ok();
        }
        Ok(db)
    }

    /// Whether a good part of the file is free pages: over a quarter of it,
    /// and over 4 MB.
    fn mostly_free(&self) -> Result<bool> {
        let pragma =
            |name| -> Result<i64> { Ok(self.conn.pragma_query_value(None, name, |r| r.get(0))?) };
        let (free, pages, size) = (
            pragma("freelist_count")?,
            pragma("page_count")?,
            pragma("page_size")?,
        );
        Ok(free * 4 > pages && free * size > 4 << 20)
    }

    // Projects

    /// Inserts `project` (its id and note are ignored) and returns it with
    /// its new id: one no project had before, so what's addressed to a
    /// removed project (a note open in an editor) never reaches another.
    /// SQLite would give the highest id again once its project is removed.
    pub fn add_project(&self, project: &Project, now: i64) -> Result<Project> {
        let name = &project.name;
        let origin_str = path_str(&project.origin)?;
        // In one transaction, so two rosters adding at once don't pick the
        // same id.
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let highest: Option<i64> =
            tx.query_row("SELECT max(id) FROM projects", [], |r| r.get(0))?;
        let id = last_project_id(&tx)?.max(highest.unwrap_or(0)) + 1;
        tx.execute(
            "INSERT INTO projects (id, name, origin, created_at, agent, model, effort)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                name,
                origin_str,
                now,
                project.agent,
                project.model,
                project.effort
            ],
        )
        .map_err(|e| constraint(e, || format!("project {name:?} already exists")))?;
        set_meta(&tx, "last_project_id", &id.to_string())?;
        tx.commit()?;
        Ok(Project {
            id,
            note: None,
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
            .map_err(|e| constraint(e, || format!("project {:?} already exists", p.name)))?;
        Ok(())
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare_cached(&format!(
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

    /// The archived sessions removing `project` deletes for good; an error if
    /// it has a live one.
    pub fn purged_with(&self, project: &Project) -> Result<Vec<Session>> {
        purged_with(&self.conn, project)
    }

    /// Removes a project and its archived sessions. Returns the purged sessions.
    /// Checked again in the transaction that removes them, which no other
    /// process writes in meanwhile: one restored (or started) since it was
    /// asked about stops it.
    pub fn remove_project(&mut self, project: &Project) -> Result<Vec<Session>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sessions = purged_with(&tx, project)?;
        // Their events go too, as when a session is deleted.
        tx.execute(
            "DELETE FROM events WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            [project.id],
        )?;
        tx.execute("DELETE FROM sessions WHERE project_id = ?1", [project.id])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", [project.id])?;
        tx.commit()?;
        Ok(sessions)
    }

    // Sessions

    /// The names of a project's live sessions.
    pub fn live_names(&self, project_id: i64) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sessions WHERE project_id = ?1 AND deleted_at IS NULL")?;
        let rows = stmt.query_map([project_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether a live session of the project other than `except` is named `name`.
    pub fn name_taken(&self, project_id: i64, name: &str, except: Option<&str>) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE project_id = ?1 AND name = ?2
                AND deleted_at IS NULL AND id IS NOT ?3)",
            params![project_id, name, except],
            |r| r.get(0),
        )?)
    }

    pub fn insert_session(&self, s: &Session) -> Result<()> {
        self.conn
            .execute(
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
            )
            .map_err(|e| {
                constraint(e, || {
                    format!(
                        "project {:?} already has a session named {:?}",
                        s.project, s.name
                    )
                })
            })?;
        save_session(&self.conn, s)
    }

    /// Removes an archived session and its events for good.
    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM events WHERE session_id = ?1", [id])?;
        let n = tx.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        if n == 0 {
            bail!("no session {id}");
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
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn session(&self, id: &str) -> Result<Session> {
        load_session(&self.conn, id)?.with_context(|| format!("no session with id {id}"))
    }

    /// The whole row (`save_changes` is what roster saves with).
    #[cfg(test)]
    pub fn save_session(&self, s: &Session) -> Result<()> {
        save_session(&self.conn, s)
    }

    /// Saves what `after` changed of `before`, the session as it was loaded,
    /// and only that: whatever another roster saved of it meanwhile (an edit,
    /// a refresh, an archive) stays, unless it changed the same thing.
    // A field taken out of `after` below but not compared is an error.
    #[deny(unused_variables)]
    pub fn save_changes(&self, before: &Session, after: &Session) -> Result<()> {
        // Every field, so a new one isn't left out: what isn't saved here is
        // fixed, joined in, or saved apart (the note).
        let Session {
            id,
            project_id: _,
            project: _,
            agent: _,
            workdir: _,
            created_at: _,
            note: _,
            state,
            name,
            conversation_id,
            transcript_path,
            unread,
            exit_status,
            last_event_at,
            screen_hash,
            screen_changed_at,
            deleted_at,
            baseline_at,
            title,
            model,
            effort,
            state_since,
            pending_prompt,
            running_model,
            running_effort,
            title_override,
            turns,
            worked_ms,
            fork_from,
            asked_by,
            given_prompt,
            subagents,
            awaiting_subagents,
        } = after;
        let state_name = state.as_str();
        let subagents_text = subagents_text(subagents);
        let mut set: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if before.state != *state {
            set.push(("state", &state_name));
        }
        if before.subagents != *subagents {
            set.push(("subagents", &subagents_text));
        }
        macro_rules! changed {
            ($($field:ident),*) => {$(
                if before.$field != *$field {
                    set.push((stringify!($field), $field));
                }
            )*};
        }
        changed!(
            name,
            conversation_id,
            transcript_path,
            unread,
            exit_status,
            last_event_at,
            screen_hash,
            screen_changed_at,
            deleted_at,
            baseline_at,
            title,
            model,
            effort,
            state_since,
            pending_prompt,
            running_model,
            running_effort,
            title_override,
            turns,
            worked_ms,
            fork_from,
            asked_by,
            given_prompt,
            awaiting_subagents
        );
        if set.is_empty() {
            return Ok(());
        }
        let columns: Vec<String> = (2..)
            .zip(&set)
            .map(|(i, (column, _))| format!("{column} = ?{i}"))
            .collect();
        let sql = format!("UPDATE sessions SET {} WHERE id = ?1", columns.join(", "));
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![id];
        values.extend(set.iter().map(|(_, value)| *value));
        self.conn.execute(&sql, values.as_slice()).map_err(|e| {
            constraint(e, || {
                format!(
                    "project {:?} already has a session named {:?}",
                    after.project, after.name
                )
            })
        })?;
        Ok(())
    }

    /// Saves a session's or a project's note (None removes it), and only
    /// that.
    pub fn set_note(&self, whose: &Whose, note: Option<&Note>) -> Result<()> {
        let (table, id) = note_row(whose);
        let n = self.conn.execute(
            &format!("UPDATE {table} SET note = ?2, note_pin = ?3, note_at = ?4 WHERE id = ?1"),
            params![
                id,
                note.map(|n| &n.text),
                note.and_then(|n| n.pin).map(|p| p as i64),
                note.map(|n| n.at)
            ],
        )?;
        if n == 0 {
            bail!("{}", no_such(whose));
        }
        Ok(())
    }

    /// `set_note`, but only if the note is still `expected`: whether it was
    /// (an error if there's no such session or project).
    pub fn set_note_if(
        &self,
        whose: &Whose,
        expected: Option<&Note>,
        note: Option<&Note>,
    ) -> Result<bool> {
        let (table, id) = note_row(whose);
        let n = self.conn.execute(
            &format!(
                "UPDATE {table} SET note = ?2, note_pin = ?3, note_at = ?4
                 WHERE id = ?1 AND note IS ?5 AND note_pin IS ?6 AND note_at IS ?7"
            ),
            params![
                id,
                note.map(|n| &n.text),
                note.and_then(|n| n.pin).map(|p| p as i64),
                note.map(|n| n.at),
                expected.map(|n| &n.text),
                expected.and_then(|n| n.pin).map(|p| p as i64),
                expected.map(|n| n.at)
            ],
        )?;
        if n == 0 {
            let there: bool = self.conn.query_row(
                &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1)"),
                [&id],
                |r| r.get(0),
            )?;
            if !there {
                bail!("{}", no_such(whose));
            }
        }
        Ok(n == 1)
    }

    /// Sets a session's title of your own to `title`, if it's still `before`
    /// on conversation `conversation`: whether it was; None if there's no
    /// such session.
    pub fn set_title_if(
        &self,
        id: &str,
        title: &str,
        before: Option<&str>,
        conversation: Option<&str>,
    ) -> Result<Option<bool>> {
        let n = self.conn.execute(
            "UPDATE sessions SET title_override = ?2
             WHERE id = ?1 AND title_override IS ?3 AND conversation_id IS ?4",
            params![id, title, before, conversation],
        )?;
        if n == 1 {
            return Ok(Some(true));
        }
        Ok(load_session(&self.conn, id)?.map(|_| false))
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

    /// Puts back a first prompt taken but not typed in, unless another one
    /// was given meanwhile.
    pub fn restore_pending_prompt(&self, id: &str, prompt: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET pending_prompt = ?2 WHERE id = ?1 AND pending_prompt IS NULL",
            params![id, prompt],
        )?;
        Ok(())
    }

    // Events

    /// Applies events reported since the last fold, in one transaction, so any
    /// number of roster processes can fold concurrently. Returns how many were applied.
    /// Forgets the subagents sessions haven't heard of for a while
    /// (`Session::forget_quiet_subagents`), as of `now`; a session whose own
    /// turn is over with none left is idle. Each read again under the write
    /// lock, so what another roster saved meanwhile isn't overwritten; the
    /// lock taken only when there's something to forget.
    ///
    /// Those its agent says are finished (`finished`, given a session, a
    /// subagent's id and when it was last heard of) are stopped too, as if
    /// it had said so: a stop's report may be lost, or never sent (an agent
    /// started before roster took them). What's asked of the agent is asked
    /// before the lock.
    pub fn settle_subagents(
        &mut self,
        now: i64,
        finished: &dyn Fn(&Session, &str, i64) -> bool,
    ) -> Result<()> {
        let waiting = |conn: &Connection| -> Result<Vec<Session>> {
            let sql = format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s JOIN projects p ON p.id = s.project_id
                 WHERE s.deleted_at IS NULL AND (s.subagents IS NOT NULL OR s.awaiting_subagents)"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map([], session_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let mut done: HashMap<String, Vec<String>> = HashMap::new();
        let mut stale = false;
        for s in waiting(&self.conn)? {
            for (id, subagent) in &s.subagents {
                if finished(&s, id, subagent.heard) {
                    done.entry(s.id.clone()).or_default().push(id.clone());
                }
            }
            stale |= s.clone().forget_quiet_subagents(now);
        }
        if done.is_empty() && !stale {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for mut s in waiting(&tx)? {
            let before = s.clone();
            for id in done.get(&s.id).into_iter().flatten() {
                s.subagent_stopped(id, now);
            }
            s.forget_quiet_subagents(now);
            if s != before {
                save_session(&tx, &s)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn fold_events(&mut self, now: i64) -> Result<usize> {
        // Most of the time nothing is new: find that out without taking the
        // write lock, which agents' hooks wait for.
        let pending: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE id > coalesce(
                (SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'applied_event'), 0))",
            [],
            |r| r.get(0),
        )?;
        if !pending {
            return Ok(0);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let applied = applied_event(&tx)?;
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
                let report = match e.kind.as_str() {
                    // roster's own, in its own terms.
                    crate::model::AUTO_TITLE => e
                        .payload
                        .as_deref()
                        .and_then(|p| serde_json::from_str(p).ok())
                        .unwrap_or_default(),
                    _ => crate::agent::read_report(&s.agent, e.payload.as_deref()),
                };
                s.apply(e, &report);
                let mut learned = Vec::new();
                // What the model asked for (or the default) turned out to be, as
                // the agent started; switched to later inside it, it isn't what
                // was asked, and a conversation cleared, compacted or resumed in
                // it reports the one it runs.
                let started = e.kind == "session" && report.started;
                if let Some(model) = report.model.clone().filter(|_| started) {
                    let asked = s.model.as_deref().unwrap_or("default");
                    let key = crate::agent::learned_key(&s.agent, asked);
                    learned.push((format!("model:{key}"), model));
                }
                // The context window of the model it's running, for the statistics.
                if let (Some(window), Some(model)) = (report.context_window, &s.running_model) {
                    learned.push((format!("window:{model}"), window.to_string()));
                }
                for (key, value) in learned {
                    set_meta(&tx, &key, &value)?;
                }
            }
        }
        for s in touched.values() {
            save_session(&tx, s)?;
        }
        set_meta(&tx, "applied_event", &last.to_string())?;
        let pruned = tx.execute(
            "DELETE FROM events WHERE id <= ?1 AND at < ?2",
            params![last, now - EVENT_RETENTION_MS],
        )?;
        tx.commit()?;
        if pruned > 0 {
            self.give_back_free_pages().ok();
        }
        Ok(events.len())
    }

    /// Gives the file's free pages back to the disk. SQLite does it a page per
    /// step, so the statement runs to its end.
    fn give_back_free_pages(&self) -> Result<()> {
        self.vacuum("PRAGMA incremental_vacuum")
    }

    fn vacuum(&self, pragma: &str) -> Result<()> {
        let mut stmt = self.conn.prepare(pragma)?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    /// Gives many free pages back (hundreds of MB, say), a few MB at a time
    /// with a pause between, for hooks' reports to be written meanwhile
    /// meanwhile); then the log of those writes,
    /// which holds as much again until no roster has the file open (the TUI
    /// may run for days), gives its room back too.
    fn give_back_free_pages_slowly(&self) -> Result<()> {
        const PAGES: i64 = 1000;
        loop {
            self.vacuum(&format!("PRAGMA incremental_vacuum({PAGES})"))?;
            let free: i64 = self
                .conn
                .pragma_query_value(None, "freelist_count", |r| r.get(0))?;
            if free == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        self.vacuum("PRAGMA wal_checkpoint(TRUNCATE)")
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
        let applied = applied_event(&self.conn)?;
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
        set_meta(&self.conn, key, value)
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

    /// Claims the making of `id`'s automatic title for its `conversation`:
    /// true the first time, so one roster makes it, once per conversation.
    pub fn claim_auto_title(&self, id: &str, conversation: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value
             WHERE value != excluded.value",
            [format!("auto_title:{id}"), conversation.to_string()],
        )?;
        Ok(changed > 0)
    }

    /// The context window an agent reported for `model`, if one did.
    pub fn learned_window(&self, model: &str) -> Result<Option<u64>> {
        Ok(self
            .meta(&format!("window:{model}"))?
            .and_then(|v| v.parse().ok()))
    }

    // What was spent

    /// The agents' files whose calls are saved, by path, as each was when
    /// they were.
    pub fn spent_files(&self) -> Result<HashMap<String, SpentFile>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, agent, session, modified, size, version FROM spent_files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                SpentFile {
                    agent: r.get(1)?,
                    session: r.get(2)?,
                    modified: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    version: r.get(5)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Saves the calls in the file at `path` in place of what was saved of
    /// it, as of its `modified` time (ms) and `size`.
    #[cfg(test)]
    pub fn save_spent(
        &self,
        path: &str,
        agent: &str,
        session: &str,
        modified: i64,
        size: u64,
        calls: &[crate::tokens::Call],
    ) -> Result<()> {
        let save = SpentSave {
            path: path.into(),
            agent: agent.into(),
            session: session.into(),
            modified,
            size,
            calls: calls.to_vec(),
        };
        self.save_spent_files(&[save], &crate::tokens::Prices::default())
    }

    /// Saves the calls in each of `files` in place of what was saved of it,
    /// priced at `prices`, and with their version, in one transaction. A
    /// call saved already from another file (copied there by a fork) stays
    /// that file's; one that spent nothing (it failed) isn't saved.
    pub fn save_spent_files(
        &self,
        files: &[SpentSave],
        prices: &crate::tokens::Prices,
    ) -> Result<()> {
        let version = prices.version();
        let tx = self.conn.unchecked_transaction()?;
        let mut file = tx.prepare(
            "INSERT INTO spent_files (path, agent, session, modified, size, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (path) DO UPDATE SET agent = excluded.agent,
                 session = excluded.session, modified = excluded.modified,
                 size = excluded.size, version = excluded.version
             RETURNING id",
        )?;
        let mut forget = tx.prepare("DELETE FROM spent WHERE file = ?1")?;
        let mut model = tx.prepare(
            "INSERT INTO spent_models (name) VALUES (?1)
             ON CONFLICT (name) DO UPDATE SET name = excluded.name
             RETURNING id",
        )?;
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO spent (call, file, model, at, input, cache_read,
                 cache_write, output, cost, saved)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let mut models: HashMap<&str, i64> = HashMap::new();
        for f in files {
            let id: i64 = file.query_row(
                params![
                    f.path,
                    f.agent,
                    f.session,
                    f.modified,
                    f.size as i64,
                    version
                ],
                |r| r.get(0),
            )?;
            forget.execute([id])?;
            for call in f.calls.iter().filter(|c| c.counts.total() > 0) {
                let model_id = match models.get(call.model.as_str()) {
                    Some(&known) => known,
                    None => {
                        let new: i64 = model.query_row([&call.model], |r| r.get(0))?;
                        models.insert(&call.model, new);
                        new
                    }
                };
                let (t, cost) = (&call.counts, prices.cost(call));
                insert.execute(params![
                    call_key(&call.id),
                    id,
                    model_id,
                    call.at,
                    t.input as i64,
                    t.cache_read as i64,
                    t.cache_write as i64,
                    t.output as i64,
                    cost.map(|c| c.0),
                    cost.map(|c| c.1),
                ])?;
            }
        }
        drop((file, forget, model, insert));
        tx.commit()?;
        Ok(())
    }

    /// Forgets what was saved of the file at `path` (it moved).
    pub fn forget_spent(&self, path: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM spent WHERE file = (SELECT id FROM spent_files WHERE path = ?1)",
            [path],
        )?;
        tx.execute("DELETE FROM spent_files WHERE path = ?1", [path])?;
        tx.commit()?;
        Ok(())
    }

    /// What the calls saved from `from` to `to` (ms) spent, by conversation,
    /// model and hour.
    pub fn spent_hours(&self, from: i64, to: i64) -> Result<Vec<crate::tokens::Hour>> {
        use crate::tokens::{Counts, Hour, Spent};
        let mut stmt = self.conn.prepare(
            "SELECT f.agent, f.session, m.name, s.at / 3600000 * 3600000 AS hour,
                 SUM(s.input), SUM(s.cache_read), SUM(s.cache_write), SUM(s.output),
                 TOTAL(s.cost), TOTAL(s.saved),
                 TOTAL(CASE WHEN s.cost IS NULL
                     THEN s.input + s.cache_read + s.cache_write + s.output END)
             FROM spent s JOIN spent_files f ON f.id = s.file
                 JOIN spent_models m ON m.id = s.model
             WHERE s.at >= ?1 AND s.at <= ?2
             GROUP BY f.agent, f.session, m.name, hour",
        )?;
        let rows = stmt.query_map([from, to], |r| {
            let n = |i| r.get::<_, i64>(i).map(|v| v as u64);
            Ok(Hour {
                agent: r.get(0)?,
                session: r.get(1)?,
                model: r.get(2)?,
                start: r.get(3)?,
                spent: Spent {
                    tokens: Counts {
                        input: n(4)?,
                        cache_read: n(5)?,
                        cache_write: n(6)?,
                        cache_write_hour: 0,
                        output: n(7)?,
                    },
                    cost: r.get(8)?,
                    saved: r.get(9)?,
                    unpriced: r.get::<_, f64>(10)? as u64,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
        "UPDATE sessions SET
             name = :name,
             conversation_id = :conversation_id,
             transcript_path = :transcript_path,
             state = :state,
             unread = :unread,
             exit_status = :exit_status,
             last_event_at = :last_event_at,
             screen_hash = :screen_hash,
             screen_changed_at = :screen_changed_at,
             deleted_at = :deleted_at,
             baseline_at = :baseline_at,
             title = :title,
             model = :model,
             effort = :effort,
             state_since = :state_since,
             pending_prompt = :pending_prompt,
             running_model = :running_model,
             running_effort = :running_effort,
             title_override = :title_override,
             turns = :turns,
             worked_ms = :worked_ms,
             fork_from = :fork_from,
             asked_by = :asked_by,
             given_prompt = :given_prompt,
             subagents = :subagents,
             awaiting_subagents = :awaiting_subagents
         WHERE id = :id",
        named_params! {
            ":id": s.id,
            ":name": s.name,
            ":conversation_id": s.conversation_id,
            ":transcript_path": s.transcript_path,
            ":state": s.state.as_str(),
            ":unread": s.unread,
            ":exit_status": s.exit_status,
            ":last_event_at": s.last_event_at,
            ":screen_hash": s.screen_hash,
            ":screen_changed_at": s.screen_changed_at,
            ":deleted_at": s.deleted_at,
            ":baseline_at": s.baseline_at,
            ":title": s.title,
            ":model": s.model,
            ":effort": s.effort,
            ":state_since": s.state_since,
            ":pending_prompt": s.pending_prompt,
            ":running_model": s.running_model,
            ":running_effort": s.running_effort,
            ":title_override": s.title_override,
            ":turns": s.turns,
            ":worked_ms": s.worked_ms,
            ":fork_from": s.fork_from,
            ":asked_by": s.asked_by,
            ":given_prompt": s.given_prompt,
            ":subagents": subagents_text(&s.subagents),
            ":awaiting_subagents": s.awaiting_subagents,
        },
    )
    .map_err(|e| {
        constraint(e, || {
            format!(
                "project {:?} already has a session named {:?}",
                s.project, s.name
            )
        })
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
        note: note_from_row(r)?,
    })
}

/// A session's or a project's note, from its columns; None without text.
fn note_from_row(r: &Row) -> rusqlite::Result<Option<Note>> {
    Ok(match r.get::<_, Option<String>>("note")? {
        Some(text) => Some(Note {
            text,
            pin: r.get::<_, Option<i64>>("note_pin")?.map(|p| p as usize),
            at: r.get::<_, Option<i64>>("note_at")?.unwrap_or(0),
        }),
        None => None,
    })
}

/// Where a note is kept: its table, and its row's id.
fn note_row(whose: &Whose) -> (&'static str, rusqlite::types::Value) {
    match whose {
        Whose::Session(id) => ("sessions", id.clone().into()),
        Whose::Project(id) => ("projects", (*id).into()),
    }
}

fn no_such(whose: &Whose) -> String {
    match whose {
        Whose::Session(id) => format!("no session with id {id}"),
        Whose::Project(id) => format!("no project with id {id}"),
    }
}

/// A project's sessions, all archived, which removing it deletes for good;
/// an error if one is live.
fn purged_with(conn: &Connection, project: &Project) -> Result<Vec<Session>> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions s JOIN projects p ON p.id = s.project_id
         WHERE s.project_id = ?1 ORDER BY s.created_at, s.id"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let sessions: Vec<Session> = stmt
        .query_map([project.id], session_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    if let Some(live) = sessions.iter().find(|s| s.deleted_at.is_none()) {
        bail!(
            "project {:?} still has session {:?}; archive its sessions first",
            project.name,
            live.name
        );
    }
    Ok(sessions)
}

/// A row of `SESSION_COLUMNS`, read by column name.
fn session_from_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        project: r.get("project")?,
        name: r.get("name")?,
        agent: r.get("agent")?,
        workdir: r.get::<_, String>("workdir")?.into(),
        conversation_id: r.get("conversation_id")?,
        transcript_path: r.get("transcript_path")?,
        state: State::parse(&r.get::<_, String>("state")?),
        unread: r.get("unread")?,
        exit_status: r.get("exit_status")?,
        last_event_at: r.get("last_event_at")?,
        screen_hash: r.get("screen_hash")?,
        screen_changed_at: r.get("screen_changed_at")?,
        created_at: r.get("created_at")?,
        deleted_at: r.get("deleted_at")?,
        baseline_at: r.get("baseline_at")?,
        title: r.get("title")?,
        model: r.get("model")?,
        effort: r.get("effort")?,
        state_since: r.get("state_since")?,
        pending_prompt: r.get("pending_prompt")?,
        running_model: r.get("running_model")?,
        running_effort: r.get("running_effort")?,
        title_override: r.get("title_override")?,
        turns: r.get("turns")?,
        worked_ms: r.get("worked_ms")?,
        fork_from: r.get("fork_from")?,
        asked_by: r.get("asked_by")?,
        given_prompt: r.get("given_prompt")?,
        subagents: (r.get::<_, Option<String>>("subagents")?)
            .map(|text| subagents_from(&text))
            .unwrap_or_default(),
        awaiting_subagents: r.get("awaiting_subagents")?,
        note: note_from_row(r)?,
    })
}

/// A session's subagents as kept: JSON, by id; nothing for none.
fn subagents_text(
    subagents: &std::collections::BTreeMap<String, crate::model::Subagent>,
) -> Option<String> {
    (!subagents.is_empty()).then(|| serde_json::to_string(subagents).unwrap_or_default())
}

/// A session's subagents from what's kept; as a build kept them that kept
/// only when each was last heard of, too. None from what isn't either.
fn subagents_from(text: &str) -> std::collections::BTreeMap<String, crate::model::Subagent> {
    use crate::model::Subagent;
    use std::collections::BTreeMap;
    if let Ok(subagents) = serde_json::from_str::<BTreeMap<String, Subagent>>(text) {
        return subagents;
    }
    let heard: BTreeMap<String, i64> = serde_json::from_str(text).unwrap_or_default();
    (heard.into_iter())
        .map(|(id, at)| {
            let subagent = Subagent {
                kind: None,
                started: at,
                heard: at,
            };
            (id, subagent)
        })
        .collect()
}

/// The highest project id given, 0 for none.
fn last_project_id(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'last_project_id'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// The id of the last event folded into the sessions (0 for none yet).
fn applied_event(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'applied_event'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

/// A constraint violation (a name taken) as `message`; other errors as they are.
fn constraint(e: rusqlite::Error, message: impl FnOnce() -> String) -> anyhow::Error {
    match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            anyhow::anyhow!(message())
        }
        e => e.into(),
    }
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
        after.awaiting_subagents = true;
        db.save_changes(&before, &after).unwrap();
        assert_eq!(db.session(&s.id).unwrap(), after);
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
        s.awaiting_subagents = true;
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

        // A project's, the same way; an edit of the project leaves it.
        let p = db.project(&s.project).unwrap().unwrap();
        let whose = Whose::Project(p.id);
        assert!(db.set_note_if(&whose, None, Some(&note)).unwrap());
        assert!(!db.set_note_if(&whose, None, Some(&newer)).unwrap());
        db.save_project(&p).unwrap();
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
        let started =
            "INSERT INTO sessions (id, project_id, name, agent, workdir, state, created_at)
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
}
