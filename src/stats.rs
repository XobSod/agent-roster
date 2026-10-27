//! What agents can tell about a session beyond its state: prompts, context use,
//! tokens, cost, changed lines. Every field is optional; each agent fills in what
//! it can, from its transcript (see `Transcript`).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// How many recent prompts a transcript keeps, for generating a title.
const RECENT_PROMPTS: usize = 5;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentStats {
    pub prompts: Option<u64>,
    pub context_used: Option<u64>,
    pub context_limit: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    /// Cost and lines are totals up to an earlier point; the agent has done more since.
    pub partial: bool,
}

/// An agent's record of a conversation, read as it grows: where roster gets
/// statistics and, for a generated title, the latest prompts.
pub trait Transcript: Send {
    fn path(&self) -> &Path;
    /// Reads what was added since the last call (all of it the first time).
    fn update(&mut self) -> std::io::Result<()>;
    /// The totals so far; `model`, as the agent reports it, may tell the
    /// context window.
    fn stats(&self, model: Option<&str>) -> AgentStats;
    /// The last few prompts the user typed, oldest first.
    fn recent_prompts(&self) -> Vec<String>;
}

/// What a JSONL file adds up to, line by line: what its reader knows of its
/// format (`Jsonl` does the reading).
pub trait Lines: Default + Send {
    /// One complete line of the file.
    fn line(&mut self, line: &[u8]);
}

/// What an agent's JSONL transcript adds up to: its statistics and prompts.
pub trait Tally: Lines {
    /// The totals so far; see `Transcript::stats`.
    fn stats(&self, model: Option<&str>) -> AgentStats;
    fn prompts(&self) -> &Prompts;
}

/// Reads a JSONL file as it grows (a transcript, say): each read takes only
/// what was added since, a line still being written waits for the next, and
/// a file that got shorter (rewritten) or another one at its path (replaced,
/// renamed over it) is read again from the start.
#[derive(Debug, Default)]
pub struct Jsonl<T> {
    path: PathBuf,
    /// The file read so far, as its device and inode.
    file: Option<(u64, u64)>,
    offset: u64,
    /// An incomplete last line, finished by the next read.
    rest: Vec<u8>,
    tally: T,
}

impl<T: Lines> Jsonl<T> {
    pub fn new(path: &Path) -> Jsonl<T> {
        Jsonl {
            path: path.to_path_buf(),
            file: None,
            offset: 0,
            rest: Vec::new(),
            tally: T::default(),
        }
    }

    /// What its lines add up to so far.
    pub fn tally(&self) -> &T {
        &self.tally
    }

    /// Reads what was added since the last read (all of it the first time).
    pub fn read(&mut self) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let (len, identity) = (meta.len(), Some((meta.dev(), meta.ino())));
        if len < self.offset || (self.file.is_some() && self.file != identity) {
            *self = Jsonl::new(&self.path.clone());
        }
        self.file = identity;
        if len == self.offset {
            return Ok(());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        // In pieces: a file read whole the first time (100 MB of a long
        // transcript) takes a piece's room at once, not its own.
        const PIECE: u64 = 4 << 20;
        let mut text = std::mem::take(&mut self.rest);
        loop {
            // What's before has no line's end.
            let from = text.len();
            let read = (&mut file).take(PIECE).read_to_end(&mut text)?;
            if read == 0 {
                break;
            }
            self.offset += read as u64;
            let mut start = 0;
            for end in memchr::memchr_iter(b'\n', &text[from..]) {
                self.tally.line(&text[start..from + end]);
                start = from + end + 1;
            }
            text.drain(..start);
        }
        // What's kept is a line not yet ended, in room of its own.
        text.shrink_to_fit();
        self.rest = text;
        Ok(())
    }
}

impl<T: Tally> Transcript for Jsonl<T> {
    fn path(&self) -> &Path {
        &self.path
    }

    fn update(&mut self) -> std::io::Result<()> {
        self.read()
    }

    fn stats(&self, model: Option<&str>) -> AgentStats {
        self.tally.stats(model)
    }

    fn recent_prompts(&self) -> Vec<String> {
        self.tally.prompts().recent.iter().cloned().collect()
    }
}

/// The prompts the user typed: how many, and the last few.
#[derive(Debug, Default)]
pub struct Prompts {
    pub count: u64,
    recent: VecDeque<String>,
}

impl Prompts {
    pub fn push(&mut self, text: String) {
        self.count += 1;
        if self.recent.len() == RECENT_PROMPTS {
            self.recent.pop_front();
        }
        self.recent.push_back(text);
    }
}

/// A message's text, read from a transcript: a string, or the text blocks of
/// a list joined (images, tool calls and the like skipped, unread), as pi and
/// codex write them (`agent::message_text` for one already parsed).
pub struct Text(pub Option<String>);

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Text, D::Error> {
        struct Content;
        impl<'de> Visitor<'de> for Content {
            type Value = Text;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a message's content")
            }
            fn visit_str<E>(self, text: &str) -> Result<Text, E> {
                Ok(Text(Some(text.to_string())))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Text, A::Error> {
                let mut text = Vec::new();
                while let Some(block) = seq.next_element::<Block>()? {
                    text.extend(block.into_text());
                }
                Ok(Text((!text.is_empty()).then(|| text.join("\n"))))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Text, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Text(None))
            }
            fn visit_unit<E>(self) -> Result<Text, E> {
                Ok(Text(None))
            }
        }
        d.deserialize_any(Content)
    }
}

#[derive(serde::Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

impl Block {
    /// Its text, if it's a text block (`text`, codex's `input_text`, or one
    /// that says no type).
    fn into_text(self) -> Option<String> {
        self.kind
            .as_deref()
            .is_none_or(|k| matches!(k, "text" | "input_text"))
            .then_some(self.text)
            .flatten()
    }
}

/// The type of a JSONL entry that names it first (`{"type":"…",`), which
/// tells most entries apart without parsing them; None if it isn't first.
pub fn leading_type(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(br#"{"type":""#)?;
    Some(&rest[..memchr::memchr(b'"', rest)?])
}

/// A session's statistics as short parts, for the header and `roster info`:
/// created 2d · 5 prompts · worked 34m · context 117k/1M (12%) · 92k out ·
/// $3.20 · +420 −35. Parts the agent can't tell are left out; "≥" marks totals
/// that are behind what the agent has done since.
pub fn summary(s: &crate::model::Session, stats: Option<&AgentStats>, now: i64) -> Vec<String> {
    let mut parts = vec![format!("created {}", crate::util::ago(now - s.created_at))];
    match stats.and_then(|a| a.prompts) {
        Some(n) => parts.push(format!("{n} prompt{}", if n == 1 { "" } else { "s" })),
        None if s.turns > 0 => parts.push(format!(
            "{} turn{}",
            s.turns,
            if s.turns == 1 { "" } else { "s" }
        )),
        None => {}
    }
    let worked = s.worked(now);
    if worked > 0 {
        parts.push(format!("worked {}", crate::util::clock(worked)));
    }
    let Some(a) = stats else { return parts };
    match (a.context_used, a.context_limit) {
        (Some(used), Some(limit)) => parts.push(format!(
            "context {}/{} ({}%)",
            tokens(used),
            tokens(limit),
            (used * 100 + limit / 2) / limit.max(1)
        )),
        (Some(used), None) => parts.push(format!("context {}", tokens(used))),
        _ => {}
    }
    if let Some(out) = a.output_tokens {
        parts.push(format!("{} out", tokens(out)));
    }
    let at_least = if a.partial { "≥" } else { "" };
    if let Some(cost) = a.cost_usd {
        parts.push(format!("{at_least}${cost:.2}"));
    }
    if let (Some(added), Some(removed)) = (a.lines_added, a.lines_removed)
        && added + removed > 0
    {
        parts.push(format!("{at_least}+{added} −{removed}"));
    }
    parts
}

/// `1234` → "1.2k", `117000` → "117k", `1000000` → "1M".
pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line read, for what `Jsonl` hands on.
    #[derive(Default)]
    struct Seen(Vec<String>);

    impl Lines for Seen {
        fn line(&mut self, line: &[u8]) {
            self.0.push(String::from_utf8_lossy(line).into_owned());
        }
    }

    #[test]
    fn a_file_longer_than_a_piece_is_read_line_by_line_across_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        // Lines of every length up to a few thousand, some longer than a
        // piece, past two pieces in all, the last not yet ended.
        let mut text = String::new();
        let mut lines = Vec::new();
        for i in 0..4000 {
            let line = format!(
                "{i}:{}",
                "x".repeat(i % 3001 + if i == 7 { 5 << 20 } else { 0 })
            );
            text.push_str(&line);
            text.push('\n');
            lines.push(line);
        }
        text.push_str("half");
        std::fs::write(&path, &text).unwrap();
        let mut read = Jsonl::<Seen>::new(&path);
        read.read().unwrap();
        assert_eq!(read.tally().0.len(), lines.len());
        assert!(read.tally().0 == lines);
        // The line's end comes later.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b" done\n"))
            .unwrap();
        read.read().unwrap();
        assert_eq!(read.tally().0.last().map(String::as_str), Some("half done"));
    }

    #[test]
    fn a_file_replaced_is_read_again_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let replace = |text: &str| {
            let new = dir.path().join("new.jsonl");
            std::fs::write(&new, text).unwrap();
            std::fs::rename(&new, &path).unwrap();
        };
        let mut file = Jsonl::<Seen>::new(&path);
        std::fs::write(&path, "old\n").unwrap();
        file.read().unwrap();
        assert_eq!(file.tally().0, ["old"]);
        // Replaced by one as long, then by a longer one: each read whole.
        replace("new\n");
        file.read().unwrap();
        assert_eq!(file.tally().0, ["new"]);
        replace("one\ntwo\n");
        file.read().unwrap();
        assert_eq!(file.tally().0, ["one", "two"]);
        // Appended to, and a line still being written: read on as before.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(f, "three\nfo").unwrap();
        file.read().unwrap();
        assert_eq!(file.tally().0, ["one", "two", "three"]);
        writeln!(f, "ur").unwrap();
        file.read().unwrap();
        assert_eq!(file.tally().0, ["one", "two", "three", "four"]);
        // Cut shorter in place: read again from its start.
        std::fs::write(&path, "x\n").unwrap();
        file.read().unwrap();
        assert_eq!(file.tally().0, ["x"]);
    }

    #[test]
    fn a_messages_text_is_its_string_or_its_text_blocks() {
        let text = |json: &str| serde_json::from_str::<Text>(json).unwrap().0;
        assert_eq!(text(r#""fix it""#).as_deref(), Some("fix it"));
        assert_eq!(
            text(r#"[{"type":"text","text":"one"},{"type":"image","data":"…"},{"type":"input_text","text":"two"},{"text":"three"},{"type":"output_text","text":"no"}]"#).as_deref(),
            Some("one\ntwo\nthree")
        );
        assert_eq!(text(r#"[{"type":"tool_result","content":"x"}]"#), None);
        assert_eq!(text(r#"{"type":"text"}"#), None);
        assert_eq!(text("null"), None);
    }

    #[test]
    fn summary_has_what_is_known() {
        let mut s = crate::model::tests::session("a");
        s.created_at = 0;
        s.state = crate::model::State::Idle;
        s.turns = 3;
        s.worked_ms = 34 * 60_000;
        let day = 2 * 86_400_000;
        assert_eq!(
            summary(&s, None, day),
            ["created 2d", "3 turns", "worked 34m"]
        );
        let stats = AgentStats {
            prompts: Some(5),
            context_used: Some(117_000),
            context_limit: Some(1_000_000),
            output_tokens: Some(92_000),
            cost_usd: Some(3.2),
            lines_added: Some(420),
            lines_removed: Some(35),
            partial: true,
        };
        assert_eq!(
            summary(&s, Some(&stats), day).join(" · "),
            "created 2d · 5 prompts · worked 34m · context 117k/1M (12%) · 92k out · ≥$3.20 · ≥+420 −35"
        );
    }

    #[test]
    fn token_counts_are_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1_234), "1.2k");
        assert_eq!(tokens(2_000), "2k");
        assert_eq!(tokens(117_400), "117k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(4_660_631), "4.7M");
    }
}
