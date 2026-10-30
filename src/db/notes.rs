//! Notes, saved apart from the rest, and only if still as they were when asked.

use super::*;

pub const VERSIONS_KEPT: usize = 10;

impl Db {
    /// A session's or a project's note as saved now.
    pub fn note(&self, whose: &Whose) -> Result<Option<Note>> {
        let (table, id) = note_row(whose);
        let sql = format!("SELECT note, note_pin, note_at FROM {table} WHERE id = ?1");
        match self.conn.query_row(&sql, [&id], note_from_row).optional()? {
            Some(note) => Ok(note),
            None => bail!("{}", no_such(whose)),
        }
    }

    /// Saves a session's or a project's note (None removes it), and only
    /// that.
    pub fn set_note(&self, whose: &Whose, note: Option<&Note>) -> Result<()> {
        self.save_note(whose, None, note)?;
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
        self.save_note(whose, Some(expected), note)
    }

    /// Saves `note` over the note of `whose` if it's `expected`, or with
    /// None whatever it is: whether it was saved. The note it replaces is
    /// kept as a version if its text changes, the last `VERSIONS_KEPT` of
    /// them.
    fn save_note(
        &self,
        whose: &Whose,
        expected: Option<Option<&Note>>,
        note: Option<&Note>,
    ) -> Result<bool> {
        let (table, id) = note_row(whose);
        let owner = version_owner(whose);
        // Written by no other process between reading it and saving over it.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let sql = format!("SELECT note, note_pin, note_at FROM {table} WHERE id = ?1");
        let Some(current) = tx.query_row(&sql, [&id], note_from_row).optional()? else {
            bail!("{}", no_such(whose));
        };
        if expected.is_some_and(|expected| expected != current.as_ref()) {
            return Ok(false);
        }
        if let Some(old) = current.filter(|old| note.is_none_or(|n| n.text != old.text)) {
            let (removed, added) = crate::note::changed(&old.text, note.map_or("", |n| &n.text));
            tx.execute(
                &format!(
                    "INSERT INTO note_versions
                         ({owner}, note, note_pin, note_at, replaced_at, removed, added)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                ),
                params![
                    id,
                    old.text,
                    old.pin.map(|p| p as i64),
                    old.at,
                    crate::util::now_ms(),
                    removed as i64,
                    added as i64
                ],
            )?;
            tx.execute(
                &format!(
                    "DELETE FROM note_versions WHERE {owner} = ?1 AND id NOT IN
                     (SELECT id FROM note_versions WHERE {owner} = ?1 ORDER BY id DESC LIMIT ?2)"
                ),
                params![id, VERSIONS_KEPT as i64],
            )?;
        }
        tx.execute(
            &format!("UPDATE {table} SET note = ?2, note_pin = ?3, note_at = ?4 WHERE id = ?1"),
            params![
                id,
                note.map(|n| &n.text),
                note.and_then(|n| n.pin).map(|p| p as i64),
                note.map(|n| n.at)
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The versions kept of a session's or a project's note, the latest
    /// first.
    pub fn note_versions(&self, whose: &Whose) -> Result<Vec<Version>> {
        let (_, id) = note_row(whose);
        let owner = version_owner(whose);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT note, note_pin, note_at, replaced_at, removed, added FROM note_versions
             WHERE {owner} = ?1 ORDER BY id DESC"
        ))?;
        let versions = stmt
            .query_map([&id], |r| {
                let replaced_at = r.get("replaced_at")?;
                let lines = |column| {
                    r.get::<_, Option<i64>>(column)
                        .map(|n| n.map(|n| n as usize))
                };
                let change = lines("removed")?.zip(lines("added")?);
                Ok(note_from_row(r)?.map(|note| Version {
                    note,
                    replaced_at,
                    change,
                }))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<_>>()?;
        Ok(versions)
    }
}

/// A session's or a project's note, from its columns; None without text.
pub(super) fn note_from_row(r: &Row) -> rusqlite::Result<Option<Note>> {
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
pub(super) fn note_row(whose: &Whose) -> (&'static str, rusqlite::types::Value) {
    match whose {
        Whose::Session(id) => ("sessions", id.clone().into()),
        Whose::Project(id) => ("projects", (*id).into()),
    }
}

/// The column of `note_versions` saying whose note a version is of.
fn version_owner(whose: &Whose) -> &'static str {
    match whose {
        Whose::Session(_) => "session_id",
        Whose::Project(_) => "project_id",
    }
}

pub(super) fn no_such(whose: &Whose) -> String {
    match whose {
        Whose::Session(id) => format!("no session with id {id}"),
        Whose::Project(id) => format!("no project with id {id}"),
    }
}
