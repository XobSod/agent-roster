//! What the agents spent, as the TUI knows it: their files read on a thread
//! while a period of the usage view shows, every few seconds, those changed
//! since saved; what the period shows is summed from what's saved on a
//! thread too (a quarter of a busy machine's calls takes seconds), at once
//! and again once a reading saved something.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::db::Db;
use crate::tokens::{Period, Prices, Report, Scan};
use crate::util::now_ms;

/// How often the files are looked at again while shown, and a sum that
/// failed is tried again.
const EVERY: Duration = Duration::from_secs(10);

/// How long what's summed is shown as it is, the period it's of moving on:
/// summed again after it, saved or not.
const SLIDE: Duration = Duration::from_secs(60);

/// What reads the agents' files: each built-in agent's, changed since saved
/// in the database at `db`, opened on the reading thread, priced at
/// `prices`. Whether it saved anything.
pub fn scan(
    db: PathBuf,
    prices: Prices,
) -> impl FnOnce(&mut Scan) -> Result<bool> + Send + 'static {
    move |scan| scan.update(&Db::open(&db)?, &prices)
}

/// What sums up a period from what the database at `db` has saved, up to
/// now, opened on the summing thread.
pub fn sum(db: PathBuf) -> impl FnOnce(Period) -> Result<Report> + Send + 'static {
    move |period| {
        let now = now_ms();
        let from = now - period.days() * 86_400_000;
        let hours = Db::open(&db)?.spent_hours(from, now)?;
        Ok(Report::of(&hours, from, now, period.stretches().0))
    }
}

#[derive(Default)]
pub struct Tokens {
    /// The files read lately; on its thread while reading.
    scan: Option<Scan>,
    reading: Option<Receiver<(Scan, Result<bool>)>>,
    read: Option<Instant>,
    /// Asked to look again now.
    wanted: bool,
    /// Why they couldn't be read or saved last time, if they couldn't.
    read_error: Option<String>,
    /// Why the period couldn't be summed last time, and when, if it couldn't.
    sum_error: Option<(String, Instant)>,
    /// What a period adds up to, which, and when it was summed: shown until
    /// it's summed again.
    report: Option<(Period, Report, Instant)>,
    /// Something was saved since `report` was summed.
    stale: bool,
    summing: Option<(Period, Receiver<Result<Report>>)>,
}

impl Tokens {
    pub fn reading(&self) -> bool {
        self.reading.is_some()
    }

    pub fn summing(&self) -> bool {
        self.summing.is_some()
    }

    /// Why what was spent can't be shown as it is, if it can't.
    pub fn error(&self) -> Option<&str> {
        self.read_error
            .as_deref()
            .or(self.sum_error.as_ref().map(|(e, _)| e.as_str()))
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
    pub fn read(&mut self, update: impl FnOnce(&mut Scan) -> Result<bool> + Send + 'static) {
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

    /// Takes what was read, or summed, if it's come; whether anything has.
    pub fn arrived(&mut self) -> bool {
        let mut came = false;
        if let Some(reading) = &self.reading {
            let done = match reading.try_recv() {
                Ok(done) => Some(done),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some((
                    Scan::default(),
                    Err(anyhow::anyhow!("roster failed reading what was spent")),
                )),
            };
            if let Some((scan, read)) = done {
                self.reading = None;
                self.scan = Some(scan);
                match read {
                    Ok(saved) => {
                        self.stale |= saved;
                        self.read_error = None;
                    }
                    Err(e) => self.read_error = Some(format!("{e:#}")),
                }
                came = true;
            }
        }
        if let Some((period, summing)) = &self.summing {
            let done = match summing.try_recv() {
                Ok(done) => Some(done),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err(anyhow::anyhow!("roster failed summing what was spent")))
                }
            };
            if let Some(summed) = done {
                match summed {
                    Ok(report) => {
                        self.report = Some((*period, report, Instant::now()));
                        self.sum_error = None;
                    }
                    Err(e) => {
                        self.sum_error = Some((format!("{e:#}"), Instant::now()));
                        // To be summed again, once it's time to try.
                        self.stale = true;
                    }
                }
                self.summing = None;
                came = true;
            }
        }
        came
    }

    /// Sums up `period` on a thread, with `sum` (`sum`'s), unless it's
    /// summed already (lately, and since something was last saved), being
    /// summed, or failed a moment ago. Another period's sum is let go.
    pub fn sum(
        &mut self,
        period: Period,
        sum: impl FnOnce(Period) -> Result<Report> + Send + 'static,
    ) {
        if self.summing.as_ref().is_some_and(|(p, _)| *p == period) {
            return;
        }
        if self
            .sum_error
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < EVERY)
        {
            return;
        }
        let current = self
            .report
            .as_ref()
            .is_some_and(|(p, _, at)| *p == period && at.elapsed() < SLIDE);
        if current && !self.stale {
            return;
        }
        self.stale = false;
        let (send, summed) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(sum(period));
        });
        self.summing = Some((period, summed));
    }

    #[cfg(test)]
    pub fn summed(&mut self, period: Period, report: Report) {
        self.report = Some((period, report, Instant::now()));
    }

    /// What `period` adds up to (as last summed); None until it's been.
    pub fn report(&self, period: Period) -> Option<&Report> {
        self.report
            .as_ref()
            .filter(|(p, _, _)| *p == period)
            .map(|(_, r, _)| r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(tokens: &mut Tokens) {
        let by = Instant::now() + Duration::from_secs(10);
        while (tokens.reading() || tokens.summing()) && Instant::now() < by {
            tokens.arrived();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn report(tokens: u64) -> Report {
        let mut report = Report::default();
        report.total.tokens.input = tokens;
        report
    }

    #[test]
    fn it_reads_while_shown_and_sums_what_was_saved_on_threads() {
        let mut tokens = Tokens::default();
        assert!(!tokens.due(false));
        assert!(tokens.due(true));
        // Before anything is read, what was saved shows.
        tokens.sum(Period::Week, |_| Ok(report(1)));
        wait(&mut tokens);
        assert_eq!(
            tokens.report(Period::Week).map(|r| r.total.tokens.input),
            Some(1)
        );
        // Summed, and nothing saved since: not summed again.
        tokens.sum(Period::Week, |_| Ok(report(99)));
        assert!(!tokens.summing());
        // A reading that saved nothing changes nothing; failing, it says why.
        tokens.read(|_| Ok(false));
        assert!(!tokens.due(true));
        wait(&mut tokens);
        tokens.sum(Period::Week, |_| Ok(report(99)));
        assert!(!tokens.summing());
        tokens.read(|_| anyhow::bail!("disk full"));
        wait(&mut tokens);
        assert_eq!(tokens.error(), Some("disk full"));
        // One that saved: summed again, what was summed showing meanwhile.
        tokens.want();
        assert!(tokens.due(true));
        tokens.read(|_| Ok(true));
        wait(&mut tokens);
        tokens.sum(Period::Week, |_| {
            std::thread::sleep(Duration::from_millis(50));
            Ok(report(2))
        });
        assert_eq!(
            tokens.report(Period::Week).map(|r| r.total.tokens.input),
            Some(1)
        );
        wait(&mut tokens);
        assert_eq!(
            tokens.report(Period::Week).map(|r| r.total.tokens.input),
            Some(2)
        );
        // Another period: summed for it.
        assert_eq!(tokens.report(Period::Day), None);
        tokens.sum(Period::Day, |_| Ok(report(3)));
        wait(&mut tokens);
        assert_eq!(
            tokens.report(Period::Day).map(|r| r.total.tokens.input),
            Some(3)
        );
        assert_eq!(tokens.report(Period::Week), None);
        // A sum for another period is let go for the one shown now.
        tokens.sum(Period::Quarter, |_| {
            std::thread::sleep(Duration::from_secs(5));
            Ok(report(4))
        });
        tokens.sum(Period::Month, |_| Ok(report(5)));
        wait(&mut tokens);
        assert_eq!(
            tokens.report(Period::Month).map(|r| r.total.tokens.input),
            Some(5)
        );
        // A sum that fails says why, and isn't tried again at once; one
        // that then works clears it.
        let mut tokens = Tokens::default();
        tokens.sum(Period::Week, |_| anyhow::bail!("locked"));
        wait(&mut tokens);
        assert_eq!(tokens.error(), Some("locked"));
        tokens.sum(Period::Week, |_| Ok(report(6)));
        assert!(!tokens.summing());
        tokens.sum_error.as_mut().unwrap().1 -= EVERY;
        tokens.sum(Period::Week, |_| Ok(report(6)));
        wait(&mut tokens);
        assert_eq!(
            (
                tokens.error(),
                tokens.report(Period::Week).map(|r| r.total.tokens.input)
            ),
            (None, Some(6))
        );
    }
}
