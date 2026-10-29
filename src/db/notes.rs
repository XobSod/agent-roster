//! Notes, saved apart from the rest, and only if still as they were when asked.

use super::*;

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

pub(super) fn no_such(whose: &Whose) -> String {
    match whose {
        Whose::Session(id) => format!("no session with id {id}"),
        Whose::Project(id) => format!("no project with id {id}"),
    }
}
