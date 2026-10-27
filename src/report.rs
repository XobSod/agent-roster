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
/// tool's input and output, whole files and command output among them (found
/// in real use: 22 MB of a day's events). Top-level values longer than this
/// aren't stored.
const MAX_VALUE: usize = 4096;

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
    // (eight subagents starting at once, seen lost two) is a state wrong
    // till the agent's next; the agent waits only while roster writes.
    conn.busy_timeout(Duration::from_secs(5))?;
    let payload = trimmed(payload);
    let payload = (!payload.is_empty()).then_some(payload.as_str());
    crate::db::insert_event(&conn, &session, kind, now_ms(), payload)
}

/// The payload without its long top-level values; as it is if it isn't a
/// JSON object.
fn trimmed(payload: &str) -> String {
    let Ok(mut object) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(payload)
    else {
        return payload.to_string();
    };
    let before = object.len();
    object.retain(|_, v| v.to_string().len() <= MAX_VALUE);
    if object.len() == before {
        return payload.to_string();
    }
    serde_json::Value::Object(object).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_are_left_out() {
        let output = "x".repeat(MAX_VALUE * 2);
        let payload = format!(
            r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{{"stdout":"{output}"}}}}"#
        );
        let stored: serde_json::Value = serde_json::from_str(&trimmed(&payload)).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"})
        );
        // Short ones, and anything that isn't an object, stay as they are.
        let short = r#"{"session_id":"c-1",  "model":"claude-opus-5-5"}"#;
        assert_eq!(trimmed(short), short);
        assert_eq!(trimmed("not json"), "not json");
    }
}
