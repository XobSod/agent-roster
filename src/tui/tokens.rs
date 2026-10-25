//! What the agents spent, as the TUI knows it: their files read on a thread
//! while a period of the usage view shows, every few seconds, those changed
//! since saved; what the period shows is summed from what's saved, at once
//! and again once they're read.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::db::Db;
use crate::tokens::{Period, Report, Scan};
use crate::util::now_ms;

/// How often the files are looked at again while shown.
const EVERY: Duration = Duration::from_secs(10);

/// What reads the agents' files: each built-in agent's, changed since saved
/// in the database at `db`, opened on the reading thread.
pub fn scan(db: PathBuf) -> impl FnOnce(&mut Scan) -> Result<()> + Send + 'static {
    move |scan| scan.update(&Db::open(&db)?)
}

#[derive(Default)]
pub struct Tokens {
    /// The files read lately; on its thread while reading.
    scan: Option<Scan>,
    reading: Option<Receiver<(Scan, Result<()>)>>,
    read: Option<Instant>,
    /// Asked to look again now.
    wanted: bool,
    /// Why they couldn't be read or saved last time, if they couldn't.
    pub error: Option<String>,
    /// What the period shown adds up to, and of which period.
    report: Option<(Period, Report)>,
}

impl Tokens {
    pub fn reading(&self) -> bool {
        self.reading.is_some()
    }

    /// Looks again soon, even if it just did.
    pub fn want(&mut self) {
        self.wanted = true;
    }

    /// Whether to look now, `shown` meaning a period shows.
    pub fn due(&self, shown: bool) -> bool {
        shown
            && !self.reading()
            && (self.wanted || self.read.is_none_or(|at| at.elapsed() >= EVERY))
    }

    /// Reads what's new on a thread, with `update` (`scan`'s).
    pub fn read(&mut self, update: impl FnOnce(&mut Scan) -> Result<()> + Send + 'static) {
        let mut scan = self.scan.take().unwrap_or_default();
        let (send, scanned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read = update(&mut scan);
            send.send((scan, read))
        });
        self.reading = Some(scanned);
        self.read = Some(Instant::now());
        self.wanted = false;
    }

    /// Takes what was read if it's come; whether it has. What a period
    /// shows is summed again.
    pub fn arrived(&mut self) -> bool {
        let Some(reading) = &self.reading else {
            return false;
        };
        let (scan, read) = match reading.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => (Scan::default(), Ok(())),
        };
        self.reading = None;
        self.scan = Some(scan);
        self.error = read.err().map(|e| format!("{e:#}"));
        self.report = None;
        true
    }

    /// Sums up `period` from what `db` has saved, unless it's summed
    /// already since last read.
    pub fn sum(&mut self, period: Period, db: &Db) {
        if self.report.as_ref().is_some_and(|(p, _)| *p == period) {
            return;
        }
        let now = now_ms();
        let from = now - period.days() * 86_400_000;
        match db.spent_hours(from, now) {
            Ok(hours) => {
                let report = Report::of(&hours, from, now, period.stretches().0);
                self.report = Some((period, report));
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    #[cfg(test)]
    pub fn summed(&mut self, period: Period, report: Report) {
        self.report = Some((period, report));
    }

    /// What `period` adds up to; None until it's been summed.
    pub fn report(&self, period: Period) -> Option<&Report> {
        self.report
            .as_ref()
            .filter(|(p, _)| *p == period)
            .map(|(_, r)| r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_while_shown_and_sums_what_was_saved() {
        let db = Db::open_in_memory().unwrap();
        let mut tokens = Tokens::default();
        assert!(!tokens.due(false));
        assert!(tokens.due(true));
        // Before anything is read, what was saved shows: nothing here.
        tokens.sum(Period::Week, &db);
        assert!(
            tokens
                .report(Period::Week)
                .is_some_and(|r| r.total.tokens.total() == 0)
        );
        tokens.read(|_| anyhow::bail!("disk full"));
        assert!(!tokens.due(true));
        let by = Instant::now() + Duration::from_secs(10);
        while !tokens.arrived() && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!tokens.reading());
        assert_eq!(tokens.error.as_deref(), Some("disk full"));
        // Read: summed again; just read, not due again unless wanted.
        assert_eq!(tokens.report(Period::Week), None);
        assert!(!tokens.due(true));
        tokens.want();
        assert!(tokens.due(true));
        tokens.sum(Period::Day, &db);
        assert!(tokens.report(Period::Day).is_some());
        assert_eq!(tokens.report(Period::Week), None);
    }
}
