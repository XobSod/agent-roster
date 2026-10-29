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

mod events;
mod notes;
mod projects;
mod schema;
mod sessions;
mod spent;
#[cfg(test)]
mod tests;

pub use self::{events::insert_event, spent::SpentSave};

// What one has that others use, as when they were one file.
use self::{notes::*, projects::*, schema::*, sessions::*, spent::*};

/// SQLite's `auto_vacuum` mode in which `PRAGMA incremental_vacuum` gives
/// free pages back.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

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
        // freed (`fold_events`), so the file doesn't stay at its largest,
        // most of it free. Switching an existing file to that takes one
        // VACUUM, done once (73 ms for a 54 MB file, well within what hooks
        // wait). Older rosters open it just the same.
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
    /// with a pause between, for hooks' reports to be written meanwhile;
    /// then the log of those writes, which holds as much again until no
    /// roster has the file open (the TUI may run for days), gives its room
    /// back too.
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

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
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
