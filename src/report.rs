//! `roster report <kind>`: called by agent hooks. Inserts one event and exits.
//!
//! It must be silent and always exit 0, whatever goes wrong: for Claude, a `Stop`
//! hook exiting with code 2 means "don't stop, keep going". That's also why `main`
//! dispatches here before clap parses (and possibly rejects) the arguments.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::paths::Paths;
use crate::util::now_ms;

/// Hook payloads are small JSON objects; anything bigger is cut off.
const MAX_PAYLOAD: u64 = 1 << 20;

/// A payload can carry what roster never reads: Claude's after a tool has the
/// tool's input and output, whole files and command output among them (22
/// MB of a day's events). Top-level values longer than this aren't stored,
/// but for their plain members no longer than `MAX_MEMBER` (a long
/// command's description, a long output's background task). A prompt is
/// kept whole: Claude's notice that a task it left running ended comes as
/// one, the task's result in it, and is read.
const MAX_VALUE: usize = 4096;
const MAX_MEMBER: usize = 256;

/// What roster adds to a payload that names the agent's transcript: how
/// long it was as the agent reported (bytes), so what the agent writes after
/// is told from what it wrote before.
pub const TRANSCRIPT_SIZE: &str = "roster_transcript_size";

pub fn run(args: &[String]) -> ! {
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(|| report(args));
    std::process::exit(0);
}

fn report(args: &[String]) -> Result<()> {
    let kind = args.first().context("no event kind")?;
    let session = std::env::var("ROSTER_SESSION").context("ROSTER_SESSION not set")?;
    let mut payload = String::new();
    // Hooks get JSON on stdin; when run by hand from a terminal, don't wait for it.
    if !std::io::stdin().is_terminal() {
        std::io::stdin()
            .take(MAX_PAYLOAD)
            .read_to_string(&mut payload)?;
    }
    let payload = payload.trim();
    let db = match std::env::var_os("ROSTER_DB") {
        Some(path) => PathBuf::from(path),
        None => Paths::from_env()?.db(),
    };
    // Never create the database or run migrations from a hook.
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // As long as roster itself waits: a report lost while roster writes
    // (eight subagents starting at once, say) is a state wrong
    // till the agent's next; the agent waits only while roster writes.
    conn.busy_timeout(Duration::from_secs(5))?;
    let payload = stored(payload);
    let payload = (!payload.is_empty()).then_some(payload.as_str());
    crate::db::insert_event(&conn, &session, kind, now_ms(), payload)
}

/// The payload as it's stored: without its long top-level values, but for
/// their short plain members, and with its transcript's size; as it is if it
/// isn't a JSON object.
fn stored(payload: &str) -> String {
    use serde_json::Value;
    let Ok(mut object) = serde_json::from_str::<serde_json::Map<String, Value>>(payload) else {
        return payload.to_string();
    };
    let mut changed = false;
    object.retain(|k, v| {
        if k == "prompt" || v.to_string().len() <= MAX_VALUE {
            return true;
        }
        changed = true;
        let Value::Object(members) = v else {
            return false;
        };
        members
            .retain(|_, m| !(m.is_object() || m.is_array()) && m.to_string().len() <= MAX_MEMBER);
        !members.is_empty()
    });
    let size = (object.get("transcript_path").and_then(Value::as_str))
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|m| m.len());
    if let Some(size) = size {
        object.insert(TRANSCRIPT_SIZE.into(), size.into());
    } else if !changed {
        return payload.to_string();
    }
    Value::Object(object).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_are_left_out_but_for_their_short_members_and_a_prompt() {
        let output = "x".repeat(MAX_VALUE * 2);
        let edit = "y".repeat(MAX_MEMBER * 2);
        let payload = format!(
            r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":"{output}","tool_response":{{"stdout":"{output}","oldString":"{edit}","structuredPatch":[1],"backgroundTaskId":"b1","interrupted":false}}}}"#
        );
        let kept: serde_json::Value = serde_json::from_str(&stored(&payload)).unwrap();
        assert_eq!(
            kept,
            serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": "Bash",
                "tool_response": {"backgroundTaskId": "b1", "interrupted": false}})
        );
        let notice = format!("<task-notification>\n<result>{output}</result>");
        let prompt = serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": notice});
        assert_eq!(stored(&prompt.to_string()), prompt.to_string());
        let short = r#"{"session_id":"c-1",  "model":"claude-opus-5-5"}"#;
        assert_eq!(stored(short), short);
        assert_eq!(stored("not json"), "not json");
    }

    #[test]
    fn a_payload_naming_its_transcript_gets_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        std::fs::write(&path, "0123456789").unwrap();
        let payload = serde_json::json!({"transcript_path": path}).to_string();
        let kept: serde_json::Value = serde_json::from_str(&stored(&payload)).unwrap();
        assert_eq!(kept[TRANSCRIPT_SIZE], 10);
        // Not there (yet): nothing said.
        let gone = serde_json::json!({"transcript_path": dir.path().join("none")}).to_string();
        assert_eq!(stored(&gone), gone);
    }
}
