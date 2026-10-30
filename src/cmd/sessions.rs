//! Sessions: making, listing, showing, changing and adopting them, their titles, and typing a first prompt in.

use crate::*;

/// `roster new`.
pub fn new(
    r: &mut Roster,
    project: String,
    session: SessionArgs,
    works_in: WorksInArgs,
) -> Result<()> {
    let p = r
        .db
        .project(&project)?
        .with_context(|| format!("no project {project:?} (add it with `roster project add`)"))?;
    let (mut request, attach) = session.request(p, r.config.default_agent());
    request.works_in = works_in.works_in();
    start_session(r, request, attach)?;
    Ok(())
}

/// `roster chat`.
pub fn chat(r: &mut Roster, session: SessionArgs) -> Result<()> {
    let chats = r.chat_project()?;
    let (request, attach) = session.request(chats, r.config.default_agent());
    start_session(r, request, attach)?;
    Ok(())
}

/// `roster ls`.
pub fn list(
    r: &mut Roster,
    words: Vec<String>,
    archived: bool,
    project: Option<String>,
    sort: String,
    resources: bool,
    json: bool,
) -> Result<()> {
    r.settle()?;
    let query = words.join(" ");
    let by_attention = sort == "attention";
    // Read twice, a second apart, for the CPU used between.
    let usage = match resources {
        true => {
            let first = r.resources()?;
            if first.is_empty() {
                Some(first)
            } else {
                std::thread::sleep(Duration::from_secs(1));
                Some(r.resources()?)
            }
        }
        false => None,
    };
    let options = print::ListOptions {
        archived,
        project: project.as_deref(),
        by_attention,
        usage: usage.as_ref(),
        json,
    };
    list_sessions(r, &query, &options)?;
    Ok(())
}

/// `roster show`.
pub fn show(r: &mut Roster, session: String) -> Result<()> {
    r.refresh()?;
    let s = r.resolve(&session, false)?;
    print!("{}", r.screen(&s, io::stdout().is_terminal())?);
    Ok(())
}

/// `roster edit`.
pub fn edit(
    r: &mut Roster,
    session: String,
    name: Option<String>,
    title: Option<String>,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    let before = r.resolve(&session, false)?;
    // Only what's given changes: "" for the title follows the agent's.
    let s = r.update_session(
        &before.id,
        name.as_deref(),
        title.map(Some),
        setting(model),
        setting(effort),
    )?;
    println!("Updated {}", s.label());
    // A running agent goes on with what it started with.
    let restart = (&s.model, &s.effort) != (&before.model, &before.effort);
    if restart && s.state != State::Stopped {
        println!("The agent picks up a new model or effort on its next start");
    }
    Ok(())
}

/// `roster adopt <conversation>`.
pub fn adopt(r: &mut Roster, conversation: String, args: AdoptArgs) -> Result<()> {
    let AdoptArgs {
        project,
        name,
        defaults,
        model,
        effort,
        fork,
        yes,
        attach,
        ..
    } = args;
    let mut req = Adopt {
        conversation,
        project,
        name,
        run_on: if defaults {
            RunOn::Defaults
        } else {
            RunOn::Conversation
        },
        model: setting(model),
        effort: setting(effort),
        fork,
        force: yes,
    };
    if let Some((agent, pid)) = r.open_elsewhere(&req)? {
        let question = format!(
            "It's open in {} (pid {pid}): two would write one conversation; --fork adopts a copy. Adopt it anyway?",
            util::capitalize(&agent)
        );
        if !confirm(&question, yes)? {
            return Ok(());
        }
        req.force = true;
    }
    let s = r.adopt(req)?;
    let copy = if fork { " as a copy" } else { "" };
    println!("Adopted {}{copy} ({})", s.label(), s.id);
    if attach {
        say_notices(r);
        r.attach(&s.id)?;
    }
    Ok(())
}

/// `roster info`.
pub fn info(r: &mut Roster, session: String) -> Result<()> {
    r.refresh()?;
    let s = r.resolve_any(&session)?;
    print_info(r, &s);
    Ok(())
}

/// `roster retitle`.
pub fn retitle(r: &mut Roster, session: String, auto: bool) -> Result<()> {
    let s = r.resolve(&session, false)?;
    let title = if auto {
        r.auto_title(&s.id)?
    } else {
        r.generate_title(&s.id)?
    };
    println!("{title}");
    Ok(())
}

/// `roster read`.
pub fn read(r: &mut Roster, session: String) -> Result<()> {
    let s = r.resolve(&session, false)?;
    r.set_unread(&s.id, false)?;
    Ok(())
}

/// `roster unread`.
pub fn unread(r: &mut Roster, session: String) -> Result<()> {
    let s = r.resolve(&session, false)?;
    r.set_unread(&s.id, true)?;
    Ok(())
}

/// `roster deliver`: roster's own, in the background.
pub fn deliver(r: &mut Roster, session: String) -> Result<()> {
    let s = r.resolve(&session, false)?;
    // Its output goes nowhere: you're attached, so it says so there.
    if let Err(e) = r.wait_for_prompt(&s.id, DELIVER_TIMEOUT) {
        r.tmux.say(&s.tmux_name(), &format!("roster: {e:#}"));
        return Err(e);
    }
    Ok(())
}

/// Starts a new session, waits for its first prompt to go in, and attaches if
/// asked: then the prompt goes in from a roster left in the background, as
/// nothing refreshes while you're attached.
fn start_session(r: &mut Roster, request: NewSession, attach: bool) -> Result<()> {
    let prompted = request.prompt.is_some();
    let waits = prompted && !attach;
    let s = r.new_session(request)?;
    println!("Started {} ({})", s.label(), s.id);
    say_notices(r);
    // Given on its command line (codex): nothing to wait for.
    if prompted && agent::adapter(&s.agent).takes_prompt() {
        println!("It starts with the first prompt, given on its command line");
    } else if waits {
        if r.wait_for_prompt(&s.id, Duration::from_secs(60))? {
            println!("Sent the first prompt");
        } else if r.db.session(&s.id)?.state == State::Stopped {
            println!(
                "The agent stopped before it was ready; the prompt is typed in once it's started and ready"
            );
        } else {
            println!(
                "The agent isn't ready yet (it may be waiting for you, e.g. to trust the folder);"
            );
            println!("the prompt is typed in once it is, by the next roster command or the TUI");
        }
    }
    if attach {
        // Typed in meanwhile by a roster of its own (`Roster::attach_terminal`).
        r.attach(&s.id)?;
    }
    Ok(())
}

/// How long `roster deliver` waits for an agent to be ready for its first
/// prompt: time to answer what it asks first, a folder to trust say.
const DELIVER_TIMEOUT: Duration = Duration::from_secs(600);
