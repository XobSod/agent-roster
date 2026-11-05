//! Where the TUI is, kept so the next one opens there (`remember_place`):
//! the selection, the view, what's folded and unfolded, a project shown
//! alone. Not what's typed or open over the list (the filter, a popup, a
//! form, a note, a question): a new TUI opening on those would be in the
//! middle of something.

use super::*;

/// Kept at most this often, but as the TUI quits or hands its terminal
/// over: a TUI killed with its terminal still opens close to where it was.
const KEEP_EVERY: Duration = Duration::from_secs(1);

/// Where the TUI is, as kept (JSON). A part missing or unreadable (a newer
/// roster's) is as a TUI opens without it; the rest still counts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct Place {
    /// The entry selected, a session or a heading, and its row: where the
    /// cursor lands if it's gone.
    #[serde(deserialize_with = "or_default")]
    selected: Option<EntryKey>,
    #[serde(deserialize_with = "or_default")]
    row: usize,
    #[serde(deserialize_with = "or_default")]
    view: Mode,
    /// The groups folded, and the cards unfolded to their subagents and
    /// shells (`l`), by session.
    #[serde(deserialize_with = "readable_folds")]
    folded: HashSet<Group>,
    #[serde(deserialize_with = "readable_unfolded")]
    unfolded: HashSet<String>,
    /// The project shown alone (`p f`).
    #[serde(deserialize_with = "or_default")]
    project: Option<i64>,
}

impl Default for Place {
    fn default() -> Self {
        Place {
            selected: None,
            row: 0,
            view: Mode::default(),
            folded: first_folds(),
            unfolded: HashSet::new(),
            project: None,
        }
    }
}

/// What reads as a `T`, else its default.
fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Those of a list that read as a `T`, the others left out; none if it
/// isn't a list.
fn each_readable<T>(value: serde_json::Value) -> Option<HashSet<T>>
where
    T: serde::de::DeserializeOwned + Eq + std::hash::Hash,
{
    let serde_json::Value::Array(values) = value else {
        return None;
    };
    Some(
        (values.into_iter())
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
    )
}

/// The folds that read, or as at first if none could.
fn readable_folds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<HashSet<Group>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(each_readable(value).unwrap_or_else(first_folds))
}

/// The cards unfolded that read.
fn readable_unfolded<'de, D: serde::Deserializer<'de>>(d: D) -> Result<HashSet<String>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(each_readable(value).unwrap_or_default())
}

impl App {
    pub(super) fn place(&self) -> Place {
        Place {
            selected: self.selected_key(),
            row: self.selected,
            view: self.mode,
            folded: self.folded.clone(),
            unfolded: self.unfolded.clone(),
            project: self.filter,
        }
    }

    /// Whether the TUI is at `kept`, but maybe for its row: sessions moving
    /// above the cursor change only that, and it counts only once its entry
    /// is gone.
    fn is_at(&self, kept: &Place) -> bool {
        kept.selected == self.selected_key()
            && kept.view == self.mode
            && kept.folded == self.folded
            && kept.unfolded == self.unfolded
            && kept.project == self.filter
    }

    /// Back at `place`, the data in: on its entry (a session in a folded
    /// group, on its heading); that gone or not shown, on the session
    /// nearest its row, as when the cursor keeps its row (`rebuild`). A
    /// project gone is neither shown alone nor folded.
    pub(super) fn take_place(&mut self, place: Place) {
        let there = |id: i64| self.projects.iter().any(|p| p.id == id);
        self.filter = place.project.filter(|&id| there(id));
        self.folded = (place.folded.into_iter())
            .filter(|g| match g {
                Group::Project(id) => there(*id),
                Group::Shelf(_) => true,
            })
            .collect();
        self.unfolded = place.unfolded;
        self.select_next = place.selected.clone();
        self.rebuild();
        if let Some(key) = &place.selected
            && self.position_of(key).is_none()
        {
            self.selected = self.session_near(place.row);
        }
        self.set_mode(place.view);
    }

    /// Back where the last TUI was, as it kept it (`keep_place`), the data
    /// in; whether it was. Not if it's not to be remembered, nor if what was
    /// kept isn't a place at all.
    pub fn reopen(&mut self, kept: Option<&str>) -> bool {
        if !self.remember_place {
            return false;
        }
        let Some(place) = kept.and_then(|k| serde_json::from_str::<Place>(k).ok()) else {
            return false;
        };
        self.place_kept = Some(place.clone());
        self.take_place(place);
        true
    }

    /// What the last TUI kept couldn't be read as this one opened (the
    /// database busy): kept for the next as it is, not written over with
    /// where this one, opened without it, goes.
    pub fn leave_place(&mut self) {
        self.place_left = true;
    }

    /// What was kept is gone (`remember_place` turned off): the next time
    /// there's anything to keep, it's kept, wherever this TUI is.
    pub fn forget_place(&mut self) {
        self.place_kept = None;
    }

    /// Where the TUI is, to `keep` (as JSON, true if kept) for the next one
    /// if it changed since it last was: `KEEP_EVERY` after it was last tried
    /// at the soonest, unless `at_once` (quitting, handing the terminal over),
    /// and then its row too. Nothing, if it's not to be remembered, or left
    /// (`leave_place`).
    pub fn keep_place(&mut self, now: Instant, at_once: bool, keep: impl FnOnce(&str) -> bool) {
        let soon = (self.place_tried).is_some_and(|at| now.duration_since(at) < KEEP_EVERY);
        if !self.remember_place || self.place_left || (soon && !at_once) {
            return;
        }
        if (self.place_kept.as_ref())
            .is_some_and(|k| self.is_at(k) && (!at_once || k.row == self.selected))
        {
            return;
        }
        self.place_tried = Some(now);
        let place = self.place();
        // Not kept (the database busy), it's tried again.
        if let Ok(json) = serde_json::to_string(&place)
            && keep(&json)
        {
            self.place_kept = Some(place);
        }
    }
}
