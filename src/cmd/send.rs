//! `roster send`: a prompt sent to a session, and what's queued for it
//! listed, dropped or edited.

use crate::*;
use engine::Sent;

/// `roster send`.
#[allow(clippy::too_many_arguments)]
pub fn send(
    r: &mut Roster,
    session: String,
    prompt: Option<String>,
    start: bool,
    list: bool,
    drop: Option<Option<usize>>,
    yes: bool,
    edit: Option<usize>,
) -> Result<()> {
    r.refresh()?;
    let s = r.resolve_any(&session)?;
    let label = s.label();
    let queue = r.db.queue(&s.id)?;
    // Prompt `n` of its queue (from 1), as it is now.
    let nth = |n: usize| {
        (n.checked_sub(1).and_then(|i| queue.get(i))).with_context(|| match queue.len() {
            0 => format!("nothing is queued for {label}"),
            1 => format!("1 prompt is queued for {label}, so no prompt {n}"),
            k => format!("{k} prompts are queued for {label}, so no prompt {n}"),
        })
    };
    if list {
        if queue.is_empty() {
            eprintln!("Nothing queued for {label}");
            return Ok(());
        }
        let now = util::now_ms();
        let rows: Vec<Vec<String>> = (queue.iter().enumerate())
            .map(|(i, q)| {
                vec![
                    (i + 1).to_string(),
                    q.sender_label.clone().unwrap_or_else(|| "you".into()),
                    util::ago_said(now - q.sent_at),
                    print::cut(&util::one_line(&q.prompt), 60),
                ]
            })
            .collect();
        print::print_table(&["N", "FROM", "SENT", "PROMPT"], &rows);
        return Ok(());
    }
    if let Some(n) = drop {
        // What the question says, by its ids: what's sent while it's
        // answered isn't dropped unasked.
        let (question, ids) = match n {
            Some(n) => {
                let q = nth(n)?;
                let prompt = print::cut(&util::one_line(&q.prompt), 60);
                (
                    format!("Drop \"{prompt}\" from {label}'s queue?"),
                    vec![q.id],
                )
            }
            None if queue.is_empty() => {
                println!("Nothing queued for {label}");
                return Ok(());
            }
            None if queue.len() == 1 => (
                format!("Drop the prompt queued for {label}?"),
                vec![queue[0].id],
            ),
            None => (
                format!("Drop the {} prompts queued for {label}?", queue.len()),
                queue.iter().map(|q| q.id).collect(),
            ),
        };
        if !confirm(&question, yes)? {
            return Ok(());
        }
        let mut gone = 0;
        for id in ids {
            gone += r.db.drop_queued(&s.id, Some(id))?;
        }
        match (n, gone) {
            (None, 0) => println!("Nothing queued for {label}"),
            (None, 1) => println!("Dropped the prompt queued for {label}"),
            (None, k) => println!("Dropped the {k} prompts queued for {label}"),
            (Some(n), 0) => bail!("prompt {n} of {label}'s queue was typed in meanwhile"),
            (Some(n), _) => println!("Dropped prompt {n} of {label}'s queue"),
        }
        return Ok(());
    }
    if let Some(n) = edit {
        let q = nth(n)?.clone();
        let file = r.queued_file(q.id);
        let saved = note::edit_externally(&file, &q.prompt, |text| {
            let text = text.trim();
            if text.is_empty() {
                bail!("nothing left of it: --drop {n} drops it");
            }
            engine::check_queued_fits(&s, text)?;
            match r.db.edit_queued(&s.id, q.id, &q.prompt, text)? {
                db::Edited::Saved => Ok(()),
                db::Edited::Gone => bail!("prompt {n} of {label}'s queue was typed in meanwhile"),
                db::Edited::Changed => bail!("prompt {n} of {label}'s queue was changed meanwhile"),
            }
        })?;
        match saved {
            Some(()) => println!("Saved prompt {n} of {label}'s queue"),
            None => bail!("$EDITOR exited with an error; the prompt is as it was"),
        }
        return Ok(());
    }
    let prompt = prompt.context("what to send?")?;
    let prompt = match prompt.as_str() {
        "-" => io::read_to_string(io::stdin())?,
        _ => prompt,
    };
    match r.send(&s.id, &prompt, start)? {
        Sent::Typed => println!("Typed into {label}"),
        Sent::Queued { ahead, waiting } => {
            let place = match ahead {
                0 => "next".to_string(),
                1 => "1 ahead of it".to_string(),
                n => format!("{n} ahead of it"),
            };
            println!("Queued for {label} ({place}): {waiting}");
        }
    }
    Ok(())
}
