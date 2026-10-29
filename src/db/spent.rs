//! What the agents spent: their files' calls, saved by number, read back by time.

use super::*;

/// A call's id as roster keeps it: its 64-bit hash (FNV-1a, the same on
/// every machine and in every version), which tells calls apart as the id
/// does in an eighth of the room: of ten million calls, two sharing one is
/// a chance in hundreds of thousands.
pub(super) fn call_key(id: &str) -> i64 {
    crate::util::fnv1a_64(id.as_bytes()) as i64
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
}
