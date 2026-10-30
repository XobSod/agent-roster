//! `roster project`: adding, changing and removing projects.

use crate::*;

/// `roster project add`.
pub fn add(r: &mut Roster, name: String, path: PathBuf, defaults: DefaultsArgs) -> Result<()> {
    let defaults = Defaults {
        agent: setting(defaults.agent).flatten(),
        model: setting(defaults.model).flatten(),
        effort: setting(defaults.effort).flatten(),
    };
    let p = r.add_project(&name, &path, defaults)?;
    println!("Added project {} ({})", p.name, p.origin.display());
    Ok(())
}

/// `roster project edit`.
pub fn edit(
    r: &mut Roster,
    project: String,
    name: Option<String>,
    path: Option<PathBuf>,
    defaults: DefaultsArgs,
) -> Result<()> {
    let p = r.projects()?.into_iter().find(|p| p.name == project);
    let p = p.with_context(|| format!("no project {project:?}"))?;
    // Only what's given changes.
    let p = r.update_project(
        p.id,
        name.as_deref(),
        path.as_deref(),
        setting(defaults.agent),
        setting(defaults.model),
        setting(defaults.effort),
    )?;
    println!("Updated project {} ({})", p.name, p.origin.display());
    Ok(())
}

/// `roster project rm`.
pub fn remove(r: &mut Roster, name: String, yes: bool) -> Result<()> {
    if !confirm(&r.removal_question(&name)?, yes)? {
        return Ok(());
    }
    let purged = r.remove_project(&name)?;
    println!("Removed project {name}");
    for s in purged {
        let conversation = s.conversation_id.as_deref().unwrap_or("none");
        let note = if s.note.is_some() {
            ", and its note"
        } else {
            ""
        };
        println!(
            "  with archived session {} (conversation {conversation}){note}",
            s.name
        );
    }
    Ok(())
}
