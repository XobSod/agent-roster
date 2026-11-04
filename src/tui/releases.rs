//! What's new (`v r`): the notes of each release since this one, newest
//! first, as plain text, read where it's updated: `u` updates, being here
//! the agreeing.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::input;
use crate::update::notes::{heading, plain};
use crate::update::{self, Known, Published, Release};

#[derive(Default)]
pub struct ReleasesView {
    /// How far it's scrolled; and as last drawn, how far it can be and how
    /// many lines a page is.
    pub scroll: usize,
    pub scroll_max: Cell<usize>,
    pub page: Cell<usize>,
}

/// What a key asks of the app.
#[derive(Debug, PartialEq)]
pub enum Asked {
    None,
    Close,
    Update,
}

impl ReleasesView {
    pub fn on_key(&mut self, e: KeyEvent) -> Asked {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let (max, page) = (self.scroll_max.get(), self.page.get().max(1));
        // Drawn shorter since, it's no further than it can be.
        let at = self.scroll.min(max);
        let to = match (e.code, input::vertical(e)) {
            (KeyCode::Char('u'), _) if !ctrl => return Asked::Update,
            (KeyCode::Char('q'), _) => return Asked::Close,
            _ if input::cancels(e) => return Asked::Close,
            (_, Some(true)) | (KeyCode::Char('j'), _) => at + 1,
            (_, Some(false)) | (KeyCode::Char('k'), _) => at.saturating_sub(1),
            (KeyCode::PageDown | KeyCode::Char(' '), _) => at + page,
            (KeyCode::Char('f'), _) if ctrl => at + page,
            (KeyCode::Char('d'), _) if ctrl => at + page / 2,
            (KeyCode::PageUp, _) => at.saturating_sub(page),
            (KeyCode::Char('b'), _) if ctrl => at.saturating_sub(page),
            (KeyCode::Char('u'), _) if ctrl => at.saturating_sub(page / 2),
            (KeyCode::Home | KeyCode::Char('g'), _) => 0,
            (KeyCode::End | KeyCode::Char('G'), _) => max,
            _ => at,
        };
        self.scroll = to.min(max);
        Asked::None
    }

    /// What the keys do, for the footer.
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        [("j/k", "scroll"), ("u", "update"), ("Esc", "close")]
            .map(|(key, what)| (key.to_string(), what))
            .into()
    }
}

/// What an update on its thread says (`update_in_background`).
pub enum Updating {
    /// What GitHub answered: the published releases, newest first.
    Asked(Vec<Published>),
    /// What it came to, or why not.
    Done(anyhow::Result<Updated>),
}

/// What asking and updating came to.
#[derive(Debug, PartialEq)]
pub enum Updated {
    Installed(Release),
    /// None is newer.
    Newest,
    /// The newest is one the window didn't show when it was asked: left to
    /// be read first.
    Unread(Release),
}

/// Installs `newest`, the newest newer release GitHub says there is, if
/// it's `shown`, the one the window showed as such: what was read is what
/// was agreed to.
fn install_shown(
    newest: Option<Published>,
    shown: Option<&Release>,
    install: impl FnOnce(&Published) -> anyhow::Result<()>,
) -> anyhow::Result<Updated> {
    let Some(newest) = newest else {
        return Ok(Updated::Newest);
    };
    if shown != Some(&newest.version) {
        return Ok(Updated::Unread(newest.version));
    }
    install(&newest)?;
    Ok(Updated::Installed(newest.version))
}

/// Asks what's published and installs the newest over `exe`, the binary
/// that runs (`update::can_update`), if it's `shown`, the release the window
/// showed as the newest, on a thread, so the TUI goes on meanwhile; quitting
/// it abandons the update (`update::install` leaves nothing behind that the
/// next doesn't clear).
pub fn update_in_background(exe: PathBuf, shown: Option<Release>) -> Receiver<Updating> {
    let (send, updating) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let running = Release::running();
        let done = update::ask().and_then(|published| {
            let newest = published.iter().find(|p| p.version > running).cloned();
            send.send(Updating::Asked(published)).ok();
            install_shown(newest, shown.as_ref(), |p| update::install(p, &exe))
        });
        send.send(Updating::Done(done)).ok();
    });
    updating
}

/// What the window shows `width` wide, for a roster that's `running`
/// (`version` as it says it), as `known`: the notes of each release newer,
/// newest first; with none, its own, if known; else that it's the newest,
/// as far as a check that worked says. Each line with whether it's a
/// release's heading.
pub fn lines(known: &Known, running: &Release, version: &str, width: usize) -> Vec<(bool, String)> {
    let mut shown = known.newer_notes(running);
    if shown.is_empty() {
        shown.extend(known.own_notes(running));
    }
    if shown.is_empty() {
        return vec![(
            false,
            match known.checked {
                0 => format!(
                    "Nothing heard yet of what's published (this is roster {version}): u asks"
                ),
                _ => format!("roster {version} is the newest"),
            },
        )];
    }
    let mut lines = Vec::new();
    for (i, notes) in shown.into_iter().enumerate() {
        if i > 0 {
            lines.push((false, String::new()));
        }
        lines.push((true, heading(&notes.version, &notes.date)));
        lines.push((false, String::new()));
        for line in plain(&notes.text) {
            if line.code {
                lines.push((false, super::ui::truncate(&line.text, width)));
                continue;
            }
            let wrapped = super::ui::wrap(&line.text, width.saturating_sub(line.indent));
            if wrapped.is_empty() {
                lines.push((false, String::new()));
            }
            for (j, part) in wrapped.into_iter().enumerate() {
                let indent = if j == 0 { line.lead } else { line.indent };
                lines.push((false, format!("{}{part}", " ".repeat(indent))));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Notes;

    #[test]
    fn what_u_installs_is_the_release_shown_never_one_unread() {
        let release = |v: &str| Release::parse(v).unwrap();
        let published = |v: &str| Published {
            version: release(v),
            date: "2026-11-20".into(),
            notes: String::new(),
            assets: Vec::new(),
        };
        let installed = std::cell::RefCell::new(Vec::new());
        let update = |newest: Option<&str>, shown: Option<&str>| {
            let shown = shown.map(release);
            install_shown(newest.map(published), shown.as_ref(), |p| {
                installed.borrow_mut().push(p.version.to_string());
                Ok(())
            })
            .unwrap()
        };
        assert_eq!(
            update(Some("0.2.0"), Some("0.2.0")),
            Updated::Installed(release("0.2.0"))
        );
        assert_eq!(*installed.borrow(), ["0.2.0"]);
        installed.borrow_mut().clear();
        // Published since it was shown, or with nothing shown: to read first.
        assert_eq!(
            update(Some("0.3.0"), Some("0.2.0")),
            Updated::Unread(release("0.3.0"))
        );
        assert_eq!(
            update(Some("0.2.0"), None),
            Updated::Unread(release("0.2.0"))
        );
        assert_eq!(update(None, Some("0.2.0")), Updated::Newest);
        assert!(installed.borrow().is_empty());
    }

    fn known(versions: &[&str]) -> Known {
        Known {
            checked: 1,
            newest: versions.first().map(|v| v.to_string()),
            releases: (versions.iter())
                .map(|v| Notes {
                    version: v.to_string(),
                    date: "2026-11-20".into(),
                    text: format!("- what {v} brings, a line long enough to wrap"),
                })
                .collect(),
        }
    }

    #[test]
    fn whats_new_is_each_release_since_newest_first_else_its_own() {
        let running = Release::parse("0.1.0+2.g7056428").unwrap();
        let known = known(&["0.3.0", "0.2.0", "0.1.0"]);
        let shown = lines(&known, &running, "0.1.0+2.g7056428", 30);
        let headings: Vec<&str> = (shown.iter())
            .filter(|(heading, _)| *heading)
            .map(|(_, l)| l.as_str())
            .collect();
        assert_eq!(
            headings,
            ["roster 0.3.0 · 2026-11-20", "roster 0.2.0 · 2026-11-20"]
        );
        // Wrapped under the item's text.
        assert_eq!(shown[2].1, "• what 0.3.0 brings, a line");
        assert_eq!(shown[3].1, "  long enough to wrap");
        // Updated: its own.
        let running = Release::parse("0.3.0").unwrap();
        let shown = lines(&known, &running, "0.3.0", 80);
        assert_eq!(shown[0], (true, "roster 0.3.0 · 2026-11-20".into()));
        assert_eq!(shown.iter().filter(|(heading, _)| *heading).count(), 1);
        // Nothing known of it.
        let running = Release::parse("0.4.0").unwrap();
        assert_eq!(
            lines(&known, &running, "0.4.0", 80),
            [(false, "roster 0.4.0 is the newest".into())]
        );
    }

    #[test]
    fn keys_scroll_close_and_update() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut view = ReleasesView::default();
        view.scroll_max.set(30);
        view.page.set(10);
        assert_eq!(view.on_key(key(KeyCode::Char('j'))), Asked::None);
        assert_eq!(view.scroll, 1);
        view.on_key(key(KeyCode::PageDown));
        assert_eq!(view.scroll, 11);
        view.on_key(key(KeyCode::Char('G')));
        assert_eq!(view.scroll, 30);
        view.on_key(key(KeyCode::Down));
        assert_eq!(view.scroll, 30);
        view.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(view.scroll, 25);
        view.on_key(key(KeyCode::Char('g')));
        view.on_key(key(KeyCode::Char('k')));
        assert_eq!(view.scroll, 0);
        assert_eq!(view.on_key(key(KeyCode::Char('u'))), Asked::Update);
        assert_eq!(view.on_key(key(KeyCode::Esc)), Asked::Close);
        assert_eq!(view.on_key(key(KeyCode::Char('q'))), Asked::Close);
    }
}
