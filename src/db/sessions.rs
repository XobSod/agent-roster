//! Sessions: their rows, saved column by column, first prompts claimed once, subagents and shells settled.

use super::*;

/// The columns a session's state changes with (`Session::set_state`), the
/// mark the end of a turn leaves, and what a refresh saw it by
/// (`save_observed`).
const STATED_COLUMNS: &[&str] = &[
    "state",
    "state_since",
    "turns",
    "worked_ms",
    "asked_by",
    "subagents",
    "shells",
    "awaiting",
    "unread",
    "exit_status",
    "last_event_at",
    "screen_hash",
    "screen_changed_at",
    "sent",
];

pub(super) const SESSION_COLUMNS: &str =
    "s.id, s.project_id, p.name AS project, s.name, s.agent, s.workdir, s.conversation_id,
    s.transcript_path, s.state, s.unread, s.exit_status, s.last_event_at, s.screen_hash,
    s.screen_changed_at, s.created_at, s.deleted_at, s.baseline_at, s.title, s.model, s.effort,
    s.state_since, s.pending_prompt, s.running_model, s.running_effort, s.title_override, s.turns,
    s.worked_ms, s.fork_from, s.asked_by, s.given_prompt, s.sent, s.subagents, s.awaiting, s.shells,
    s.started_at, s.session_at, s.conversation_begun, s.note, s.note_pin, s.note_at, s.worktree";

impl Db {
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
    /// set) only if asked.
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
    pub fn save_changes(&self, before: &Session, after: &Session) -> Result<()> {
        self.save_changed(before, after, false)
    }

    /// `save_changes` for what a refresh saw of a session: its state saved
    /// only if it's still the state the refresh read (`before`), heard of
    /// and seen as it was then, and the next refresh sees it otherwise. So a
    /// turn another roster gave its agent meanwhile (`take_turn`), or a
    /// report it took, isn't undone by what was seen before it.
    pub fn save_observed(&self, before: &Session, after: &Session) -> Result<()> {
        self.save_changed(before, after, true)
    }

    // A field taken out of `after` below but not compared is an error.
    #[deny(unused_variables)]
    fn save_changed(&self, before: &Session, after: &Session, observed: bool) -> Result<()> {
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
            conversation_begun,
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
            started_at,
            session_at,
            running_model,
            running_effort,
            title_override,
            turns,
            worked_ms,
            fork_from,
            asked_by,
            given_prompt,
            sent,
            subagents,
            shells,
            awaiting,
            worktree,
        } = after;
        let state_name = state.as_str();
        let subagents_text = subagents_text(subagents);
        let shells_text = shells_text(shells);
        let worktree_text = worktree_text(worktree);
        let mut set: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if before.state != *state {
            set.push(("state", &state_name));
        }
        if before.subagents != *subagents {
            set.push(("subagents", &subagents_text));
        }
        if before.shells != *shells {
            set.push(("shells", &shells_text));
        }
        if before.worktree != *worktree {
            set.push(("worktree", &worktree_text));
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
            conversation_begun,
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
            started_at,
            session_at,
            running_model,
            running_effort,
            title_override,
            turns,
            worked_ms,
            fork_from,
            asked_by,
            given_prompt,
            sent,
            awaiting
        );
        // What changes with its state (`Session::set_state`), and what it was
        // seen by: kept apart if only the state read is to go, and written
        // first, while what's to be compared is as another roster left it.
        let guarded = observed && before.state != *state;
        let (stated, set): (Vec<_>, Vec<_>) =
            (set.into_iter()).partition(|(column, _)| guarded && STATED_COLUMNS.contains(column));
        let before_state = before.state.as_str();
        let was = [
            &before_state as &dyn rusqlite::ToSql,
            &before.state_since,
            &before.last_event_at,
            &before.screen_changed_at,
        ];
        let condition = " AND state = ?2 AND state_since IS ?3 AND last_event_at IS ?4
             AND screen_changed_at IS ?5";
        for (set, condition) in [(stated, condition), (set, "")] {
            if set.is_empty() {
                continue;
            }
            let first = if condition.is_empty() { 2 } else { 6 };
            let columns: Vec<String> = (first..)
                .zip(&set)
                .map(|(i, (column, _))| format!("{column} = ?{i}"))
                .collect();
            let sql = format!(
                "UPDATE sessions SET {} WHERE id = ?1{condition}",
                columns.join(", ")
            );
            let mut values: Vec<&dyn rusqlite::ToSql> = vec![id];
            if !condition.is_empty() {
                values.extend(was);
            }
            values.extend(set.iter().map(|(_, value)| *value));
            self.conn.execute(&sql, values.as_slice()).map_err(|e| {
                constraint(e, || {
                    format!(
                        "project {:?} already has a session named {:?}",
                        after.project, after.name
                    )
                })
            })?;
        }
        Ok(())
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

    /// Session `before` claimed to start, as `starting` says (when, and its
    /// screen's baseline), saved before its agent runs: if it's still
    /// stopped, as it was, and not started since. False if another roster is
    /// starting it meanwhile.
    pub fn claim_start(&self, before: &Session, starting: &Session) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE sessions SET started_at = ?2, baseline_at = ?3
             WHERE id = ?1 AND state = 'stopped' AND started_at IS ?4 AND deleted_at IS NULL",
            params![
                before.id,
                starting.started_at,
                starting.baseline_at,
                before.started_at
            ],
        )?;
        Ok(n == 1)
    }

    /// Takes session `id`'s first prompt to type in, if `begin` gives its
    /// agent the turn (`take_turn`).
    pub fn take_pending_prompt(
        &self,
        id: &str,
        begin: impl FnOnce(&mut Session) -> bool,
    ) -> Result<Option<Turn<String>>> {
        // Yours: its turn as yours are.
        let take = |_: &Connection, s: &mut Session| {
            s.sent = None;
            Ok(s.pending_prompt.take())
        };
        self.take_turn(id, take, begin)
    }

    /// `take` takes what's to be typed into session `id`'s agent, if `begin`
    /// gives it the turn: the session read again and saved as they change
    /// it, in one transaction holding the database, so of rosters about to
    /// type into it at once, one does, into a turn of its own. None if
    /// there's nothing to take, or `begin` gives no turn.
    pub(super) fn take_turn<T>(
        &self,
        id: &str,
        take: impl FnOnce(&Connection, &mut Session) -> Result<Option<T>>,
        begin: impl FnOnce(&mut Session) -> bool,
    ) -> Result<Option<Turn<T>>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let Some(mut s) = load_session(&tx, id)? else {
            return Ok(None);
        };
        let before = s.clone();
        let Some(prompt) = take(&tx, &mut s)? else {
            return Ok(None);
        };
        if !begin(&mut s) {
            return Ok(None);
        }
        save_session(&tx, &s)?;
        tx.commit()?;
        Ok(Some(Turn {
            prompt,
            before,
            after: s,
        }))
    }

    /// The turn `turn` began given back, its first prompt not typed in
    /// (`give_back`): put back unless another was given meanwhile.
    pub fn give_back_pending(&self, turn: &Turn<String>) -> Result<bool> {
        self.give_back(turn, |tx| {
            tx.execute(
                "UPDATE sessions SET pending_prompt = ?2 WHERE id = ?1 AND pending_prompt IS NULL",
                params![turn.after.id, turn.prompt],
            )?;
            Ok(())
        })
    }

    /// The turn `turn` began given back, its prompt not typed in, in one
    /// transaction holding the database: `put_back` puts the prompt back
    /// where it was taken from, if the session is still live, and its state
    /// is as it was before the turn, if no other roster changed it since.
    /// Whether the prompt was put back.
    pub(super) fn give_back<T>(
        &self,
        turn: &Turn<T>,
        put_back: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let Some(now) = load_session(&tx, &turn.after.id)?.filter(|s| s.deleted_at.is_none())
        else {
            return Ok(false);
        };
        put_back(&tx)?;
        let stated = |s: &Session| (s.state, s.state_since, s.last_event_at, s.screen_changed_at);
        if stated(&now) == stated(&turn.after) {
            let before = Session {
                pending_prompt: turn.after.pending_prompt.clone(),
                ..turn.before.clone()
            };
            self.save_changes(&turn.after, &before)?;
        } else {
            // Whose the turn under way is, as it was, unless that changed too.
            tx.execute(
                "UPDATE sessions SET sent = ?2 WHERE id = ?1 AND sent IS ?3",
                params![turn.after.id, turn.before.sent, turn.after.sent],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

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
                 WHERE s.deleted_at IS NULL AND (s.subagents IS NOT NULL OR s.awaiting)"
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

    /// Ends the shells sessions' agents left running that `ended` says have
    /// ended, each by when (`Session::shells_ended`): those whose end no hook
    /// said, told to the agent mid-turn; and saves how far `ended` says each
    /// session's transcript was read with none of their ends in it
    /// (`Session::shells_read_to`). Each read again under the write lock, as
    /// `settle_subagents` does.
    pub fn settle_shells(&mut self, ended: &mut dyn FnMut(&Session) -> ShellsFound) -> Result<()> {
        let running = |conn: &Connection| -> Result<Vec<Session>> {
            let sql = format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s JOIN projects p ON p.id = s.project_id
                 WHERE s.deleted_at IS NULL AND s.shells IS NOT NULL"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map([], session_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let mut found = HashMap::new();
        for s in running(&self.conn)? {
            let ShellsFound { ended, read, taken } = ended(&s);
            if !ended.is_empty() || read.is_some() || !taken.is_empty() {
                let read = read.map(|upto| (upto, s.shells.keys().cloned().collect::<Vec<_>>()));
                found.insert(s.id.clone(), (ended, read, taken));
            }
        }
        if found.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for mut s in running(&tx)? {
            let Some((ids, read, taken)) = found.get(&s.id) else {
                continue;
            };
            let ended = s.shells_ended(ids);
            let further = (read.as_ref()).is_some_and(|(upto, read)| s.shells_read_to(*upto, read));
            let left = s.ends_taken(taken);
            if ended || further || left {
                save_session(&tx, &s)?;
            }
        }
        tx.commit()?;
        Ok(())
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
}

pub(super) fn load_session(conn: &Connection, id: &str) -> Result<Option<Session>> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions s JOIN projects p ON p.id = s.project_id WHERE s.id = ?1"
    );
    Ok(conn.query_row(&sql, [id], session_from_row).optional()?)
}

pub(super) fn save_session(conn: &Connection, s: &Session) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET
             name = :name,
             conversation_id = :conversation_id,
             transcript_path = :transcript_path,
             conversation_begun = :conversation_begun,
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
             started_at = :started_at,
             session_at = :session_at,
             running_model = :running_model,
             running_effort = :running_effort,
             title_override = :title_override,
             turns = :turns,
             worked_ms = :worked_ms,
             fork_from = :fork_from,
             asked_by = :asked_by,
             given_prompt = :given_prompt,
             sent = :sent,
             subagents = :subagents,
             shells = :shells,
             awaiting = :awaiting,
             worktree = :worktree
         WHERE id = :id",
        named_params! {
            ":id": s.id,
            ":name": s.name,
            ":conversation_id": s.conversation_id,
            ":transcript_path": s.transcript_path,
            ":conversation_begun": s.conversation_begun,
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
            ":started_at": s.started_at,
            ":session_at": s.session_at,
            ":running_model": s.running_model,
            ":running_effort": s.running_effort,
            ":title_override": s.title_override,
            ":turns": s.turns,
            ":worked_ms": s.worked_ms,
            ":fork_from": s.fork_from,
            ":asked_by": s.asked_by,
            ":given_prompt": s.given_prompt,
            ":sent": s.sent,
            ":subagents": subagents_text(&s.subagents),
            ":shells": shells_text(&s.shells),
            ":awaiting": s.awaiting,
            ":worktree": worktree_text(&s.worktree),
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

/// A row of `SESSION_COLUMNS`, read by column name.
pub(super) fn session_from_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        project: r.get("project")?,
        name: r.get("name")?,
        agent: r.get("agent")?,
        workdir: r.get::<_, String>("workdir")?.into(),
        conversation_id: r.get("conversation_id")?,
        transcript_path: r.get("transcript_path")?,
        conversation_begun: r.get("conversation_begun")?,
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
        started_at: r.get("started_at")?,
        session_at: r.get("session_at")?,
        running_model: r.get("running_model")?,
        running_effort: r.get("running_effort")?,
        title_override: r.get("title_override")?,
        turns: r.get("turns")?,
        worked_ms: r.get("worked_ms")?,
        fork_from: r.get("fork_from")?,
        asked_by: r.get("asked_by")?,
        given_prompt: r.get("given_prompt")?,
        sent: r.get("sent")?,
        subagents: (r.get::<_, Option<String>>("subagents")?)
            .map(|text| subagents_from(&text))
            .unwrap_or_default(),
        shells: (r.get::<_, Option<String>>("shells")?)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default(),
        awaiting: r.get("awaiting")?,
        note: note_from_row(r)?,
        worktree: (r.get::<_, Option<String>>("worktree")?)
            .and_then(|text| serde_json::from_str(&text).ok()),
    })
}

/// A session's worktree as kept: JSON; nothing for none.
pub(super) fn worktree_text(worktree: &Option<crate::model::Worktree>) -> Option<String> {
    (worktree.as_ref()).map(|w| serde_json::to_string(w).unwrap_or_default())
}

/// What's found of a session's shells in its agent's transcript
/// (`Db::settle_shells`).
#[derive(Debug, Default, PartialEq)]
pub struct ShellsFound {
    /// Those ended, each by when.
    pub ended: Vec<(String, i64)>,
    /// How far it was read with none of their ends in it, if that's to be
    /// kept.
    pub read: Option<u64>,
    /// Ends told by what they ran (`Shell::ends_as`) a shell took: by what
    /// they ran, and where each is written, for the others to leave.
    pub taken: Vec<(String, u64)>,
}

/// A session's shells as kept: JSON, by id; nothing for none.
pub(super) fn shells_text(
    shells: &std::collections::BTreeMap<String, crate::model::Shell>,
) -> Option<String> {
    (!shells.is_empty()).then(|| serde_json::to_string(shells).unwrap_or_default())
}

/// A session's subagents as kept: JSON, by id; nothing for none.
pub(super) fn subagents_text(
    subagents: &std::collections::BTreeMap<String, crate::model::Subagent>,
) -> Option<String> {
    (!subagents.is_empty()).then(|| serde_json::to_string(subagents).unwrap_or_default())
}

/// A session's subagents from what's kept; as a build kept them that kept
/// only when each was last heard of, too. None from what isn't either.
pub(super) fn subagents_from(
    text: &str,
) -> std::collections::BTreeMap<String, crate::model::Subagent> {
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
