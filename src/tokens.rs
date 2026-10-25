//! What the agents have spent: tokens of each kind, and what they cost or
//! would at the API's prices. Read call by call from every conversation each
//! agent keeps on this machine, started in roster or not
//! (`Adapter::token_files`), and kept by the hour in roster's database, so
//! it outlasts the conversations (Claude deletes its own after 30 days
//! unless told otherwise).

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::Serialize;

use crate::db::Db;
use crate::stats::{Jsonl, Lines};

/// One model call: when, to which model, and what it took.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// The call, as its agent names it (Claude's message id, codex's
    /// response id), prefixed with the agent's name: one copied into another
    /// file (a fork, a rollout archived) is still one call.
    pub id: String,
    /// In ms.
    pub at: i64,
    /// As the agent names it; pi's with its provider (`vllm/qwen3`).
    pub model: String,
    pub counts: Counts,
    /// What the agent says it cost (pi); None if it doesn't say.
    pub cost: Option<f64>,
}

/// Tokens by kind, each apart from the others: `input` is what was sent
/// beyond what was read from the cache or written to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Counts {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Of `cache_write`, what was kept an hour rather than five minutes
    /// (Claude's), at twice input's price.
    #[serde(skip)]
    pub cache_write_hour: u64,
    pub output: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.sent() + self.output
    }

    /// What the call sent.
    fn sent(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: &Counts) {
        self.input += other.input;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.cache_write_hour += other.cache_write_hour;
        self.output += other.output;
    }
}

/// What one of an agent's files says it spent, read as the file grows.
pub trait Spending: Send {
    /// Reads what was added since (all of it the first time).
    fn update(&mut self) -> std::io::Result<()>;
    fn calls(&self) -> &[Call];
}

/// What a JSONL file says was spent, line by line (`Jsonl` does the reading).
pub trait Calls: Lines {
    fn calls(&self) -> &[Call];
}

impl<T: Calls> Spending for Jsonl<T> {
    fn update(&mut self) -> std::io::Result<()> {
        self.read()
    }

    fn calls(&self) -> &[Call] {
        self.tally().calls()
    }
}

/// What a million tokens of each kind cost, in dollars.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

const fn rates(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Rates {
    Rates {
        input,
        output,
        cache_read,
        cache_write,
    }
}

/// A model's prices: its rates, and for a call that sends more than so many
/// tokens, the rates all of that call is priced at instead.
type Price = (&'static str, Rates, Option<(u64, Rates)>);

/// The API's prices, as the model catalog of pi 1.3.0 has them
/// (`@earendil-works/pi-ai`, generated 2026-10-21), for the Claude and
/// OpenAI models the agents run.
const PRICES: &[Price] = &[
    ("claude-fable-5", rates(10.0, 50.0, 1.0, 12.5), None),
    ("claude-fable-5-1", rates(10.0, 50.0, 0.25, 12.5), None),
    ("claude-opus-4-5", rates(5.0, 25.0, 0.5, 6.25), None),
    ("claude-opus-4-6", rates(5.0, 25.0, 0.5, 6.25), None),
    ("claude-opus-4-7", rates(5.0, 25.0, 0.5, 6.25), None),
    ("claude-opus-4-8", rates(5.0, 25.0, 0.5, 6.25), None),
    ("claude-opus-5", rates(5.0, 25.0, 0.5, 6.25), None),
    ("claude-opus-5-5", rates(4.0, 20.0, 0.2, 5.0), None),
    ("claude-sonnet-4-5", rates(3.0, 15.0, 0.3, 3.75), None),
    ("claude-sonnet-4-6", rates(3.0, 15.0, 0.3, 3.75), None),
    ("claude-sonnet-5", rates(2.0, 10.0, 0.2, 2.5), None),
    ("claude-sonnet-5-5", rates(2.0, 10.0, 0.1, 2.5), None),
    ("claude-haiku-4-5", rates(1.0, 5.0, 0.1, 1.25), None),
    (
        "claude-haiku-5-5",
        rates(0.1, 0.5, 0.01, 0.125),
        Some((100_000, rates(0.5, 2.5, 0.05, 0.625))),
    ),
    ("gpt-5", rates(1.25, 10.0, 0.125, 0.0), None),
    ("gpt-5-mini", rates(0.25, 2.0, 0.025, 0.0), None),
    ("gpt-5-nano", rates(0.05, 0.4, 0.005, 0.0), None),
    ("gpt-5.1", rates(1.25, 10.0, 0.125, 0.0), None),
    ("gpt-5.2", rates(1.75, 14.0, 0.175, 0.0), None),
    ("gpt-5.3-codex", rates(1.75, 14.0, 0.175, 0.0), None),
    ("gpt-5.3-codex-spark", rates(1.75, 14.0, 0.175, 0.0), None),
    (
        "gpt-5.4",
        rates(2.5, 15.0, 0.25, 0.0),
        Some((272_000, rates(5.0, 22.5, 0.5, 0.0))),
    ),
    ("gpt-5.4-mini", rates(0.75, 4.5, 0.075, 0.0), None),
    ("gpt-5.4-nano", rates(0.2, 1.25, 0.02, 0.0), None),
    (
        "gpt-5.5",
        rates(5.0, 30.0, 0.5, 0.0),
        Some((272_000, rates(10.0, 45.0, 1.0, 0.0))),
    ),
    (
        "gpt-5.6-luna",
        rates(0.2, 1.2, 0.02, 0.25),
        Some((272_000, rates(0.4, 1.8, 0.04, 0.5))),
    ),
    (
        "gpt-5.6-terra",
        rates(2.0, 12.0, 0.2, 2.5),
        Some((272_000, rates(4.0, 18.0, 0.4, 5.0))),
    ),
    (
        "gpt-5.6-sol",
        rates(4.0, 20.0, 0.4, 5.0),
        Some((272_000, rates(8.0, 30.0, 0.8, 10.0))),
    ),
    (
        "gpt-6-luna",
        rates(0.1, 0.5, 0.01, 0.125),
        Some((272_000, rates(0.2, 0.75, 0.02, 0.25))),
    ),
    (
        "gpt-6-sol",
        rates(2.0, 10.0, 0.2, 2.5),
        Some((272_000, rates(4.0, 15.0, 0.4, 5.0))),
    ),
    (
        "gpt-6.1-sol",
        rates(2.0, 10.0, 0.1, 2.5),
        Some((272_000, rates(4.0, 15.0, 0.2, 5.0))),
    ),
    (
        "gpt-6-astra",
        rates(10.0, 50.0, 1.0, 12.5),
        Some((272_000, rates(20.0, 75.0, 2.0, 25.0))),
    ),
];

/// A model's prices, by its name without a provider (`anthropic/…`, as pi
/// names it) or a date (`…-20251001`).
fn price(model: &str) -> Option<&'static Price> {
    let name = model.rsplit('/').next().unwrap_or(model);
    let name = match name.rsplit_once('-') {
        Some((base, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => name,
    };
    PRICES.iter().find(|p| p.0 == name)
}

impl Call {
    /// What it cost, or would have at the API's prices, and what reading
    /// from the cache saved; None if no price for its model is known.
    pub fn cost(&self) -> Option<(f64, f64)> {
        let c = &self.counts;
        let priced = price(&self.model).map(|(_, normal, long)| {
            let r = match long {
                Some((above, long)) if c.sent() > *above => long,
                _ => normal,
            };
            let five_minutes = c.cache_write.saturating_sub(c.cache_write_hour);
            let cost = c.input as f64 * r.input
                + c.cache_read as f64 * r.cache_read
                + five_minutes as f64 * r.cache_write
                + c.cache_write_hour as f64 * r.input * 2.0
                + c.output as f64 * r.output;
            let saved = c.cache_read as f64 * (r.input - r.cache_read);
            (cost / 1e6, saved / 1e6)
        });
        // What the agent says it cost counts; 0 is what pi says of a model
        // it knows no price for.
        match self.cost.filter(|c| *c > 0.0) {
            Some(own) => Some((own, priced.map_or(0.0, |p| p.1))),
            None => priced,
        }
    }
}

/// What some calls spent.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Spent {
    pub tokens: Counts,
    /// In dollars, of the calls with a known price.
    pub cost: f64,
    /// What reading from the cache saved, at those prices.
    pub saved: f64,
    /// The tokens of calls whose price isn't known, not in `cost`.
    pub unpriced: u64,
}

impl Spent {
    pub fn merge(&mut self, other: &Spent) {
        self.tokens.add(&other.tokens);
        self.cost += other.cost;
        self.saved += other.saved;
        self.unpriced += other.unpriced;
    }

    /// Whether none of it has a price.
    pub fn unpriced(&self) -> bool {
        self.unpriced > 0 && self.unpriced == self.tokens.total()
    }
}

/// A stretch of time to see what was spent in, up to now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
    Quarter,
}

impl Period {
    pub const ALL: [Period; 4] = [Period::Day, Period::Week, Period::Month, Period::Quarter];

    pub fn days(self) -> i64 {
        match self {
            Period::Day => 1,
            Period::Week => 7,
            Period::Month => 30,
            Period::Quarter => 90,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "24h",
            Period::Week => "7 days",
            Period::Month => "30 days",
            Period::Quarter => "90 days",
        }
    }

    /// How many stretches its chart shows, and what one is.
    pub fn stretches(self) -> (usize, &'static str) {
        match self {
            Period::Day => (24, "an hour"),
            Period::Week => (28, "6 hours"),
            Period::Month => (30, "a day"),
            Period::Quarter => (45, "2 days"),
        }
    }
}

/// What was spent from `from` to `to`, by agent and by model.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub from: i64,
    pub to: i64,
    pub total: Spent,
    pub sessions: usize,
    /// The most tokens first.
    pub agents: Vec<AgentSpent>,
    /// The most tokens first.
    pub models: Vec<ModelSpent>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentSpent {
    pub agent: String,
    /// The conversations it spent in.
    pub sessions: usize,
    pub spent: Spent,
    /// Its tokens in each of the period's equal stretches, oldest first.
    #[serde(skip)]
    pub chart: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelSpent {
    pub agent: String,
    pub model: String,
    pub spent: Spent,
}

/// How long an hour is, in ms: what was spent is kept by the hour.
const HOUR: i64 = 3_600_000;

/// What changes how the files are read or what their calls cost (a reader
/// mended, a price added): saved with what was spent, and when it isn't
/// what was saved with, every file is read again.
pub const VERSION: &str = "1";

/// What a conversation's calls to one model spent in one hour, as summed
/// from what roster keeps (`Db::spent_hours`).
#[derive(Clone, Debug, PartialEq)]
pub struct Hour {
    pub agent: String,
    /// The conversation it's of (a Claude subagent's file is of its parent's).
    pub session: String,
    pub model: String,
    /// When it starts, in ms.
    pub start: i64,
    pub spent: Spent,
}

impl Report {
    /// What was spent from `from` to `to` (ms), in the `hours` that overlap
    /// it, each agent's in `stretches` equal stretches of it for a chart.
    pub fn of(hours: &[Hour], from: i64, to: i64, stretches: usize) -> Report {
        let mut agents: HashMap<&str, (HashSet<&str>, Spent, Vec<u64>)> = HashMap::new();
        let mut models: HashMap<(&str, &str), Spent> = HashMap::new();
        let mut total = Spent::default();
        let length = (to - from).max(1);
        for hour in hours {
            if hour.start + HOUR <= from || hour.start > to {
                continue;
            }
            let (sessions, spent, chart) = agents
                .entry(&hour.agent)
                .or_insert_with(|| (HashSet::new(), Spent::default(), vec![0; stretches]));
            sessions.insert(&hour.session);
            spent.merge(&hour.spent);
            if stretches > 0 {
                let into = (hour.start.max(from) - from) as i128;
                let i = (into * stretches as i128 / length as i128) as usize;
                chart[i.min(stretches - 1)] += hour.spent.tokens.total();
            }
            models
                .entry((&hour.agent, &hour.model))
                .or_default()
                .merge(&hour.spent);
            total.merge(&hour.spent);
        }
        let mut agents: Vec<AgentSpent> = agents
            .into_iter()
            .map(|(agent, (sessions, spent, chart))| AgentSpent {
                agent: agent.to_string(),
                sessions: sessions.len(),
                spent,
                chart,
            })
            .collect();
        agents.sort_by_key(|a| (std::cmp::Reverse(a.spent.tokens.total()), a.agent.clone()));
        let mut models: Vec<ModelSpent> = models
            .into_iter()
            .map(|((agent, model), spent)| ModelSpent {
                agent: agent.to_string(),
                model: model.to_string(),
                spent,
            })
            .collect();
        models.sort_by_key(|m| (std::cmp::Reverse(m.spent.tokens.total()), m.model.clone()));
        Report {
            from,
            to,
            sessions: agents.iter().map(|a| a.sessions).sum(),
            total,
            agents,
            models,
        }
    }
}

/// How long since a file last changed it's still read as it grows, rather
/// than all again when it next changes.
const KEEP_READING: Duration = Duration::from_secs(3600);

/// The agents' files that changed lately, each read as far as it has grown.
#[derive(Default)]
pub struct Scan {
    files: HashMap<PathBuf, Box<dyn Spending>>,
}

/// One of an agent's files, as found.
struct Found {
    agent: &'static str,
    session: String,
    path: PathBuf,
    key: String,
    modified: SystemTime,
    size: u64,
}

impl Scan {
    /// Reads the built-in agents' files that changed since what `db` has of
    /// them was saved (all of them, saved with another `VERSION`), and saves
    /// each one's calls in place of what was. What was saved of a file gone
    /// stays, unless it was moved (codex archiving a rollout): then it's the
    /// file where it is now.
    pub fn update(&mut self, db: &Db) -> Result<()> {
        const KEY: &str = "spent:version";
        let current = db.meta(KEY)?.as_deref() == Some(VERSION);
        let mut saved = if current {
            db.spent_files()?
        } else {
            HashMap::new()
        };
        let mut found = Vec::new();
        for &agent in crate::agent::BUILT_IN {
            for (path, session) in crate::agent::adapter(agent).token_files() {
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                found.push(Found {
                    agent,
                    session,
                    key: path.to_string_lossy().into_owned(),
                    path,
                    modified: meta.modified().unwrap_or(UNIX_EPOCH),
                    size: meta.len(),
                });
            }
        }
        // A file gone from where it was saved, found by its name where an
        // agent keeps its files, was moved: what was saved of it goes, and
        // it's saved where it is.
        let here: HashMap<(&str, &std::ffi::OsStr), &str> = found
            .iter()
            .filter_map(|f| Some(((f.agent, f.path.file_name()?), f.key.as_str())))
            .collect();
        let listed: HashSet<&str> = found.iter().map(|f| f.key.as_str()).collect();
        let mut moved = Vec::new();
        for (key, file) in &saved {
            let name = std::path::Path::new(key).file_name();
            if !listed.contains(key.as_str())
                && let Some(to) = name.and_then(|n| here.get(&(file.agent.as_str(), n)))
            {
                moved.push((key.clone(), to.to_string()));
            }
        }
        for (from, to) in moved {
            db.forget_spent(&from)?;
            saved.remove(&from);
            saved.remove(&to);
        }
        let now = SystemTime::now();
        let mut recent = HashSet::new();
        for f in found {
            if now
                .duration_since(f.modified)
                .is_ok_and(|d| d < KEEP_READING)
            {
                recent.insert(f.path.clone());
            }
            let at = ms(f.modified);
            if saved
                .get(&f.key)
                .is_some_and(|s| (s.modified, s.size) == (at, f.size))
            {
                continue;
            }
            let reader = match self.files.entry(f.path) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => match crate::agent::adapter(f.agent).tokens(e.key()) {
                    Some(reader) => e.insert(reader),
                    None => continue,
                },
            };
            // One that can't be read now (gone since) is read next time.
            if reader.update().is_err() {
                continue;
            }
            db.save_spent(&f.key, f.agent, &f.session, at, f.size, reader.calls())?;
        }
        if !current {
            db.set_meta(KEY, VERSION)?;
        }
        self.files.retain(|path, _| recent.contains(path));
        Ok(())
    }
}

fn ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Tokens to three figures: `950`, `9.47M`, `37.5M`, `2.15B`.
pub fn amount(n: u64) -> String {
    const UNITS: [(f64, &str); 3] = [(1e9, "B"), (1e6, "M"), (1e3, "k")];
    // What would round up to the next unit is in it: 1.00M, not 1000k.
    let Some((size, unit)) = UNITS
        .into_iter()
        .find(|(size, _)| n as f64 >= size * 0.9995)
    else {
        return n.to_string();
    };
    let x = n as f64 / size;
    let digits = match x {
        x if x >= 99.95 => 0,
        x if x >= 9.995 => 1,
        _ => 2,
    };
    format!("{x:.digits$}{unit}")
}

/// Dollars to the cent, thousands apart: `$4,355.62`.
pub fn dollars(x: f64) -> String {
    let cents = (x * 100.0).round() as u64;
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("${grouped}.{:02}", cents % 100)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn call(at: i64, model: &str, input: u64, cache_read: u64, output: u64) -> Call {
        Call {
            id: format!("test:{model}@{at}:{input}"),
            at,
            model: model.into(),
            counts: Counts {
                input,
                cache_read,
                output,
                ..Counts::default()
            },
            cost: None,
        }
    }

    #[test]
    fn a_call_costs_what_its_models_prices_say() {
        // A million of each, on Opus 5.5: $4 in, $0.20 from the cache, $20 out;
        // the cache saved $3.80.
        let c = call(0, "claude-opus-5-5", 1_000_000, 1_000_000, 1_000_000);
        let (cost, saved) = c.cost().unwrap();
        assert!((cost - 24.2).abs() < 1e-9, "{cost}");
        assert!((saved - 3.8).abs() < 1e-9, "{saved}");
        // Written to the cache: for five minutes at its price, for an hour at
        // twice input's.
        let mut c = call(0, "claude-opus-5-5", 0, 0, 0);
        c.counts.cache_write = 3_000_000;
        c.counts.cache_write_hour = 1_000_000;
        assert!((c.cost().unwrap().0 - (2.0 * 5.0 + 8.0)).abs() < 1e-9);
        // A long call is priced at the long rates, all of it.
        let c = call(0, "gpt-6.1-sol", 300_000, 0, 0);
        assert!((c.cost().unwrap().0 - 1.2).abs() < 1e-9);
        let c = call(0, "gpt-6.1-sol", 200_000, 0, 0);
        assert!((c.cost().unwrap().0 - 0.4).abs() < 1e-9);
        // Named with a date or a provider, it's the same model.
        assert!(
            call(0, "claude-haiku-4-5-20251001", 1, 0, 0)
                .cost()
                .is_some()
        );
        assert!(
            call(0, "anthropic/claude-opus-5-5", 1, 0, 0)
                .cost()
                .is_some()
        );
        // Unknown, it has no price, unless the agent said one (pi's 0 isn't).
        let mut c = call(0, "vllm/qwen3", 1_000, 0, 0);
        assert_eq!(c.cost(), None);
        c.cost = Some(0.0);
        assert_eq!(c.cost(), None);
        c.cost = Some(0.5);
        assert_eq!(c.cost(), Some((0.5, 0.0)));
    }

    #[test]
    fn amounts_and_dollars_read_briefly() {
        let read: Vec<String> = [950, 1_234, 37_500_000, 9_470_000, 999_999, 2_150_000_000]
            .into_iter()
            .map(amount)
            .collect();
        assert_eq!(read, ["950", "1.23k", "37.5M", "9.47M", "1.00M", "2.15B"]);
        assert_eq!(dollars(4355.624), "$4,355.62");
        assert_eq!(dollars(0.0), "$0.00");
        assert_eq!(dollars(1_234_567.891), "$1,234,567.89");
        assert_eq!(dollars(100.0), "$100.00");
    }

    const H: i64 = HOUR;

    /// What files' calls add up to by the hour, as saved: an agent, a
    /// conversation, calls.
    pub(crate) fn hours_of(files: Vec<(&str, &str, Vec<Call>)>) -> Vec<Hour> {
        let db = Db::open_in_memory().unwrap();
        for (i, (agent, session, calls)) in files.iter().enumerate() {
            db.save_spent(&format!("/f{i}"), agent, session, 0, 0, calls)
                .unwrap();
        }
        db.spent_hours(i64::MIN / 2, i64::MAX / 2).unwrap()
    }

    #[test]
    fn calls_are_summed_by_the_hour_of_their_model() {
        let calls = vec![
            call(10, "claude-opus-5-5", 100, 1_000, 10),
            call(H - 1, "claude-opus-5-5", 100, 1_000, 10),
            call(H, "claude-opus-5-5", 1, 0, 0),
            call(20, "claude-haiku-5-5", 5, 0, 0),
            // Spent nothing (failed): in none.
            call(30, "claude-sonnet-5-5", 0, 0, 0),
        ];
        let mut hours = hours_of(vec![("claude", "a", calls)]);
        hours.sort_by_key(|h| (h.start, h.model.clone()));
        let kept: Vec<(i64, &str, u64)> = hours
            .iter()
            .map(|h| (h.start, h.model.as_str(), h.spent.tokens.total()))
            .collect();
        assert_eq!(
            kept,
            [
                (0, "claude-haiku-5-5", 5),
                (0, "claude-opus-5-5", 2_220),
                (H, "claude-opus-5-5", 1)
            ]
        );
        assert!(hours[1].spent.cost > 0.0 && hours[1].spent.unpriced == 0);
    }

    #[test]
    fn a_report_sums_by_agent_and_model_in_its_period() {
        let hours = hours_of(vec![
            (
                "claude",
                "a",
                vec![
                    call(H, "claude-opus-5-5", 100, 1_000, 10),
                    // Before the period: left out.
                    call(-H, "claude-opus-5-5", 1, 1, 1),
                ],
            ),
            // A subagent's file, of the same conversation.
            (
                "claude",
                "a",
                vec![call(7 * H, "claude-haiku-5-5", 50, 0, 5)],
            ),
            ("pi", "b", vec![call(4 * H, "vllm/qwen3", 2_000, 0, 20)]),
        ]);
        let report = Report::of(&hours, 0, 8 * H, 4);
        assert_eq!(report.total.tokens.total(), 1_110 + 55 + 2_020);
        assert_eq!(report.sessions, 2);
        assert_eq!(report.agents[0].agent, "pi");
        assert_eq!(report.agents[0].chart, [0, 0, 2_020, 0]);
        assert!(report.agents[0].spent.unpriced());
        let claude = &report.agents[1];
        assert_eq!(claude.sessions, 1);
        assert_eq!(claude.chart, [1_110, 0, 0, 55]);
        assert!(!claude.spent.unpriced() && claude.spent.cost > 0.0);
        let models: Vec<&str> = report.models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(
            models,
            ["vllm/qwen3", "claude-opus-5-5", "claude-haiku-5-5"]
        );
        // What has no price isn't in the cost, and says so.
        assert_eq!(report.total.unpriced, 2_020);
        assert!((report.total.cost - claude.spent.cost).abs() < 1e-12);
        // An hour the period starts in counts, whole.
        let report = Report::of(&hours, H + 1, 8 * H, 4);
        assert_eq!(report.total.tokens.total(), 1_110 + 55 + 2_020);
    }
}
