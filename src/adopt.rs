//! Conversations started outside roster that it can take over:
//! Claude keeps each one as `<claude dir>/projects/<dir>/<id>.jsonl`, and each
//! running Claude registers itself in `<claude dir>/sessions/<pid>.json`. Both are
//! Claude's internal files, so anything unreadable is skipped.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How much of the start and end of a conversation file is read.
const HEAD: u64 = 64 * 1024;
const TAIL: u64 = 512 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub id: String,
    pub path: PathBuf,
    /// Where it ran; it's resumed there.
    pub cwd: PathBuf,
    pub branch: Option<String>,
    /// The name it was given (`claude --name`, `/rename`, or a session manager).
    pub custom_title: Option<String>,
    /// Claude's title for it.
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    /// The model it last used.
    pub model: Option<String>,
    /// When the file last changed, in ms.
    pub modified: i64,
    /// A Claude process that has it open right now.
    pub open_in: Option<i32>,
}

impl Conversation {
    /// What to call it, on one line: its name, Claude's title, else a prompt.
    pub fn label(&self) -> String {
        [
            &self.custom_title,
            &self.title,
            &self.first_prompt,
            &self.last_prompt,
        ]
        .into_iter()
        .flatten()
        .next()
        .map_or(String::new(), |s| one_line(s))
    }
}

pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Claude's own directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => dirs::home_dir().map(|h| h.join(".claude")),
    }
}

/// Every conversation Claude has kept, newest first.
pub fn conversations(claude_dir: &Path) -> Vec<Conversation> {
    let open = open_conversations(claude_dir);
    let mut found: Vec<Conversation> = fs::read_dir(claude_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|dir| fs::read_dir(dir.path()).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| read(&p))
        .map(|mut c| {
            c.open_in = open.get(&c.id).copied();
            c
        })
        .collect();
    found.sort_by_key(|c| std::cmp::Reverse(c.modified));
    found
}

/// What one conversation file says about itself, from its start and end.
fn read(path: &Path) -> Option<Conversation> {
    let id = path.file_stem()?.to_str()?.to_string();
    let mut file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    let mut head = Vec::new();
    (&mut file).take(HEAD).read_to_end(&mut head).ok()?;
    let mut tail = Vec::new();
    let tail_start = meta.len().saturating_sub(TAIL);
    if tail_start > 0 {
        file.seek(SeekFrom::Start(tail_start)).ok()?;
        file.read_to_end(&mut tail).ok()?;
        // The first line is likely cut.
        let cut = tail
            .iter()
            .position(|b| *b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        tail.drain(..cut);
    } else {
        tail = head.clone();
    }
    let mut c = Conversation {
        id,
        path: path.to_path_buf(),
        cwd: PathBuf::new(),
        branch: None,
        custom_title: None,
        title: None,
        first_prompt: None,
        last_prompt: None,
        model: None,
        modified,
        open_in: None,
    };
    let entries = |bytes: &[u8]| -> Vec<Value> {
        bytes
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect()
    };
    let mut cwd = None;
    for e in entries(&head).iter().chain(entries(&tail).iter()) {
        if let Some(dir) = e["cwd"].as_str() {
            cwd = Some(PathBuf::from(dir));
        }
        // Outside a repository (or detached) Claude says "HEAD".
        if let Some(branch) = e["gitBranch"]
            .as_str()
            .filter(|b| !b.is_empty() && *b != "HEAD")
        {
            c.branch = Some(branch.to_string());
        }
        match e["type"].as_str() {
            Some("custom-title") => {
                c.custom_title = e["customTitle"]
                    .as_str()
                    .map(String::from)
                    .or(c.custom_title.take())
            }
            Some("user") if c.first_prompt.is_none() => c.first_prompt = typed_prompt(e),
            Some("ai-title") => {
                c.title = e["aiTitle"].as_str().map(String::from).or(c.title.take())
            }
            Some("last-prompt") => {
                c.last_prompt = e["lastPrompt"]
                    .as_str()
                    .map(String::from)
                    .or(c.last_prompt.take())
            }
            Some("assistant") => {
                if let Some(model) = e["message"]["model"]
                    .as_str()
                    .filter(|m| m.starts_with("claude-"))
                {
                    c.model = Some(model.to_string());
                }
            }
            _ => {}
        }
    }
    // Without a directory there's nowhere to resume it; without a prompt it
    // has nothing in it.
    c.cwd = cwd?;
    if c.first_prompt.is_none() && c.last_prompt.is_none() {
        return None;
    }
    Some(c)
}

/// A prompt the user typed: text, not a tool result, a meta message, or a
/// command wrapped in tags.
fn typed_prompt(e: &Value) -> Option<String> {
    if e["isMeta"].as_bool() == Some(true) {
        return None;
    }
    let content = &e["message"]["content"];
    let text = match content.as_str() {
        Some(text) => text,
        None => content.as_array()?.iter().find(|b| b["type"] == "text")?["text"].as_str()?,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

/// Conversations a running Claude has open, by conversation id, with its pid.
/// Claude removes its file when it exits; files of processes that died are
/// ignored.
fn open_conversations(claude_dir: &Path) -> HashMap<String, i32> {
    let mut open = HashMap::new();
    for entry in fs::read_dir(claude_dir.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let (Some(pid), Some(id)) = (v["pid"].as_i64(), v["sessionId"].as_str()) else {
            continue;
        };
        if is_claude(pid as i32) {
            open.insert(id.to_string(), pid as i32);
        }
    }
    open
}

/// Whether `pid` is a running Claude (not a reused pid of something else).
fn is_claude(pid: i32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline"))
        .is_ok_and(|cmd| String::from_utf8_lossy(&cmd).contains("claude"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation_file(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        let folder = dir.join("projects").join("-work-app");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{id}.jsonl"));
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    #[test]
    fn reads_what_a_conversation_says_about_itself() {
        let dir = tempfile::tempdir().unwrap();
        conversation_file(
            dir.path(),
            "c-1",
            &[
                r#"{"type":"user","cwd":"/work/app","gitBranch":"main","message":{"content":"fix it"}}"#,
                r#"{"type":"assistant","message":{"model":"claude-opus-5-5"}}"#,
                r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
                r#"{"type":"ai-title","aiTitle":"Fix the parser"}"#,
                r#"{"type":"last-prompt","lastPrompt":"and the tests"}"#,
            ],
        );
        // Named, with prompts in blocks and commands in tags.
        conversation_file(
            dir.path(),
            "c-2",
            &[
                r#"{"type":"user","cwd":"/work/app","message":{"content":"<command-name>/model</command-name>"}}"#,
                r#"{"type":"user","message":{"content":[{"type":"text","text":"  review\n the PR "}]}}"#,
                r#"{"type":"custom-title","customTitle":"Reviews"}"#,
            ],
        );
        // Nothing in it yet: not offered.
        conversation_file(
            dir.path(),
            "empty",
            &[r#"{"type":"mode","cwd":"/work/app"}"#],
        );
        let found = conversations(dir.path());
        assert_eq!(found.len(), 2);
        let c = found.iter().find(|c| c.id == "c-2").unwrap();
        assert_eq!(c.first_prompt.as_deref(), Some("review\n the PR"));
        assert_eq!(c.label(), "Reviews");
        let mut unnamed = c.clone();
        unnamed.custom_title = None;
        assert_eq!(unnamed.label(), "review the PR");
        let c = found.iter().find(|c| c.id == "c-1").unwrap();
        assert_eq!(c.label(), "Fix the parser");
        assert_eq!(
            (c.id.as_str(), c.cwd.as_path()),
            ("c-1", Path::new("/work/app"))
        );
        assert_eq!(
            (c.branch.as_deref(), c.title.as_deref()),
            (Some("main"), Some("Fix the parser"))
        );
        assert_eq!(
            (c.last_prompt.as_deref(), c.model.as_deref()),
            (Some("and the tests"), Some("claude-opus-5-5"))
        );
        assert_eq!(c.open_in, None);
    }

    #[test]
    fn knows_which_conversations_a_running_claude_has_open() {
        let dir = tempfile::tempdir().unwrap();
        conversation_file(
            dir.path(),
            "c-1",
            &[r#"{"type":"last-prompt","cwd":"/w","lastPrompt":"hi"}"#],
        );
        // A process whose command line says claude, standing in for Claude (no
        // exec: that would change its command line).
        let bin = dir.path().join("claude");
        fs::write(&bin, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
        fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut child = std::process::Command::new("sh").arg(&bin).spawn().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let pid = child.id();
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"c-1"}}"#),
        )
        .unwrap();
        // A file left by a process that's gone.
        fs::write(
            sessions.join("999999.json"),
            r#"{"pid":999999,"sessionId":"c-2"}"#,
        )
        .unwrap();
        assert_eq!(conversations(dir.path())[0].open_in, Some(pid as i32));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(conversations(dir.path())[0].open_in, None);
    }
}
