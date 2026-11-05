//! Newer releases of roster: asking GitHub what's published, what that
//! taught kept in the database for every roster process to see, and
//! installing the newest over the binary that runs.

mod github;
mod install;
pub mod notes;

use std::cmp::Ordering;
use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::Db;

pub use github::{Published, ask};
pub use install::{can_update, install};

/// What's known of the releases (`Known`), as JSON, in the meta table.
pub const KNOWN: &str = "update:known";

/// The highest release of a roster whose TUI ran (its release part), in
/// the meta table.
pub const RAN: &str = "update:ran";

/// When the TUI last tried asking (ms), whether or not it was answered, in
/// the meta table.
pub const TRIED: &str = "update:tried";

/// The newest release whose notes the TUI showed, closed since, in the meta
/// table: they open on their own only for a newer one.
pub const SHOWN: &str = "update:shown";

/// The TUI asks at most this often, and after asking that failed, this
/// long later.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const RETRY_MS: i64 = 60 * 60 * 1000;

/// A release's version, ordered as SemVer orders it: three numbers and maybe
/// a pre-release (`1.2.0-rc.1`), build metadata left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    numbers: [u64; 3],
    pre: Vec<Identifier>,
}

/// One of a pre-release's dot-separated parts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    // First, as SemVer puts a numeric part before any other.
    Numeric(u64),
    Alphanumeric(String),
}

impl Release {
    /// A version, or a tag (`v0.2.0`), with build metadata or not
    /// (`0.1.0+3.g7056428`, a checkout's, is `0.1.0`); none for one that
    /// isn't a release's.
    pub fn parse(version: &str) -> Option<Release> {
        let version = version.strip_prefix('v').unwrap_or(version);
        let version = version.split_once('+').map_or(version, |(v, _)| v);
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (version, None),
        };
        let numbers: Vec<u64> = core.split('.').map(number).collect::<Option<_>>()?;
        let pre = match pre {
            Some(pre) => pre.split('.').map(identifier).collect::<Option<_>>()?,
            None => Vec::new(),
        };
        Some(Release {
            numbers: numbers.try_into().ok()?,
            pre,
        })
    }

    /// This build's: the release it is, or for a checkout's, the one it's
    /// past.
    pub fn running() -> Release {
        Release::parse(env!("ROSTER_VERSION")).expect("build.rs gives a SemVer version")
    }

    /// Whether it's a pre-release (`1.2.0-rc.1`).
    pub fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }
}

fn number(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

fn identifier(part: &str) -> Option<Identifier> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    Some(match number(part) {
        Some(n) => Identifier::Numeric(n),
        None => Identifier::Alphanumeric(part.to_string()),
    })
}

impl Ord for Release {
    fn cmp(&self, other: &Release) -> Ordering {
        let pre = match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            // A pre-release comes before its release.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            // Part by part; all alike, the one with more parts last.
            (false, false) => self.pre.cmp(&other.pre),
        };
        self.numbers.cmp(&other.numbers).then(pre)
    }
}

impl PartialOrd for Release {
    fn partial_cmp(&self, other: &Release) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let [major, minor, patch] = self.numbers;
        write!(f, "{major}.{minor}.{patch}")?;
        for (i, part) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match part {
                Identifier::Numeric(n) => write!(f, "{n}")?,
                Identifier::Alphanumeric(s) => f.write_str(s)?,
            }
        }
        Ok(())
    }
}

/// What the last asking learned of the published releases (`KNOWN`): when,
/// the newest, and the notes of those as new as the roster that asked or
/// newer, newest first. Its own are kept so that, once it's been replaced,
/// the release it was replaced by has its notes to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Known {
    /// When, in ms; 0: never.
    pub checked: i64,
    pub newest: Option<String>,
    pub releases: Vec<Notes>,
}

/// A release's notes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notes {
    pub version: String,
    /// When it was published, as `2026-11-01`.
    pub date: String,
    /// As written, in Markdown.
    pub text: String,
}

impl Known {
    /// As kept in the meta table; nothing known if it can't be read.
    pub fn parse(text: Option<&str>) -> Known {
        text.and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default()
    }

    /// What asking at `now` learned of `published` (newest first), asked
    /// by `running`.
    pub fn learned(published: &[Published], running: &Release, now: i64) -> Known {
        Known {
            checked: now,
            newest: published.first().map(|p| p.version.to_string()),
            releases: (published.iter())
                .filter(|p| p.version >= *running)
                .map(|p| Notes {
                    version: p.version.to_string(),
                    date: p.date.clone(),
                    text: p.notes.clone(),
                })
                .collect(),
        }
    }

    /// The newest release, if it's newer than `running`.
    pub fn newer(&self, running: &Release) -> Option<Release> {
        let newest = Release::parse(self.newest.as_deref()?)?;
        (newest > *running).then_some(newest)
    }

    /// The notes of each release newer than `running`, newest first.
    pub fn newer_notes(&self, running: &Release) -> Vec<&Notes> {
        (self.releases.iter())
            .filter(|n| Release::parse(&n.version).is_some_and(|v| v > *running))
            .collect()
    }

    /// `running`'s own notes, if they're known.
    pub fn own_notes(&self, running: &Release) -> Option<&Notes> {
        (self.releases.iter()).find(|n| Release::parse(&n.version).as_ref() == Some(running))
    }
}

/// Whether the TUI asks again, having last been answered at `checked` and
/// last tried at `tried` (ms; 0: never): once a day at most, and an hour
/// after a try that failed. A time to come (the clock was put back since)
/// isn't waited for.
pub fn due(checked: i64, tried: i64, now: i64) -> bool {
    let since = |at: i64, wait: i64| now - at >= wait || at > now;
    since(checked, DAY_MS) && since(tried, RETRY_MS)
}

/// What a TUI starting as `running` says, by the highest release that ran
/// (`RAN`): the release it was updated to, if it's higher (nothing the
/// first time, with nothing to tell it by); and whether `running` is to be
/// kept as the highest. An older build run meanwhile changes nothing, so
/// going back to the newer doesn't say it again.
pub fn updated(ran: Option<&str>, running: &Release) -> (Option<Release>, bool) {
    match ran.and_then(Release::parse) {
        Some(ran) if ran >= *running => (None, false),
        Some(_) => (Some(running.clone()), true),
        None => (None, true),
    }
}

/// Asks GitHub now, keeps what it learned in `db`, and says which
/// published releases are newer than this build, newest first.
pub fn newer(db: Option<&Db>) -> Result<Vec<Published>> {
    let running = Release::running();
    let published = ask()?;
    // What's said of them holds whether or not it's kept for others.
    if let Some(db) = db {
        keep(db, &published, crate::util::now_ms());
    }
    Ok((published.into_iter())
        .filter(|p| p.version > running)
        .collect())
}

/// Keeps in `db`, for every roster, what asking at `now` learned of
/// `published` (newest first); the text kept, if it was.
pub fn keep(db: &Db, published: &[Published], now: i64) -> Option<String> {
    let known = Known::learned(published, &Release::running(), now);
    let json = serde_json::to_string(&known).ok()?;
    db.set_meta(KNOWN, &json).ok()?;
    Some(json)
}

/// Keeps in `db` that the notes of `newest` and those before it were shown
/// (`SHOWN`), in the TUI or printed, unless a newer one's were.
pub fn keep_shown(db: &Db, newest: &Release) -> Result<()> {
    let kept = db.meta(SHOWN)?.as_deref().and_then(Release::parse);
    if kept.as_ref() < Some(newest) {
        db.set_meta(SHOWN, &newest.to_string())?;
    }
    Ok(())
}

/// The release newer than this build that the database knows of, read
/// without changing it, for `--version` to say; none if there's none, or
/// the database can't be read at once.
pub fn newer_known() -> Option<Release> {
    let db = crate::paths::Paths::from_env().ok()?.db();
    let text = crate::db::peek_meta(&db, KNOWN)?;
    Known::parse(Some(&text)).newer(&Release::running())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> Release {
        Release::parse(version).unwrap()
    }

    #[test]
    fn releases_are_ordered_as_semver_orders_them() {
        let ordered = [
            "0.1.0",
            "0.1.1",
            "0.2.0-alpha",
            "0.2.0-alpha.1",
            "0.2.0-alpha.beta",
            "0.2.0-beta",
            "0.2.0-beta.2",
            "0.2.0-beta.11",
            "0.2.0-rc.1",
            "0.2.0",
            "0.10.0",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(release(pair[0]) < release(pair[1]), "{pair:?}");
        }
        assert_eq!(release("v0.2.0"), release("0.2.0"));
        assert_eq!(release("1.2.0-rc.1").to_string(), "1.2.0-rc.1");
        for wrong in ["0.1", "0.1.0.1", "v1", "0.1.x", "0.1.0-", "0.1.0-a..b", ""] {
            assert_eq!(Release::parse(wrong), None, "{wrong}");
        }
    }

    #[test]
    fn a_checkouts_build_is_the_release_its_past() {
        assert_eq!(release("0.1.0+3.g7056428.dirty"), release("0.1.0"));
        assert_eq!(release("0.1.0+g7056428").to_string(), "0.1.0");
        assert_eq!(release("1.2.0-rc.1+2.gabcdef0"), release("1.2.0-rc.1"));
        let past = release("0.1.0+12.g7056428");
        assert!(release("0.1.1") > past);
        assert!(release("0.1.0") <= past);
        // As this build's own version is read.
        assert!(Release::parse(env!("ROSTER_VERSION")).is_some());
    }

    fn known(newest: &str, versions: &[&str]) -> Known {
        Known {
            checked: 1,
            newest: Some(newest.into()),
            releases: (versions.iter())
                .map(|v| Notes {
                    version: v.to_string(),
                    date: "2026-11-01".into(),
                    text: format!("notes of {v}"),
                })
                .collect(),
        }
    }

    #[test]
    fn what_is_known_is_read_against_the_roster_reading_it() {
        let known = known("0.3.0", &["0.3.0", "0.2.0", "0.1.0"]);
        let versions = |notes: Vec<&Notes>| -> Vec<String> {
            notes.into_iter().map(|n| n.version.clone()).collect()
        };
        let old = release("0.1.0+4.g7056428");
        assert_eq!(known.newer(&old), Some(release("0.3.0")));
        assert_eq!(versions(known.newer_notes(&old)), ["0.3.0", "0.2.0"]);
        assert_eq!(known.own_notes(&old).unwrap().text, "notes of 0.1.0");
        // Updated since: the newest is its own.
        let new = release("0.3.0");
        assert_eq!(known.newer(&new), None);
        assert!(known.newer_notes(&new).is_empty());
        assert_eq!(known.own_notes(&new).unwrap().text, "notes of 0.3.0");
        assert_eq!(Known::parse(None), Known::default());
        assert_eq!(Known::parse(Some("{not json")), Known::default());
        assert_eq!(Known::default().newer(&old), None);
        let json = serde_json::to_string(&known).unwrap();
        assert_eq!(Known::parse(Some(&json)), known);
    }

    #[test]
    fn the_tui_asks_once_a_day_at_most() {
        let now = 100 * DAY_MS;
        assert!(due(0, 0, now));
        // Answered: a day later.
        assert!(!due(now, now, now));
        assert!(!due(now - DAY_MS + 60_000, now - DAY_MS + 60_000, now));
        assert!(due(now - DAY_MS, now - DAY_MS, now));
        assert!(due(now - 3 * DAY_MS, now - 3 * DAY_MS, now));
        // Tried since, and not answered: an hour after the try.
        let tried = now - RETRY_MS + 60_000;
        assert!(!due(now - 3 * DAY_MS, tried, now));
        assert!(due(now - 3 * DAY_MS, now - RETRY_MS, now));
        assert!(!due(0, tried, now));
        // Asked "tomorrow", by a clock put back since.
        assert!(due(now + DAY_MS, now + DAY_MS, now));
    }

    #[test]
    fn notes_shown_are_kept_as_the_newest_shown() {
        let db = Db::open_in_memory().unwrap();
        let kept = || db.meta(SHOWN).unwrap();
        keep_shown(&db, &release("0.3.0")).unwrap();
        assert_eq!(kept().as_deref(), Some("0.3.0"));
        // An older roster's, shown since: the newer stays.
        keep_shown(&db, &release("0.2.0")).unwrap();
        assert_eq!(kept().as_deref(), Some("0.3.0"));
        keep_shown(&db, &release("0.4.0")).unwrap();
        assert_eq!(kept().as_deref(), Some("0.4.0"));
        db.set_meta(SHOWN, "garbled").unwrap();
        keep_shown(&db, &release("0.1.0")).unwrap();
        assert_eq!(kept().as_deref(), Some("0.1.0"));
    }

    #[test]
    fn a_first_run_after_its_release_grew_says_it_was_updated() {
        let running = release("0.2.0");
        assert_eq!(
            updated(Some("0.1.0"), &running),
            (Some(running.clone()), true)
        );
        // Said once: what ran is this, from then on.
        assert_eq!(updated(Some("0.2.0"), &running), (None, false));
        // Nothing to tell it by: kept, nothing said.
        assert_eq!(updated(None, &running), (None, true));
        assert_eq!(updated(Some("garbled"), &running), (None, true));
        // Going back says nothing, and keeps the highest: coming up again
        // says nothing either.
        assert_eq!(updated(Some("0.3.0"), &running), (None, false));
        // A checkout's build, by its release: a commit more says nothing.
        let checkout = Release::parse("0.2.0+5.g7056428").unwrap();
        assert_eq!(updated(Some("0.2.0"), &checkout), (None, false));
        let grown = Release::parse("0.3.0+1.gabcdef0").unwrap();
        assert_eq!(
            updated(Some("0.2.0"), &grown),
            (Some(release("0.3.0")), true)
        );
    }
}
