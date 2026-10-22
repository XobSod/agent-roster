//! Claude's conversations, those started outside roster that it can take over:
//! Claude keeps each one as `<claude dir>/projects/<dir>/<id>.jsonl`, and each
//! running Claude registers itself in `<claude dir>/sessions/<pid>.json`. Both are
//! Claude's internal files, so anything unreadable is skipped.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent::Conversation;

/// Claude's own directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => dirs::home_dir().map(|h| h.join(".claude")),
    }
}

/// The conversations Claude has kept whose id `wanted` says yes to (only
/// their files are read), newest first.
pub fn conversations(claude_dir: &Path, wanted: impl Fn(&str) -> bool) -> Vec<Conversation> {
    let open = open_conversations(claude_dir);
    let mut found: Vec<Conversation> = fs::read_dir(claude_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|dir| fs::read_dir(dir.path()).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(&wanted))
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
    let (entries, modified) = crate::agent::file_ends(path)?;
    let mut c = Conversation {
        agent: super::CLAUDE.to_string(),
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
    let mut cwd = None;
    for e in &entries {
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

    /// Until the child has exec'd, its command line is still the test's.
    pub fn wait_until_claude(pid: u32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !is_claude(pid as i32) {
            assert!(
                std::time::Instant::now() < deadline,
                "{pid} never became claude"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
        let found = conversations(dir.path(), |_| true);
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
    fn reads_all_of_a_file_up_to_the_size_of_its_start_and_end() {
        // A title near the end of a 200 KB file (found in real use: files
        // between 64 and 512 KB were read only for their first 64 KB).
        let dir = tempfile::tempdir().unwrap();
        let filler = format!(r#"{{"type":"progress","data":"{}"}}"#, "x".repeat(1000));
        let mut lines =
            vec![r#"{"type":"user","cwd":"/w","message":{"content":"hi"}}"#.to_string()];
        lines.extend(std::iter::repeat_n(filler, 200));
        lines.push(r#"{"type":"ai-title","aiTitle":"Late title"}"#.into());
        lines.push(r#"{"type":"custom-title","customTitle":"Renamed"}"#.into());
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        conversation_file(dir.path(), "c-1", &lines);
        conversation_file(dir.path(), "c-2", &lines);
        let found = conversations(dir.path(), |id| id == "c-1");
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].title.as_deref(), found[0].custom_title.as_deref()),
            (Some("Late title"), Some("Renamed"))
        );
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
        wait_until_claude(pid);
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
        assert_eq!(
            conversations(dir.path(), |_| true)[0].open_in,
            Some(pid as i32)
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(conversations(dir.path(), |_| true)[0].open_in, None);
    }
}
