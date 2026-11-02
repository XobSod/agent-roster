//! Prompts sent to a session, waiting in its queue to be typed in, one at a
//! time, by whichever roster process gets to each first.

use super::*;
use crate::model::{Queued, SENT, words};

/// How long before roster's record of a prompt it typed in its agent's hook
/// can say it took it: the record comes once typing's over.
const HEARD_BEFORE_RECORD_MS: i64 = 5000;

/// What a session's agent took, as `Db::sent_to` reads it.
#[derive(Default)]
struct Taken {
    prompts: Vec<SentPrompt>,
    /// Those of `prompts` recorded as typed in, not yet heard of from its
    /// agent: the same prompt heard is one of these.
    unheard: Vec<usize>,
    /// The last of `prompts` heard with no record of it, and when: its
    /// record may be on its way.
    heard: Option<(usize, i64)>,
}

impl Taken {
    /// `prompt` recorded at `at` as typed into it, sent by `from`.
    fn sent(&mut self, prompt: String, from: Option<String>, at: i64) {
        match self.heard.take() {
            Some((i, heard))
                if at - heard <= HEARD_BEFORE_RECORD_MS
                    && words(&self.prompts[i].0) == words(&prompt) =>
            {
                self.prompts[i].1 = from;
            }
            _ => {
                self.unheard.push(self.prompts.len());
                self.prompts.push((prompt, from));
            }
        }
    }

    /// `prompt` heard from its agent at `at`, as it took it.
    fn heard(&mut self, prompt: String, at: i64) {
        let said = words(&prompt);
        let sent = (self.unheard.iter()).position(|&i| words(&self.prompts[i].0) == said);
        match sent {
            Some(sent) => {
                self.unheard.remove(sent);
            }
            None => {
                self.heard = Some((self.prompts.len(), at));
                self.prompts.push((prompt, None));
            }
        }
    }
}

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
    /// queue with `prompt`, if it's still there, typed in by none, and as
    /// `base`, the text the edit began from, changed by none.
    pub fn edit_queued(&self, id: &str, queued: i64, base: &str, prompt: &str) -> Result<Edited> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let saved = tx.execute(
            "UPDATE queued SET prompt = ?4 WHERE session_id = ?1 AND id = ?2 AND prompt = ?3",
            params![id, queued, base, prompt],
        )?;
        let there: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM queued WHERE session_id = ?1 AND id = ?2)",
            params![id, queued],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(match (saved, there) {
            (1, _) => Edited::Saved,
            (_, true) => Edited::Changed,
            (_, false) => Edited::Gone,
        })
    }

    /// The prompts each of sessions `ids`' agents took, as far as roster
    /// knows, by session, oldest first, each with what its sender was called
    /// (None: you): those typed in from its queue, and those its agent said
    /// it took (a prompt of yours like one sent is told from it by this:
    /// each record is one prompt heard). None for a session with none. One
    /// look for all of them: the events aren't indexed by session.
    pub fn sent_to(&self, ids: &[&str]) -> Result<HashMap<String, Vec<SentPrompt>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT session_id, kind, at, json_extract(payload, '$.prompt'),
                    json_extract(payload, '$.from'), json_extract(payload, '$.label')
             FROM events WHERE kind IN (?, 'prompt') AND session_id IN ({}) AND json_valid(payload)
             ORDER BY id",
            vec!["?"; ids.len()].join(", ")
        ))?;
        let params = std::iter::once(SENT).chain(ids.iter().copied());
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
            let from: Option<String> = r.get(4)?;
            let label: Option<String> = r.get(5)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                from.map(|f| label.unwrap_or(f)),
            ))
        })?;
        let mut taken: HashMap<String, Taken> = HashMap::new();
        for row in rows {
            if let (id, kind, at, Some(prompt), from) = row? {
                let taken = taken.entry(id).or_default();
                match kind == SENT {
                    true => taken.sent(prompt, from, at),
                    false => taken.heard(prompt, at),
                }
            }
        }
        Ok((taken.into_iter())
            .map(|(id, taken)| (id, taken.prompts))
            .collect())
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

/// A prompt typed in from a queue, and what the session that sent it was
/// called (None: you).
pub type SentPrompt = (String, Option<String>);

/// What came of editing a queued prompt (`Db::edit_queued`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edited {
    Saved,
    /// Typed in meanwhile, or dropped: there's nothing to save it into.
    Gone,
    /// Edited by another meanwhile, which this would undo.
    Changed,
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
