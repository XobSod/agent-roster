//! Carrying out what the user asked for (`Op`): what it says, or why it couldn't.

use super::*;

/// Carries out an op. Failures become messages; only terminal errors end the TUI.
pub(super) fn perform(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    op: Op,
) -> Result<()> {
    let outcome: Result<String> = match op {
        Op::ChangeSetting { setting, input } => {
            let back = input.is_none();
            let before = roster.config_text.clone();
            let changed = roster.change_setting(&setting, input.as_deref());
            if roster.config_text != before {
                use_config(roster, app);
            }
            changed.map(|()| {
                if back {
                    format!("{setting} is back to its default")
                } else {
                    format!("Set {setting}")
                }
            })
        }
        Op::EditConfig => edit_config(terminal, roster, app)?,
        Op::AddAgent(line) => {
            let before = roster.config_text.clone();
            let added = roster.add_agent(&line);
            if roster.config_text != before {
                use_config(roster, app);
            }
            added.map(|name| format!("Added the agent {name}: new sessions can run it"))
        }
        Op::Attach(id) => {
            app.attaching(&id);
            let result = attach(terminal, roster, app, &id)?.map(|prompted| {
                app.detached(prompted);
                String::new()
            });
            // Attaching sized it to the terminal: fit it to the preview again.
            app.preview.fitted.clear();
            result
        }
        Op::Start(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Starting {label}…"))?;
            roster.start(&id).map(|()| format!("Started {label}"))
        }
        Op::PromptAgain(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Starting {label}…"))?;
            roster
                .give_prompt_again(&id)
                .map(|()| format!("Started {label}, its first prompt given again"))
        }
        Op::RestartPromptAgain(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Restarting {label}…"))?;
            roster
                .restart_giving_prompt(&id)
                .map(|()| format!("Restarted {label}, its first prompt given again"))
        }
        Op::AttachPromptAgain(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Starting {label}…"))?;
            // Started meanwhile (by a roster of its own, say): attached to
            // as it is, as attaching would.
            let started =
                (roster.resolve(&id, false)).is_ok_and(|s| s.state != crate::model::State::Stopped);
            let given = if started {
                Ok(())
            } else {
                roster.give_prompt_again(&id)
            };
            match given {
                Err(e) => Err(e),
                Ok(()) => {
                    app.attaching(&id);
                    let result = attach(terminal, roster, app, &id)?.map(|prompted| {
                        app.detached(prompted);
                        String::new()
                    });
                    app.preview.fitted.clear();
                    result
                }
            }
        }
        Op::Stop(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Stopping {label}…"))?;
            roster.stop(&id).map(|stopped| {
                if stopped {
                    format!("Stopped {label}")
                } else {
                    format!("{label} wasn't running")
                }
            })
        }
        Op::Kill { session, process } => {
            let label = app.session_label(&session);
            busy(terminal, app, format!("Killing {label}…"))?;
            roster.kill(&session, process).map(|killed| match process {
                Some((pid, _)) => killed.said(&format!("process {pid} of {label}")),
                None => killed.said(&label),
            })
        }
        Op::Restart(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Restarting {label}…"))?;
            roster.restart(&id).map(|()| format!("Restarted {label}"))
        }
        Op::Archive(id) => {
            let label = app.session_label(&id);
            busy(terminal, app, format!("Archiving {label}…"))?;
            roster
                .archive(&id)
                .map(|()| format!("Archived {label}; it's under Archived"))
        }
        Op::Delete(id, how) => {
            // A live one's agent stops first, which can take a moment.
            busy(
                terminal,
                app,
                format!("Deleting {}…", app.session_label(&id)),
            )?;
            roster.delete(&id, how).map(|(s, said)| {
                [format!("Deleted {} for good", s.label())]
                    .into_iter()
                    .chain(said)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        }
        Op::AskDelete { id, step } => {
            let asked = roster.db.session(&id).and_then(|s| {
                let leaving = roster.leaving(std::slice::from_ref(&id))?;
                Ok(app.ask_leaving(
                    crate::agent::delete_question(&s),
                    &leaving,
                    step,
                    |how| Op::Delete(id.clone(), how),
                    |step| Op::AskDelete {
                        id: id.clone(),
                        step: Some(step),
                    },
                ))
            });
            match asked {
                Ok(Some(op)) => return perform(terminal, roster, app, op),
                Ok(None) => Ok(String::new()),
                Err(e) => Err(e),
            }
        }
        Op::Restore { id, name } => roster.restore(&id, name.as_deref()).map(|s| {
            app.select(EntryKey::Session(s.id.clone()));
            format!("Restored {} (stopped)", s.label())
        }),
        Op::SetUnread(id, unread) => roster.set_unread(&id, unread).map(|()| String::new()),
        Op::NewSession {
            works_in,
            project,
            agent,
            name,
            model,
            effort,
            prompt,
        } => {
            busy(terminal, app, format!("Starting {agent} in {project}…"))?;
            let prompted = prompt.is_some();
            let request = NewSession {
                works_in,
                project,
                name,
                agent: Some(agent),
                model,
                effort,
                prompt,
            };
            roster.new_session(request).map(|s| {
                app.select(EntryKey::Session(s.id.clone()));
                let prompt = match prompted {
                    false => "",
                    true if agent::adapter(&s.agent).takes_prompt() => {
                        ", the prompt on its command line"
                    }
                    true => "; the prompt is typed in once it's ready",
                };
                format!("Started {}{prompt}", s.label())
            })
        }
        Op::EditSession {
            id,
            name,
            title,
            model,
            effort,
        } => roster
            .update_session(&id, name.as_deref(), title, model, effort)
            .map(|s| {
                if s.state == crate::model::State::Stopped {
                    format!("Saved {}", s.label())
                } else {
                    format!(
                        "Saved {}; a new model or effort applies after a restart",
                        s.label()
                    )
                }
            }),
        Op::AddProject {
            name,
            path,
            agent,
            model,
            effort,
        } => roster
            .add_project(
                &name,
                Path::new(&path),
                Defaults {
                    agent,
                    model,
                    effort,
                },
            )
            .map(|p| {
                app.select_project(&p.name);
                format!("Added project {}", p.name)
            }),
        Op::EditProject {
            id,
            name,
            path,
            agent,
            model,
            effort,
        } => roster
            .update_project(
                id,
                name.as_deref(),
                path.as_deref().map(Path::new),
                agent,
                model,
                effort,
            )
            .map(|p| {
                app.select_project(&p.name);
                format!("Saved project {}", p.name)
            }),
        // Asked on a thread: the model takes a while, and the TUI goes on.
        Op::RetitleSession(id) if app.titling.iter().any(|t| t.id == id) => Err(anyhow::anyhow!(
            "Already asking for a title for {}",
            app.session_label(&id)
        )),
        Op::RetitleSession(id) => roster.title_job(&id).map(|job| {
            let agent = crate::util::capitalize(&job.agent);
            let asked_of = job.asked_of.clone();
            let (send, title) = std::sync::mpsc::channel();
            std::thread::spawn(move || send.send(job.run()));
            app.titling.push(app::Titling {
                id: id.clone(),
                title,
                asked_of,
            });
            format!("Asking {agent} for a title for {}…", app.session_label(&id))
        }),
        Op::SaveNotes {
            saves,
            close,
            unsaved,
        } => save_notes(roster, app, saves, close, unsaved),
        Op::EditNote(whose) => edit_note(terminal, roster, app, &whose)?,
        Op::ReloadNote(whose) => roster.note(&whose).map(|note| {
            let editor = app.note.as_mut().and_then(|open| open.of(&whose));
            match editor {
                Some(editor) => {
                    editor.reload(note.as_ref());
                    format!("Reloaded the note of {}", editor.label)
                }
                None => String::new(),
            }
        }),
        Op::NoteVersion { whose, step } => roster.note_versions(&whose).and_then(|mut kept| {
            let current = roster.note(&whose)?;
            let Some(editor) = app.note.as_mut().and_then(|open| open.of(&whose)) else {
                return Ok(String::new());
            };
            let label = editor.label.clone();
            let of = kept.len();
            let to = match super::note::step_to(&kept, editor.version.as_ref(), step) {
                None if of == 0 => return Ok(format!("The note of {label} has no older versions")),
                None => {
                    return Ok(format!(
                        "No earlier version: version {of} is the oldest kept"
                    ));
                }
                Some(0) => {
                    editor.reload(current.as_ref());
                    return Ok(format!("The note of {label} as saved"));
                }
                Some(to) => to,
            };
            editor.show_version(kept.swap_remove(to - 1), current.as_ref());
            Ok(format!(
                "Version {to} of {of} of the note of {label}: :w brings it back, :e! goes back to \
                 the note"
            ))
        }),
        Op::SaveQueued {
            session,
            queued,
            n,
            base,
            text,
            close,
        } => save_queued(roster, app, &session, (queued, n), (&base, &text), close),
        Op::EditQueued { session, queued, n } => {
            edit_queued(terminal, roster, app, &session, (queued, n))?
        }
        Op::DropQueued {
            session,
            queued,
            prompt,
        } => {
            let label = app.session_label(&session);
            let asked = queued.len();
            (queued.iter())
                .map(|&q| roster.db.drop_queued(&session, Some(q)))
                .sum::<Result<usize>>()
                .and_then(|gone| match (prompt, gone) {
                    (Some(p), 0) => bail!("{p} of {label}'s queue was typed in meanwhile"),
                    (Some(p), _) => Ok(format!("Dropped {p} from {label}'s queue")),
                    (None, 0) => bail!("The prompts queued for {label} were typed in meanwhile"),
                    (None, 1) if asked == 1 => Ok(format!("Dropped the prompt queued for {label}")),
                    (None, k) if k == asked => {
                        Ok(format!("Dropped the {k} prompts queued for {label}"))
                    }
                    (None, k) => Ok(format!(
                        "Dropped {k} of the {asked} prompts queued for {label}: the others were \
                         typed in meanwhile"
                    )),
                })
        }
        Op::NewChat => roster.chat_project().and_then(|chats| {
            app.chat_project = Some(chats.id);
            app.open_chat_form(&roster.projects()?, &chats);
            Ok(String::new())
        }),
        Op::FindConversations => roster.adoptable(true).map(|found| {
            app.pick_conversation(found);
            String::new()
        }),
        Op::Adopt {
            conversation,
            project,
            name,
            run_on,
            model,
            effort,
            fork,
            force,
        } => {
            busy(terminal, app, format!("Adopting into {project}…"))?;
            let request = Adopt {
                conversation,
                project: Some(project),
                name,
                run_on,
                model,
                effort,
                fork,
                force,
            };
            roster.adopt(request).map(|s| {
                app.select(EntryKey::Session(s.id.clone()));
                let copy = if fork { " as a copy" } else { "" };
                format!("Adopted {}{copy}", s.label())
            })
        }
        Op::KillOrphans => roster
            .kill_orphans()
            .map(|n| format!("Killed {n} orphaned tmux session(s)")),
        // On a thread, the TUI going on meanwhile (`update_arrived`).
        Op::Update => match (&app.updating, crate::update::can_update()) {
            (Some(_), _) => Ok("roster is being updated already".into()),
            (None, Err(e)) => Err(e),
            (None, Ok(exe)) => {
                let shown = app.known.newer(&crate::update::Release::running());
                app.say_meanwhile(Some(updating_to(shown.as_ref())));
                app.updating = Some(releases::update_in_background(exe, shown));
                Ok(String::new())
            }
        },
        Op::RemoveProject(name, how) => roster.remove_project(&name, how).map(|(_, said)| {
            [format!("Removed project {name}")]
                .into_iter()
                .chain(said)
                .collect::<Vec<_>>()
                .join("; ")
        }),
        Op::AskRemoveProject { name, step } => {
            let asked = roster.removal_question(&name).and_then(|question| {
                let project =
                    (roster.db.project(&name)?).with_context(|| format!("no project {name:?}"))?;
                let ids: Vec<String> = (roster.db.purged_with(&project)?.into_iter())
                    .map(|s| s.id)
                    .collect();
                let leaving = roster.leaving(&ids)?;
                Ok(app.ask_leaving(
                    question,
                    &leaving,
                    step,
                    |how| Op::RemoveProject(name.clone(), how),
                    |step| Op::AskRemoveProject {
                        name: name.clone(),
                        step: Some(step),
                    },
                ))
            });
            match asked {
                Ok(Some(op)) => return perform(terminal, roster, app, op),
                Ok(None) => Ok(String::new()),
                Err(e) => Err(e),
            }
        }
        Op::Quit => Ok(String::new()),
    };
    match outcome {
        Ok(message) if message.is_empty() => {}
        Ok(message) => app.flash(message),
        Err(e) => app.fail(format!("{e:#}")),
    }
    Ok(())
}

/// What the footer says while roster is updated: to which release; with
/// none known, it's asking.
fn updating_to(release: Option<&crate::update::Release>) -> String {
    match release {
        Some(release) => format!("Updating to v{release}…"),
        None => "Asking what's published…".into(),
    }
}

/// Shows a message while a slow operation runs.
pub(super) fn busy(terminal: &mut DefaultTerminal, app: &mut App, message: String) -> Result<()> {
    app.flash(message);
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(())
}

/// Hands the terminal to `run` (an editor) and takes it back. The outer result
/// is the terminal's.
pub(super) fn in_terminal<T>(terminal: &mut DefaultTerminal, run: impl FnOnce() -> T) -> Result<T> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    let ran = run();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(ran)
}

/// Saves notes from the editor in turn, each marked saved there, until one
/// can't be; then closes it if all were and `close`. What was saved, to say,
/// and `unsaved` after it as an error.
pub(super) fn save_notes(
    roster: &Roster,
    app: &mut App,
    saves: Vec<op::NoteSave>,
    close: bool,
    unsaved: Option<String>,
) -> Result<String> {
    let mut said = Vec::new();
    // What was saved before one that couldn't be is, and says so.
    let failed = |e: anyhow::Error, said: &[String]| match said.is_empty() {
        true => e,
        false => e.context(said.join("; ")),
    };
    for s in saves {
        let saved = roster.save_note(&s.whose, &s.text, s.pin, s.base.as_ref(), s.force);
        let (note, merged) = match saved {
            Ok(NoteSaved::Saved { note, merged }) => (note, merged),
            Ok(NoteSaved::Conflicts {
                merged,
                pin,
                current,
            }) => {
                let n = merged.conflicts;
                if let Some(open) = app.note.as_mut() {
                    if let Some(side) = open.side_of(&s.whose) {
                        open.show_open(side);
                    }
                    if let Some(editor) = open.of(&s.whose) {
                        editor.conflicted(&merged, pin, current.as_ref());
                    }
                }
                let s = if n == 1 { "" } else { "s" };
                let why = format!("{n} conflict{s} with the external change, marked in the note");
                return Err(failed(anyhow::anyhow!(why), &said));
            }
            Err(e) => return Err(failed(e, &said)),
        };
        if let Some(editor) = app.note.as_mut().and_then(|n| n.of(&s.whose)) {
            match merged {
                true => editor.reload(note.as_ref()),
                false => editor.saved(note.as_ref()),
            }
        }
        let with = if merged {
            ", merged with the external change"
        } else {
            ""
        };
        said.push(match note {
            Some(_) => format!("Saved the note of {}{with}", s.label),
            None => format!("{} has no note now{with}", s.label),
        });
    }
    if close {
        app.note = None;
    }
    if let Some(mut why) = unsaved {
        // After what was saved, mid-sentence.
        if let Some(first) = why.get_mut(..1) {
            first.make_ascii_lowercase();
        }
        anyhow::bail!("{}; {why}", said.join("; "));
    }
    Ok(said.join("; "))
}

/// Hands a note to `$EDITOR`: the open editor's text, which comes back into
/// it, or else the note as saved, saved again when the editor exits. The
/// outer result is the terminal's.
pub(super) fn edit_note(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    whose: &Whose,
) -> Result<Result<String>> {
    let noted = match whose {
        Whose::Session(id) => roster.resolve_any(id).map(|s| s.noted()),
        Whose::Project(id) => roster.projects().and_then(|ps| {
            let p = ps.into_iter().find(|p| p.id == *id);
            Ok(p.context("the project is gone")?.noted())
        }),
    };
    let n = match noted {
        Ok(n) => n,
        Err(e) => return Ok(Err(e)),
    };
    let file = roster.note_file(&n);
    let failed = || anyhow::anyhow!("$EDITOR exited with an error; the note is as it was");
    if let Some(editor) = app.note.as_mut().and_then(|open| open.of(whose)) {
        let text = editor.text();
        let back = in_terminal(terminal, || {
            crate::note::edit_externally(&file, &text, |back| {
                editor.replace(&back);
                Ok(())
            })
        })?;
        return Ok(back.and_then(|done| {
            done.map(|()| "Back from $EDITOR: :w saves the note".to_string())
                .ok_or_else(failed)
        }));
    }
    let text = n.note.as_ref().map_or(String::new(), |n| n.text.clone());
    let saved = in_terminal(terminal, || {
        crate::note::edit_externally(&file, &text, |back| roster.change_note(&n, &back))
    })?;
    Ok(saved.and_then(|said| said.ok_or_else(failed)))
}

/// Saves `text`, as written from `base` (`edit`: both), as prompt `n` of
/// session `id`'s queue (`which`: its id and `n`), the editor open on it
/// then as saved, and closed if `close`. If it was typed in or changed
/// meanwhile, that's said, and what the editor has is kept in a file, as
/// `$EDITOR` keeps it, the editor closed.
pub(super) fn save_queued(
    roster: &Roster,
    app: &mut App,
    id: &str,
    which: (i64, usize),
    edit: (&str, &str),
    close: bool,
) -> Result<String> {
    let ((queued, n), (base, text)) = (which, edit);
    let label = app.session_label(id);
    let text = text.trim();
    if text.is_empty() {
        bail!("Nothing is left of prompt {n}: d drops it");
    }
    crate::engine::check_queued_fits(&roster.db.session(id)?, text)?;
    let mut failed = match roster.db.edit_queued(id, queued, base, text)? {
        crate::db::Edited::Saved => None,
        crate::db::Edited::Gone => Some(format!(
            "Prompt {n} of {label}'s queue was typed in meanwhile"
        )),
        crate::db::Edited::Changed => Some(format!(
            "Prompt {n} of {label}'s queue was changed meanwhile"
        )),
    };
    if let Some(view) = app.queue.as_mut().filter(|v| v.session == id)
        && let Some(editing) = view.editing.as_mut().filter(|e| e.id == queued)
    {
        match &mut failed {
            None => {
                let note = crate::note::Note {
                    text: text.to_string(),
                    pin: None,
                    at: 0,
                };
                // Trimmed as it's kept, it's shown so.
                match crate::note::tidy(&editing.editor.text()) == text {
                    true => editing.editor.saved(Some(&note)),
                    false => editing.editor.reload(Some(&note)),
                }
                editing.base = text.to_string();
                if close {
                    view.editing = None;
                }
            }
            Some(why) => {
                let file = roster.queued_file(queued);
                // Not kept, it stays in the editor.
                let kept = crate::note::write_own(&file, &editing.editor.text())
                    .with_context(|| why.clone())?;
                *why += &format!("; what you wrote is kept in {}", kept.display());
                view.editing = None;
            }
        }
    }
    match failed {
        Some(why) => Err(anyhow::anyhow!(why)),
        None => Ok(format!("Saved prompt {n} of {label}'s queue")),
    }
}

/// Hands prompt `n` of session `id`'s queue (`which`: its id and `n`) to
/// `$EDITOR`: the open editor's text, which comes back into it, or else the
/// prompt as queued, saved when the editor exits. The outer result is the
/// terminal's.
pub(super) fn edit_queued(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
    id: &str,
    which: (i64, usize),
) -> Result<Result<String>> {
    let (queued, n) = which;
    let file = roster.queued_file(queued);
    let failed = || anyhow::anyhow!("$EDITOR exited with an error; the prompt is as it was");
    let open = (app.queue.as_mut())
        .filter(|v| v.session == id)
        .and_then(|v| v.editing.as_mut())
        .filter(|e| e.id == queued);
    if let Some(editing) = open {
        let text = editing.editor.text();
        let back = in_terminal(terminal, || {
            crate::note::edit_externally(&file, &text, |back| {
                editing.editor.replace(&back);
                Ok(())
            })
        })?;
        return Ok(back.and_then(|done| {
            done.map(|()| "Back from $EDITOR: :w saves the prompt".to_string())
                .ok_or_else(failed)
        }));
    }
    let label = app.session_label(id);
    let prompt = match roster.db.queue(id) {
        Ok(queue) => queue.into_iter().find(|q| q.id == queued).map(|q| q.prompt),
        Err(e) => return Ok(Err(e)),
    };
    let Some(prompt) = prompt else {
        return Ok(Err(anyhow::anyhow!(
            "Prompt {n} of {label}'s queue was typed in meanwhile"
        )));
    };
    let saved = in_terminal(terminal, || {
        crate::note::edit_externally(&file, &prompt, |back| {
            save_queued(roster, app, id, which, (&prompt, &back), true)
        })
    })?;
    Ok(saved.and_then(|said| said.ok_or_else(failed)))
}

/// Hands the terminal to `$VISUAL` or `$EDITOR` (else vi) on config.toml, made
/// first from the template of every setting if there's none, then puts what
/// was saved in use. The outer result is the terminal's.
pub(super) fn edit_config(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &mut App,
) -> Result<Result<String>> {
    let file = match roster.config_file_to_edit() {
        Ok((file, _)) => file,
        Err(e) => return Ok(Err(e)),
    };
    Ok(
        match in_terminal(terminal, || crate::util::run_editor(&file))? {
            Err(e) => Err(e),
            Ok(_) => match roster.reload_config_now() {
                Ok(Reloaded::InUse(problem)) => {
                    use_config(roster, app);
                    match problem {
                        None => Ok("config.toml saved: its settings are in use".into()),
                        Some(e) => Err(e.context(
                            "config.toml's settings are in use, but roster's tmux didn't take them",
                        )),
                    }
                }
                Ok(Reloaded::Same) => Ok("config.toml unchanged".into()),
                Err(e) => Err(e.context("config.toml has an error, the settings in use stay")),
            },
        },
    )
}

/// Hands the terminal to `tmux attach` until the user detaches. The outer result
/// is the terminal's, the inner one the attach's: whether a prompt was submitted.
///
/// tmux switches to the terminal's other screen and back by itself, so roster
/// stays on its own until then (leaving it first flashes the normal screen),
/// and draws itself again as it was the moment tmux is done, before catching
/// up on what happened meanwhile (which takes a moment: catching up first
/// shows the normal screen, then a blank one).
pub(super) fn attach(
    terminal: &mut DefaultTerminal,
    roster: &mut Roster,
    app: &App,
    id: &str,
) -> Result<Result<bool>> {
    disable_raw_mode()?;
    let view = serde_json::to_string(&app.view()).ok();
    let attached = roster.attach_terminal(id, view.as_deref());
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(attached.and_then(|since| roster.detached(id, since)))
}
