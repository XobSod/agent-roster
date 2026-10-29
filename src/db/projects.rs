//! Projects: added, saved, listed and removed, their ids never given to another.

use super::*;

pub(super) const PROJECT_COLUMNS: &str =
    "id, name, origin, agent, model, effort, note, note_pin, note_at";

impl Db {
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
}

pub(super) fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
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

/// A project's sessions, all archived, which removing it deletes for good;
/// an error if one is live.
pub(super) fn purged_with(conn: &Connection, project: &Project) -> Result<Vec<Session>> {
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

/// The highest project id given, 0 for none.
pub(super) fn last_project_id(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'last_project_id'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}
