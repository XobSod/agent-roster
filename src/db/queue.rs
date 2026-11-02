//! Prompts sent to a session, waiting in its queue to be typed in, one at a
//! time, by whichever roster process gets to each first.

use super::*;
use crate::model::Queued;

impl Db {
    /// Puts `prompt` at the end of session `id`'s queue, sent by `sender`
    /// (its id and label; None: you): its id in the queue, and how many were
    /// ahead of it. Refused if `limit` are queued already.
    pub fn enqueue(
        &self,
        id: &str,
        prompt: &str,
        sender: Option<(&str, &str)>,
        at: i64,
        limit: usize,
    ) -> Result<(i64, usize)> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        // Archived since it was looked at: its queue went.
        let live: Option<bool> = tx
            .query_row(
                "SELECT deleted_at IS NULL FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if live != Some(true) {
            bail!("it's archived");
        }
        let ahead: i64 = tx.query_row(
            "SELECT count(*) FROM queued WHERE session_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        let ahead = ahead as usize;
        if ahead >= limit {
            let queued = match ahead {
                1 => "1 prompt is".to_string(),
                n => format!("{n} prompts are"),
            };
            bail!("{queued} queued for it already, as many as `queue_limit` allows");
        }
        tx.execute(
            "INSERT INTO queued (session_id, prompt, sender, sender_label, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, prompt, sender.map(|s| s.0), sender.map(|s| s.1), at],
        )?;
        let queued = tx.last_insert_rowid();
        tx.commit()?;
        Ok((queued, ahead))
    }

    /// Session `id`'s queue, first to go in first.
    pub fn queue(&self, id: &str) -> Result<Vec<Queued>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, prompt, sender, sender_label, sent_at FROM queued
             WHERE session_id = ?1 ORDER BY id",
        )?;
        let queued = stmt.query_map([id], queued_from_row)?;
        Ok(queued.collect::<rusqlite::Result<_>>()?)
    }

    /// Every session's queue, by session id: none for one with nothing
    /// queued.
    pub fn queues(&self) -> Result<HashMap<String, Vec<Queued>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, id, prompt, sender, sender_label, sent_at FROM queued
             ORDER BY id",
        )?;
        let mut queues: HashMap<String, Vec<Queued>> = HashMap::new();
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>("session_id")?, queued_from_row(r)?))
        })?;
        for row in rows {
            let (id, queued) = row?;
            queues.entry(id).or_default().push(queued);
        }
        Ok(queues)
    }

    /// Takes the first of session `id`'s queue out to type in, if `begin`
    /// gives its agent the turn (`take_turn`).
    pub fn take_queued(
        &self,
        id: &str,
        begin: impl FnOnce(&mut Session) -> bool,
    ) -> Result<Option<Turn<Queued>>> {
        let take = |tx: &Connection, s: &mut Session| -> Result<Option<Queued>> {
            let first = tx
                .query_row(
                    "SELECT id, prompt, sender, sender_label, sent_at FROM queued
                     WHERE session_id = ?1 ORDER BY id LIMIT 1",
                    [id],
                    queued_from_row,
                )
                .optional()?;
            if let Some(q) = &first {
                tx.execute("DELETE FROM queued WHERE id = ?1", [q.id])?;
                s.sent = sent_by_another(q);
            }
            Ok(first)
        };
        self.take_turn(id, take, begin)
    }

    /// The turn `turn` began given back, its prompt not typed in
    /// (`give_back`): first in its queue again, as it was.
    pub fn give_back_queued(&self, turn: &Turn<Queued>) -> Result<bool> {
        self.give_back(turn, |tx| put_back(tx, &turn.after.id, &turn.prompt))
    }

    /// Takes `q` out of session `id`'s queue to give its agent as it starts
    /// (on its command line): whether it was still there as it was.
    pub fn take_queued_as(&self, id: &str, q: &Queued) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM queued WHERE session_id = ?1 AND id = ?2 AND prompt = ?3",
            params![id, q.id, q.prompt],
        )?;
        Ok(n == 1)
    }

    /// Puts `q`, taken out of session `id`'s queue but not given its agent,
    /// back where it was, first; not into an archived session's.
    pub fn put_back_queued(&self, id: &str, q: &Queued) -> Result<()> {
        put_back(&self.conn, id, q)
    }

    /// Drops the prompt `queued` (its id) from session `id`'s queue, or with
    /// None all of it: how many went.
    pub fn drop_queued(&self, id: &str, queued: Option<i64>) -> Result<usize> {
        Ok(match queued {
            Some(q) => self.conn.execute(
                "DELETE FROM queued WHERE session_id = ?1 AND id = ?2",
                params![id, q],
            )?,
            None => self
                .conn
                .execute("DELETE FROM queued WHERE session_id = ?1", [id])?,
        })
    }

    /// Replaces the text of the prompt `queued` (its id) in session `id`'s
    /// queue, if it's still there, typed in by none: whether it was.
    pub fn edit_queued(&self, id: &str, queued: i64, prompt: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE queued SET prompt = ?3 WHERE session_id = ?1 AND id = ?2",
            params![id, queued, prompt],
        )?;
        Ok(n == 1)
    }

    /// The sessions with anything queued.
    pub fn queued_sessions(&self) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT session_id FROM queued")?;
        let ids = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(ids.collect::<rusqlite::Result<_>>()?)
    }
}

/// `Session::sent` for the turn `q` begins: its prompt, if another session
/// sent it.
pub fn sent_by_another(q: &Queued) -> Option<String> {
    q.sender.is_some().then(|| q.prompt.clone())
}

/// `Db::put_back_queued`, on `conn`.
fn put_back(conn: &Connection, id: &str, q: &Queued) -> Result<()> {
    conn.execute(
        "INSERT INTO queued (id, session_id, prompt, sender, sender_label, sent_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6 FROM sessions WHERE id = ?2 AND deleted_at IS NULL",
        params![q.id, id, q.prompt, q.sender, q.sender_label, q.sent_at],
    )?;
    Ok(())
}

fn queued_from_row(r: &Row) -> rusqlite::Result<Queued> {
    Ok(Queued {
        id: r.get("id")?,
        prompt: r.get("prompt")?,
        sender: r.get("sender")?,
        sender_label: r.get("sender_label")?,
        sent_at: r.get("sent_at")?,
    })
}
