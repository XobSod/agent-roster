//! pi's conversations, those started outside roster that it can take over:
//! pi keeps each as `<sessions>/--<cwd>--/<timestamp>_<id>.jsonl`, where
//! `<sessions>` is `sessions` in pi's directory (`$PI_CODING_AGENT_DIR`, else
//! `~/.pi/agent`); or as `<dir>/<timestamp>_<id>.jsonl` for a directory of
//! its own, `$PI_CODING_AGENT_SESSION_DIR`. pi's internal files, so anything
//! unreadable is skipped. (A `sessionDir` in pi's settings isn't followed;
//! the variables are.) pi doesn't say which of them a running pi has open.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::PI;
use crate::agent::Conversation;

/// Where pi keeps its sessions.
pub fn sessions_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        return Some(home(PathBuf::from(dir)));
    }
    let agent = match std::env::var_os("PI_CODING_AGENT_DIR") {
        Some(dir) => home(PathBuf::from(dir)),
        None => dirs::home_dir()?.join(".pi").join("agent"),
    };
    Some(agent.join("sessions"))
}

/// `path` with a leading `~` as the home directory, as pi reads it.
fn home(path: PathBuf) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

/// The session files in `sessions`: in its directories, or in it.
fn session_files(sessions: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(sessions)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            let inside: Vec<PathBuf> = if path.is_dir() {
                fs::read_dir(&path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .collect()
            } else {
                vec![path]
            };
            inside
        })
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
}

/// The conversations pi has kept whose id `wanted` says yes to (only their
/// files are read).
pub fn conversations(sessions: &Path, wanted: impl Fn(&str) -> bool) -> Vec<Conversation> {
    session_files(sessions)
        .filter(|p| id_of(p).is_some_and(&wanted))
        .filter_map(|p| read(&p))
        .collect()
}

/// The file pi keeps conversation `id` in, if it has made it.
pub fn session_file(sessions: &Path, id: &str) -> Option<PathBuf> {
    session_files(sessions).find(|p| id_of(p) == Some(id))
}

/// The id in a session file's name, after its timestamp.
fn id_of(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.split_once('_').map_or(stem, |(_, id)| id))
}

/// What one session file says about itself, from its start and end.
fn read(path: &Path) -> Option<Conversation> {
    let (entries, modified) = crate::agent::file_ends(path)?;
    let mut c = Conversation {
        agent: PI.to_string(),
        id: id_of(path)?.to_string(),
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
        match e["type"].as_str() {
            // The header: its id and where it ran.
            Some("session") => {
                if let Some(id) = e["id"].as_str() {
                    c.id = id.to_string();
                }
                cwd = e["cwd"].as_str().map(PathBuf::from);
            }
            // Its name, the last one given: pi's title for it, as pi
            // reports it while it runs (`/name` changes it).
            Some("session_info") => c.title = e["name"].as_str().map(String::from),
            Some("message") => {
                let m = &e["message"];
                match m["role"].as_str() {
                    Some("user") => {
                        if let Some(text) = text_of(&m["content"]) {
                            c.last_prompt = Some(text);
                        }
                    }
                    Some("assistant") => {
                        if let Some(model) = m["model"].as_str() {
                            c.model = Some(model.to_string());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    // Without a directory there's nowhere to resume it; without a prompt it
    // has nothing in it.
    c.cwd = cwd?;
    c.first_prompt = Some(first_prompt(path)?);
    c.last_prompt = c.last_prompt.or_else(|| c.first_prompt.clone());
    Some(c)
}

/// The first thing said in a session file: read up to it, which can be
/// further than the start `file_ends` reads (pi can begin with a system
/// message holding every tool's description).
fn first_prompt(path: &Path) -> Option<String> {
    const LIMIT: usize = 8 * 1024 * 1024;
    let mut read = 0;
    for line in BufReader::new(fs::File::open(path).ok()?).split(b'\n') {
        let line = line.ok()?;
        read += line.len() + 1;
        if read > LIMIT {
            return None;
        }
        if memchr::memmem::find(&line, b"\"user\"").is_none() {
            continue;
        }
        let Ok(e) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if e["type"] == "message"
            && e["message"]["role"] == "user"
            && let Some(text) = text_of(&e["message"]["content"])
        {
            return Some(text);
        }
    }
    None
}

/// A message's text: a string, or its text blocks.
fn text_of(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_a_session_says_about_itself() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("--work-app--");
        fs::create_dir_all(&folder).unwrap();
        let lines = [
            r#"{"type":"session","version":3,"id":"019f-c1","timestamp":"t","cwd":"/work/app"}"#,
            r#"{"type":"model_change","id":"a","provider":"anthropic","modelId":"claude-opus-4-8"}"#,
            r#"{"type":"message","id":"b","message":{"role":"user","content":[{"type":"text","text":"fix the parser"}]}}"#,
            r#"{"type":"message","id":"c","message":{"role":"assistant","model":"claude-opus-4-8","content":[]}}"#,
            r#"{"type":"message","id":"d","message":{"role":"toolResult","content":[{"type":"text","text":"output"}]}}"#,
            r#"{"type":"message","id":"e","message":{"role":"user","content":"and the tests"}}"#,
            r#"{"type":"session_info","id":"f","name":"Parser fixes"}"#,
        ];
        fs::write(
            folder.join("2026-10-22T06-15-06-470Z_019f-c1.jsonl"),
            lines.join("\n") + "\n",
        )
        .unwrap();
        // Nothing said in it yet: not offered.
        fs::write(
            folder.join("2026-10-22T07-00-00-000Z_019f-c2.jsonl"),
            r#"{"type":"session","version":3,"id":"019f-c2","cwd":"/work/app"}"#,
        )
        .unwrap();
        let found = conversations(dir.path(), |_| true);
        assert_eq!(found.len(), 1);
        let c = &found[0];
        assert_eq!((c.agent.as_str(), c.id.as_str()), ("pi", "019f-c1"));
        assert_eq!(c.cwd, Path::new("/work/app"));
        assert_eq!(
            (c.first_prompt.as_deref(), c.last_prompt.as_deref()),
            (Some("fix the parser"), Some("and the tests"))
        );
        assert_eq!(c.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(c.label(), "Parser fixes");
        assert!(conversations(dir.path(), |id| id == "other").is_empty());
        assert_eq!(
            session_file(dir.path(), "019f-c2"),
            Some(folder.join("2026-10-22T07-00-00-000Z_019f-c2.jsonl"))
        );
        assert_eq!(session_file(dir.path(), "019f-c3"), None);
    }

    #[test]
    fn a_directory_of_its_own_holds_the_files_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines =
            vec![r#"{"type":"session","version":3,"id":"019f-d1","cwd":"/work/app"}"#.to_string()];
        // A long system message first: more than the start `file_ends` reads.
        let system = "x".repeat(100 * 1024);
        lines.push(format!(
            r#"{{"type":"message","id":"a","message":{{"role":"system","content":"","sections":{{"preamble":"{system}"}}}}}}"#
        ));
        lines.push(
            r#"{"type":"message","id":"b","message":{"role":"user","content":"fix the parser"}}"#
                .to_string(),
        );
        let filler = "y".repeat(600 * 1024);
        lines.push(format!(
            r#"{{"type":"message","id":"c","message":{{"role":"toolResult","content":"{filler}"}}}}"#
        ));
        fs::write(
            dir.path().join("2026-10-22T06-15-06-470Z_019f-d1.jsonl"),
            lines.join("\n") + "\n",
        )
        .unwrap();
        let found = conversations(dir.path(), |_| true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].first_prompt.as_deref(), Some("fix the parser"));
        assert_eq!(found[0].label(), "fix the parser");
    }
}
