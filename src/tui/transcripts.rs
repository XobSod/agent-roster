//! What the list and the preview read from agents' files: subagents at work, last prompts, statistics.

use super::*;

/// The last prompt of each of `group`'s sessions that may show in `rows`,
/// from their transcripts: those read before (which costs next to nothing);
/// the others are read on a thread, and show when they come
/// (`transcripts_read`).
pub(super) fn group_prompts(app: &mut App, group: Group, rows: u16) -> HashMap<String, String> {
    // As many as can show: each takes a line, and one for its prompt and
    // one for its note.
    let mut lines = 0;
    let members: Vec<crate::model::Session> = app
        .group_members(group)
        .into_iter()
        .take_while(|s| {
            let fits = lines < rows as usize;
            lines += 1 + usize::from(s.transcript_path.is_some()) + usize::from(s.note.is_some());
            fits
        })
        .cloned()
        .collect();
    let mut prompts = HashMap::new();
    let mut unread = Vec::new();
    for s in members {
        let Some(path) = s.transcript_path.as_deref().map(Path::new) else {
            continue;
        };
        match app.preview.transcripts.get_mut(&s.id) {
            Some(reader) if reader.path() == path => {
                if reader.update().is_ok()
                    && let Some(last) = reader.recent_prompts().pop()
                {
                    prompts.insert(s.id.clone(), last);
                }
            }
            // Not read again: one being read, or one that's gone (deleted
            // with its conversation).
            _ if app.preview.reading.ids.contains(&s.id) || !path.exists() => {}
            _ => {
                if let Some(reader) = agent::transcript_of(&s, path) {
                    unread.push((s.id.clone(), reader));
                }
            }
        }
    }
    if !unread.is_empty() {
        let reading = &mut app.preview.reading;
        reading.ids.extend(unread.iter().map(|(id, _)| id.clone()));
        let send = reading.send.clone();
        std::thread::spawn(move || {
            for (id, mut reader) in unread {
                let read = reader.update().is_ok().then_some(reader);
                if send.send((id, read)).is_err() {
                    return;
                }
            }
        });
    }
    prompts
}

/// What's read of the subagents cards show a line for: their tasks and
/// models, each read once it's all there (`SubagentInfo::settled`), or for
/// a while after it started; and what each is doing, each time. What no
/// card shows any more is let go, and a card whose session has no subagent
/// or shell at work any more folds.
pub(super) fn read_subagents(app: &mut App) {
    let now = crate::util::now_ms();
    let busy: HashSet<String> = (app.sessions.iter())
        .filter(|s| s.at_work(now) > 0)
        .map(|s| s.id.clone())
        .collect();
    app.unfolded.retain(|id| busy.contains(id));
    let mut shown = HashSet::new();
    let mut unread = Vec::new();
    let listed = app.mode == Mode::Sessions;
    for s in app
        .sessions
        .iter()
        .filter(|s| listed && app.at_work_shown(s, now))
    {
        let Some(path) = s.transcript_path.as_deref() else {
            continue;
        };
        for (id, subagent) in &s.subagents {
            if now - subagent.heard > crate::model::SUBAGENT_QUIET_MS {
                continue;
            }
            shown.insert(id.clone());
            let known = app.subagent_info.get(id);
            let settling = now - subagent.started <= SUBAGENT_SETTLES_MS;
            if known.is_none() || known.is_some_and(|i| !i.settled && settling) {
                unread.push((s.agent.clone(), path.to_string(), id.clone()));
            }
        }
    }
    for (name, path, id) in unread {
        let info = agent::adapter(&name).subagent_info(std::path::Path::new(&path), &id);
        app.subagent_info.insert(id, info);
    }
    app.subagent_info.retain(|id, _| shown.contains(id));
    let agents: HashMap<&str, &str> = (app.sessions.iter())
        .flat_map(|s| s.subagents.keys().map(|id| (id.as_str(), s.agent.as_str())))
        .collect();
    // One found doing nothing now (a large result read past its call)
    // keeps what it was doing.
    let doing: HashMap<String, String> = (app.subagent_info.iter())
        .filter_map(|(id, info)| {
            let adapter = agent::adapter(agents.get(id.as_str())?);
            let now = info.file.as_deref().and_then(|f| adapter.subagent_doing(f));
            Some((
                id.clone(),
                now.or_else(|| app.subagent_doing.get(id).cloned())?,
            ))
        })
        .collect();
    app.subagent_doing = doing;
}

/// Keeps the transcripts read on a thread that have come (`group_prompts`);
/// whether any did. One that couldn't be read is tried again at the next
/// refresh.
pub(super) fn transcripts_read(app: &mut App) -> bool {
    let reading = &mut app.preview.reading;
    let mut any = false;
    for (id, read) in reading.came.try_iter() {
        reading.ids.remove(&id);
        if let Some(reader) = read {
            app.preview.transcripts.insert(id, reader);
            any = true;
        }
    }
    any
}

/// Whether the session list shows a heading's preview, which is made to its
/// size and of what's been read on a thread.
pub(super) fn heading_shown(app: &App) -> bool {
    app.mode == Mode::Sessions && matches!(app.selected_entry(), Some(Entry::Heading { .. }))
}

/// What the agent can tell about a session, its transcript read incrementally:
/// its statistics and its last prompts.
pub(super) fn transcript_info(
    roster: &Roster,
    app: &mut App,
    s: &crate::model::Session,
) -> Option<(crate::stats::AgentStats, Vec<String>)> {
    let path = std::path::Path::new(s.transcript_path.as_deref()?);
    // After /clear the conversation, and its transcript, change.
    if app
        .preview
        .transcripts
        .get(&s.id)
        .is_none_or(|t| t.path() != path)
    {
        let reader = agent::transcript_of(s, path)?;
        app.preview.transcripts.insert(s.id.clone(), reader);
    }
    let reader = app.preview.transcripts.get_mut(&s.id)?;
    reader.counting();
    reader.update().ok()?;
    let stats = roster.with_window(reader.stats(s.running_model.as_deref()), s);
    Some((stats, reader.recent_prompts()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_unfolded_cards_show_of_subagents_is_read_and_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("c1/subagents");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join("agent-a1.jsonl"), "").unwrap();
        std::fs::write(
            runs.join("agent-a1.meta.json"),
            r#"{"agentType":"general-purpose","description":"Review it","model":"sonnet"}"#,
        )
        .unwrap();
        let mut app = App::new(
            Keymap::new(&Default::default()).unwrap(),
            String::new(),
            Vec::new(),
        );
        let now = crate::util::now_ms();
        let mut s = crate::model::tests::session("a");
        s.transcript_path = Some(dir.path().join("c1.jsonl").display().to_string());
        s.subagents = [(
            "a1".to_string(),
            crate::model::Subagent {
                kind: None,
                started: now,
                heard: now,
            },
        )]
        .into();
        app.set_data(Vec::new(), vec![s.clone()]);
        // Folded: nothing read.
        read_subagents(&mut app);
        assert!(app.subagent_info.is_empty());
        app.unfolded.insert(s.id.clone());
        read_subagents(&mut app);
        assert_eq!(app.subagent_info["a1"].task.as_deref(), Some("Review it"));
        // All there (it replied): not read again; till then, for a while.
        assert!(!app.subagent_info["a1"].settled);
        let replied = r#"{"type":"user","message":{"content":"go"}}
{"type":"assistant","message":{"model":"claude-sonnet-5-5"}}
"#;
        std::fs::write(runs.join("agent-a1.jsonl"), replied).unwrap();
        read_subagents(&mut app);
        assert!(app.subagent_info["a1"].settled);
        // What it's doing, read each time.
        assert!(!app.subagent_doing.contains_key("a1"));
        let working = format!(
            "{replied}{}\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/w/db.rs"}}]}}"#
        );
        std::fs::write(runs.join("agent-a1.jsonl"), working).unwrap();
        read_subagents(&mut app);
        assert_eq!(app.subagent_doing["a1"], "Reading db.rs");
        // A result too large to read past to it: still that.
        let long = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}\n",
            "x".repeat(300 << 10)
        );
        let mut grown = std::fs::read_to_string(runs.join("agent-a1.jsonl")).unwrap();
        grown += &long;
        std::fs::write(runs.join("agent-a1.jsonl"), grown).unwrap();
        read_subagents(&mut app);
        assert_eq!(app.subagent_doing["a1"], "Reading db.rs");
        std::fs::remove_file(runs.join("agent-a1.meta.json")).unwrap();
        read_subagents(&mut app);
        assert_eq!(app.subagent_info["a1"].task.as_deref(), Some("Review it"));
        let mut late = s.clone();
        late.subagents.get_mut("a1").unwrap().started = now - SUBAGENT_SETTLES_MS - 1;
        app.subagent_info.clear();
        app.set_data(Vec::new(), vec![late]);
        read_subagents(&mut app);
        assert!(!app.subagent_info["a1"].settled);
        std::fs::write(
            runs.join("agent-a1.meta.json"),
            r#"{"description":"Review it"}"#,
        )
        .unwrap();
        read_subagents(&mut app);
        assert_eq!(app.subagent_info["a1"].task, None);
        // Gone: let go, and its card folds.
        s.subagents.clear();
        app.set_data(Vec::new(), vec![s]);
        read_subagents(&mut app);
        assert!(app.subagent_info.is_empty());
        assert!(app.unfolded.is_empty());
    }

    #[test]
    fn a_headings_transcripts_are_read_on_a_thread_as_many_as_show() {
        use crate::tui::list::Shelf;
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| Some(dir.path().join(name).to_string_lossy().into_owned());
        // Not empty, so it isn't taken for an empty file on any filesystem.
        std::fs::create_dir(dir.path().join("a-directory")).unwrap();
        std::fs::write(dir.path().join("a-directory/x"), "x").unwrap();
        let mut app = App::new(
            crate::keys::Keymap::new(&crate::config::Keys::default()).unwrap(),
            "/".into(),
            vec!["claude".into()],
        );
        let sessions: Vec<_> = (0..10)
            .map(|i| {
                let mut s = crate::model::tests::session(&format!("s{i}"));
                s.state = crate::model::State::NeedsInput;
                // First come, first served: s0 on top.
                s.state_since = Some(i);
                s.transcript_path = match i {
                    // Gone, and one that can't be read.
                    0 => path("gone.jsonl"),
                    1 => path("a-directory"),
                    _ => {
                        let line =
                            format!(r#"{{"type":"user","message":{{"content":"task {i}"}}}}"#);
                        std::fs::write(dir.path().join(format!("{i}.jsonl")), line + "\n").unwrap();
                        path(&format!("{i}.jsonl"))
                    }
                };
                s
            })
            .map(|mut s| {
                // A note's line counts too.
                if s.id == "s2" {
                    s.note = Some(crate::note::Note {
                        text: "todo".into(),
                        pin: None,
                        at: 0,
                    });
                }
                s
            })
            .collect();
        app.set_data(Vec::new(), sessions);
        let shelf = Group::Shelf(Shelf::NeedsInput);
        let reading = |app: &App| {
            let mut ids: Vec<String> = app.preview.reading.ids.iter().cloned().collect();
            ids.sort();
            ids
        };
        // What's being read comes back, each with its id cleared; whether
        // any could be read.
        let come = |app: &mut App| {
            let until = Instant::now() + Duration::from_secs(5);
            let mut any = false;
            while !app.preview.reading.ids.is_empty() && Instant::now() < until {
                any |= transcripts_read(app);
                std::thread::sleep(Duration::from_millis(10));
            }
            any
        };
        // In 7 rows three show (a line each, one for its prompt, s2's one for
        // its note); the one that's gone isn't read.
        assert!(group_prompts(&mut app, shelf, 7).is_empty());
        assert_eq!(reading(&app), ["s1", "s2"]);
        assert!(come(&mut app));
        assert!(reading(&app).is_empty());
        // The one read is kept and its prompt shows; the other is tried
        // again, and isn't news when it fails again.
        assert_eq!(app.preview.transcripts.keys().collect::<Vec<_>>(), ["s2"]);
        let prompts = group_prompts(&mut app, shelf, 7);
        assert_eq!(prompts, HashMap::from([("s2".into(), "task 2".into())]));
        assert_eq!(reading(&app), ["s1"]);
        assert!(!come(&mut app));
        assert!(reading(&app).is_empty());
        assert_eq!(app.preview.transcripts.len(), 1);
    }
}
