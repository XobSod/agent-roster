//! The database's tables, and how each schema version comes from the one before.

use super::*;

pub(super) const SCHEMA_VERSION: i64 = 15;

pub(super) const SCHEMA: &str = "
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
    awaiting          INTEGER NOT NULL DEFAULT 0,
    shells            TEXT,
    started_at        INTEGER,
    session_at        INTEGER
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
/// the texts would take 393. Their indexes are `SPENT_INDEXES`.
pub(super) const SPENT_TABLES: &str = "
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
pub(super) const SPENT_CALLS_ONCE: &str = "CREATE UNIQUE INDEX spent_call ON spent(call);";

/// What was spent, found by time and by file.
pub(super) const SPENT_INDEXES: &str = "
CREATE INDEX spent_at ON spent(at);
CREATE INDEX spent_file ON spent(file);
";

/// How a schema version comes from the one before: SQL, or for one that
/// has to compute what it keeps, code.
pub(super) enum Migration {
    Sql(&'static str),
    Code(fn(&Transaction) -> rusqlite::Result<()>),
}

pub(super) use Migration::{Code, Sql};

/// What each schema version adds to the one before.
pub(super) const MIGRATIONS: &[(i64, Migration)] = &[
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
    (
        14,
        Sql(
            "ALTER TABLE sessions RENAME COLUMN awaiting_subagents TO awaiting;
         ALTER TABLE sessions ADD COLUMN shells TEXT;",
        ),
    ),
    (
        15,
        Sql("ALTER TABLE sessions ADD COLUMN started_at INTEGER;
         ALTER TABLE sessions ADD COLUMN session_at INTEGER;"),
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
pub(super) fn spent_by_number(tx: &Transaction) -> rusqlite::Result<()> {
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
pub(super) fn index_calls_once(tx: &Transaction) -> rusqlite::Result<usize> {
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
