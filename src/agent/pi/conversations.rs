//! pi's conversations, those started outside roster that it can take over:
//! pi keeps each as `<sessions>/--<cwd>--/<timestamp>_<id>.jsonl`, where
//! `<sessions>` is `$PI_CODING_AGENT_SESSION_DIR`, else `sessions` in pi's
//! directory (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`). pi's internal
//! files, so anything unreadable is skipped. (A `sessionDir` in pi's
//! settings isn't followed; the variable is.) pi doesn't say which of them a
//! running pi has open.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::PI;
use crate::agent::Conversation;

/// Where pi keeps its sessions.
pub fn sessions_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        return Some(PathBuf::from(dir));
    }
    let agent = match std::env::var_os("PI_CODING_AGENT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::home_dir()?.join(".pi").join("agent"),
    };
    Some(agent.join("sessions"))
}

/// The conversations pi has kept whose id `wanted` says yes to (only their
/// files are read).
pub fn conversations(sessions: &Path, wanted: impl Fn(&str) -> bool) -> Vec<Conversation> {
    fs::read_dir(sessions)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|dir| fs::read_dir(dir.path()).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| id_of(p).is_some_and(&wanted))
        .filter_map(|p| read(&p))
        .collect()
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
            // Its name, the last one given.
            Some("session_info") => c.custom_title = e["name"].as_str().map(String::from),
            Some("message") => {
                let m = &e["message"];
                match m["role"].as_str() {
                    Some("user") => {
                        if let Some(text) = text_of(&m["content"]) {
                            c.first_prompt.get_or_insert_with(|| text.clone());
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
    c.first_prompt.as_ref()?;
    Some(c)
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
    }
}
