//! Codex's conversations, those started outside roster that it can take over:
//! codex keeps each rollout under `<codex dir>/sessions/<year>/<month>/<day>/`
//! as `rollout-<timestamp>-<id>.jsonl`, and names the threads it has in
//! `<codex dir>/session_index.jsonl`. Both are codex's internal files, so
//! anything unreadable is skipped. Codex doesn't say which conversation a
//! running codex has open (it locks a file per thread, without a pid in it).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use super::CODEX;
use super::transcript::Typed;
use crate::agent::Conversation;
use crate::stats::{Jsonl, Lines};

/// Codex's directory: `$CODEX_HOME`, else `~/.codex`.
pub fn codex_dir() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => dirs::home_dir().map(|h| h.join(".codex")),
    }
}

/// How deep under `sessions` codex puts its files: year, month, day.
const DEPTH: usize = 4;

/// The rollouts codex has kept whose id `wanted` says yes to (only their files
/// are read), newest first.
pub fn conversations(dir: &Path, wanted: impl Fn(&str) -> bool) -> Vec<Conversation> {
    let named = names(&dir.join("session_index.jsonl"));
    let mut found: Vec<Conversation> = files(&dir.join("sessions"), 1)
        .into_iter()
        .filter(|p| id_of(p).is_some_and(&wanted))
        .filter_map(|p| read(&p))
        .map(|mut c| {
            c.title = named.get(&c.id).cloned();
            c
        })
        .collect();
    found.sort_by_key(|c| std::cmp::Reverse(c.modified));
    found
}

/// The rollout files in `dir`, however deep codex has filed them.
pub(super) fn files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    if depth > DEPTH {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                files(&path, depth + 1)
            } else {
                vec![path]
            }
        })
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        })
        .collect()
}

/// The conversation id in a rollout's name, after its timestamp. Codex writes
/// a UUID there, and its `id`s are UUIDs, so it's the file's last 36
/// characters; a name that isn't that says nothing.
pub(super) fn id_of(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() < "rollout-".len() + 37 {
        return None;
    }
    let (head, id) = stem.split_at(stem.len() - 36);
    (head.starts_with("rollout-") && head.ends_with('-') && id.contains('-')).then_some(id)
}

/// What codex calls its threads, by conversation id: names it made itself,
/// from what you asked first.
fn names(index: &Path) -> HashMap<String, String> {
    let mut names = Jsonl::<Names>::new(index);
    let _ = names.read();
    names.tally().0.clone()
}

/// The name codex gave conversation `id`, from its index as it is now.
pub fn name_of(id: &str) -> Option<String> {
    static NAMES: OnceLock<Option<Mutex<Jsonl<Names>>>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        codex_dir().map(|dir| Mutex::new(Jsonl::new(&dir.join("session_index.jsonl"))))
    });
    let mut names = names.as_ref()?.lock().ok()?;
    let _ = names.read();
    names.tally().0.get(id).cloned()
}

/// Codex's index of its threads, by id: a line each time one is named (or
/// renamed), so the last line for an id is its name.
#[derive(Debug, Default)]
struct Names(HashMap<String, String>);

impl Lines for Names {
    fn line(&mut self, line: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str())
            && !name.is_empty()
        {
            self.0.insert(id.to_string(), name.to_string());
        }
    }
}

/// What one rollout says about itself, from its start and end.
fn read(path: &Path) -> Option<Conversation> {
    let (entries, modified) = crate::agent::file_ends(path)?;
    let mut c = Conversation {
        agent: CODEX.to_string(),
        id: id_of(path)?.to_string(),
        path: path.to_path_buf(),
        cwd: PathBuf::new(),
        branch: None,
        custom_title: None,
        title: None,
        first_prompt: None,
        last_prompt: None,
        model: None,
        effort: None,
        modified,
        open_in: None,
    };
    let mut cwd = None;
    let mut typed = Typed::default();
    let mut prompt = |text: String| {
        if c.first_prompt.is_none() {
            c.first_prompt = Some(text.clone());
        }
        c.last_prompt = Some(text);
    };
    for e in &entries {
        let payload = &e["payload"];
        match e["type"].as_str() {
            // The header: its id, where it ran, and the repository it was in.
            Some("session_meta") => {
                if let Some(id) = payload["id"].as_str() {
                    c.id = id.to_string();
                }
                cwd = payload["cwd"].as_str().map(PathBuf::from).or(cwd);
                if let Some(branch) = payload["git"]["branch"]
                    .as_str()
                    .filter(|b| !b.is_empty() && *b != "HEAD")
                {
                    c.branch = Some(branch.to_string());
                }
            }
            // What it ran on, as each turn changed it.
            Some("turn_context") => {
                if let Some(model) = payload["model"].as_str() {
                    c.model = Some(model.to_string());
                }
                if let Some(effort) = payload["effort"].as_str() {
                    c.effort = Some(effort.to_string());
                }
            }
            Some("response_item") if payload["type"] == "message" && payload["role"] == "user" => {
                let kinds: Option<Vec<String>> =
                    payload["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                        .as_array()
                        .map(|kinds| {
                            kinds
                                .iter()
                                .filter_map(|k| k.as_str().map(String::from))
                                .collect()
                        });
                if typed.message(kinds.as_deref())
                    && let Some(text) = crate::agent::message_text(&payload["content"])
                {
                    prompt(text);
                }
            }
            Some("event_msg") if payload["type"] == "user_message" => {
                if typed.event()
                    && let Some(text) = payload["message"]
                        .as_str()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                {
                    prompt(text.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rollout(dir: &Path, id: &str, day: &str, lines: &[&str]) -> PathBuf {
        let folder = dir.join("sessions").join("2026").join("10").join(day);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("rollout-2026-10-{day}T10-00-00-{id}.jsonl"));
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    #[test]
    fn finds_the_id_in_a_rollouts_name() {
        let id = "01a16554-00a8-7983-837e-721a0ace204e";
        assert_eq!(
            id_of(Path::new(&format!(
                "rollout-2026-10-22T22-36-06-{id}.jsonl"
            ))),
            Some(id)
        );
        // Nothing of the kind: nothing to match an id against.
        assert_eq!(id_of(Path::new("rollout-short.jsonl")), None);
    }

    #[test]
    fn reads_what_a_rollout_says_about_itself() {
        let dir = tempfile::tempdir().unwrap();
        let id = "01a16554-00a8-7983-837e-721a0ace204e";
        rollout(
            dir.path(),
            id,
            "21",
            &[
                &format!(
                    r#"{{"timestamp":"2026-10-21T10:00:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/work/app","git":{{"branch":"main"}}}}}}"#
                ),
                r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol","effort":"high"}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>ignored</environment_context>"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["environments.environment_context"]}}}"#,
                r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /work/app

<INSTRUCTIONS>
be brief
</INSTRUCTIONS>"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["agents_md.instructions"]}}}"##,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix it"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"and the tests"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}"#,
                r#"{"type":"turn_context","payload":{"model":"gpt-reserve","effort":"low"}}"#,
            ],
        );
        // Nothing but injected context: not offered.
        rollout(
            dir.path(),
            "01a16554-00a8-7983-837e-721a0ace205f",
            "20",
            &[
                r#"{"type":"session_meta","payload":{"id":"01a16554-00a8-7983-837e-721a0ace205f","cwd":"/work/app"}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>hi</environment_context>"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["environments.environment_context"]}}}"#,
            ],
        );
        // Nothing but a prompt in markup, from an older codex: offered.
        let older = "019d880a-44de-7563-a769-7d41f3a34580";
        rollout(
            dir.path(),
            older,
            "19",
            &[
                &format!(
                    r#"{{"type":"session_meta","payload":{{"id":"{older}","cwd":"/work/app"}}}}"#
                ),
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>hi</environment_context>"}]}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<task>Fix the bug</task>"}]}}"#,
                r#"{"type":"event_msg","payload":{"type":"user_message","message":"<task>Fix the bug</task>","images":[]}}"#,
            ],
        );
        fs::write(
            dir.path().join("session_index.jsonl"),
            format!(
                r#"{{"id":"{id}","thread_name":"Fix the parser","updated_at":"2026-10-21T10:05:00Z"}}"#
            ) + "\n",
        )
        .unwrap();

        let found = conversations(dir.path(), |_| true);
        assert_eq!(found.len(), 2, "{found:?}");
        let by_id = |id: &str| found.iter().find(|c| c.id == id).unwrap();
        assert_eq!(
            by_id(older).first_prompt.as_deref(),
            Some("<task>Fix the bug</task>")
        );
        let c = by_id(id);
        assert_eq!(c.id, id);
        assert_eq!(c.agent, CODEX);
        assert_eq!(c.cwd, PathBuf::from("/work/app"));
        assert_eq!(c.branch.as_deref(), Some("main"));
        assert_eq!(c.title.as_deref(), Some("Fix the parser"));
        assert_eq!(c.first_prompt.as_deref(), Some("fix it"));
        assert_eq!(c.last_prompt.as_deref(), Some("and the tests"));
        assert_eq!(c.model.as_deref(), Some("gpt-reserve"));
        assert_eq!(c.effort.as_deref(), Some("low"));
        assert_eq!(c.open_in, None);

        // Asked for by a part of its id, as `roster adopt` matches them.
        assert_eq!(
            conversations(dir.path(), |i| i.starts_with("01a16554")).len(),
            1
        );
        assert!(conversations(dir.path(), |i| i.starts_with("ffff")).is_empty());
    }

    #[test]
    fn reads_the_names_codex_gives_as_its_index_grows() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_index.jsonl");
        let mut index = fs::File::create(&path).unwrap();
        let mut names = Jsonl::<Names>::new(&path);
        let name = |names: &Jsonl<Names>, id: &str| names.tally().0.get(id).cloned();
        names.read().unwrap();
        assert!(names.tally().0.is_empty());
        writeln!(
            index,
            r#"{{"id":"a","thread_name":"Fix the parser","updated_at":"t"}}"#
        )
        .unwrap();
        // Half a line: it waits for the rest.
        write!(index, r#"{{"id":"b","thread_na"#).unwrap();
        names.read().unwrap();
        assert_eq!(name(&names, "a").as_deref(), Some("Fix the parser"));
        assert_eq!(name(&names, "b"), None);
        writeln!(index, r#"me":"Add tests","updated_at":"t"}}"#).unwrap();
        // Renamed: the last line for it wins. An empty name is none.
        writeln!(
            index,
            r#"{{"id":"a","thread_name":"Parse the config","updated_at":"t"}}"#
        )
        .unwrap();
        writeln!(index, r#"{{"id":"c","thread_name":"","updated_at":"t"}}"#).unwrap();
        names.read().unwrap();
        assert_eq!(name(&names, "a").as_deref(), Some("Parse the config"));
        assert_eq!(name(&names, "b").as_deref(), Some("Add tests"));
        assert_eq!(name(&names, "c"), None);
        // Rewritten shorter: read again from its start.
        fs::write(&path, "{\"id\":\"d\",\"thread_name\":\"Only\"}\n").unwrap();
        names.read().unwrap();
        assert_eq!(names.tally().0.len(), 1);
        assert_eq!(name(&names, "d").as_deref(), Some("Only"));
    }

    #[test]
    fn a_name_is_a_prompt_when_codex_made_neither() {
        let mut c = Conversation {
            agent: CODEX.into(),
            id: "x".into(),
            path: PathBuf::from("/x"),
            cwd: PathBuf::from("/w"),
            branch: None,
            custom_title: None,
            title: None,
            first_prompt: Some("rename the thing".into()),
            last_prompt: None,
            model: None,
            effort: None,
            modified: 0,
            open_in: None,
        };
        assert_eq!(c.label(), "rename the thing");
        c.title = Some("Renaming".into());
        assert_eq!(c.label(), "Renaming");
    }
}
