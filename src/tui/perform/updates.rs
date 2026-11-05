//! roster updated to the newest release.

use super::*;

/// On a thread, the TUI going on meanwhile (`update_arrived`).
pub(super) fn update(app: &mut App) -> Result<String> {
    match (&app.updating, crate::update::can_update()) {
        (Some(_), _) => Ok("roster is being updated already".into()),
        (None, Err(e)) => Err(e),
        (None, Ok(exe)) => {
            let shown = app.known.newer(&crate::update::Release::running());
            app.say_meanwhile(Some(updating_to(shown.as_ref())));
            app.updating = Some(releases::update_in_background(exe, shown));
            Ok(String::new())
        }
    }
}

/// What the footer says while roster is updated: to which release; with
/// none known, it's asking.
fn updating_to(release: Option<&crate::update::Release>) -> String {
    match release {
        Some(release) => format!("Updating to v{release}…"),
        None => "Asking what's published…".into(),
    }
}
