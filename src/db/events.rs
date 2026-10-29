//! What the hooks report, folded into the sessions, and what's learned from it.

use super::*;

/// Applied events older than this are deleted.
pub(super) const EVENT_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

impl Db {
    /// Applies events reported since the last fold, in one transaction, so any
    /// number of roster processes can fold concurrently. Returns how many were applied.
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

    /// The context window an agent reported for `model`, if one did.
    pub fn learned_window(&self, model: &str) -> Result<Option<u64>> {
        Ok(self
            .meta(&format!("window:{model}"))?
            .and_then(|v| v.parse().ok()))
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

pub(super) fn events_after(
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

/// The id of the last event folded into the sessions (0 for none yet).
pub(super) fn applied_event(conn: &Connection) -> Result<i64> {
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
