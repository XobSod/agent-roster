//! `roster update`: the newest release of roster, asked for and installed.

use crate::*;

/// `roster update`: asks GitHub now, and installs the newest release over
/// this binary if it's newer, after the notes of each release newer;
/// `check`: only says whether there's one, and those notes. What it learns
/// is kept for every roster, in a database that opens.
pub fn update(check: bool) -> Result<()> {
    let exe = match check {
        true => None,
        false => Some(update::can_update()?),
    };
    let db = paths::Paths::from_env().ok().and_then(|p| {
        std::fs::create_dir_all(&p.state_dir).ok()?;
        db::Db::open(&p.db()).ok()
    });
    let newer = update::newer(db.as_ref())?;
    let Some(newest) = newer.first() else {
        println!("roster {} is the newest", env!("ROSTER_VERSION"));
        return Ok(());
    };
    let notes = newer
        .iter()
        .map(|p| update::notes::said(&p.version.to_string(), &p.date, &p.notes));
    let Some(exe) = exe else {
        println!("roster {} is out: roster update", newest.version);
        for said in notes {
            println!("\n{said}");
        }
        return Ok(());
    };
    for said in notes {
        println!("{said}\n");
    }
    update::install(newest, &exe)?;
    println!(
        "Installed roster {} at {}: what's running goes on as it was, and uses it from its next start",
        newest.version,
        util::short_path(&exe)
    );
    Ok(())
}
