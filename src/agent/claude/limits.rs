//! What Claude's account has used of its limits, asked of Claude Code itself
//! in the environment Claude's sessions get (where an env file may point
//! `CLAUDE_CONFIG_DIR` elsewhere): in its SDK mode, its `get_usage` control
//! request, which Claude answers with what its own `/usage` shows, reading its
//! own login and asking the usage service itself. Roster never sees the login.
//! Claude calls the request experimental.

use anyhow::{Result, bail};
use serde_json::Value;

use crate::agent::{Ask, Question};
use crate::launch::Input;
use crate::usage::{Usage, Window};
use crate::util::{iso_ms, now_ms};

/// What asks Claude Code its plan's limits, as its SDK does: `initialize`,
/// then `get_usage` without the scan of the local transcripts, answered as
/// `r2`.
const GET_USAGE: &str = concat!(
    r#"{"type":"control_request","request_id":"r1","request":{"subtype":"initialize"}}"#,
    "\n",
    r#"{"type":"control_request","request_id":"r2","request":{"subtype":"get_usage","skip_behaviors":true}}"#,
    "\n",
);

/// Asks Claude Code itself, run as `command`, in its SDK mode (no prompt, no
/// model called), with no MCP server started and none of the user's or
/// projects' settings read (nor their hooks run): it serves until its input
/// ends, which it's given once it has answered. Without `ANTHROPIC_API_KEY`:
/// there, Claude would take it over its login, where its sessions take it
/// only if told to. None if no plan's limits apply (Claude on an API key, or
/// never logged in).
pub(super) fn ask(command: String, ask: &Ask<'_>) -> Option<Result<Usage>> {
    let answered = |printed: &[u8]| get_usage(printed).is_some();
    let question = Question {
        argv: [
            // By its path: the env file's PATH may have no env.
            "/usr/bin/env",
            "-u",
            "ANTHROPIC_API_KEY",
            command.as_str(),
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
            "--setting-sources",
            "",
        ]
        .map(String::from)
        .into(),
        input: Some(Input {
            text: GET_USAGE.as_bytes(),
            answered: &answered,
        }),
    };
    let printed = match ask(question) {
        Ok(printed) => printed,
        Err(e) => return Some(Err(e.context("Claude didn't answer"))),
    };
    let Some(answer) = get_usage(printed.as_bytes()) else {
        return Some(Err(anyhow::anyhow!("Claude didn't answer")));
    };
    read_usage(&answer, now_ms()).transpose()
}

/// Claude's answer to `get_usage` among the complete lines it printed.
fn get_usage(printed: &[u8]) -> Option<Value> {
    let complete = &printed[..printed.iter().rposition(|b| *b == b'\n')? + 1];
    complete
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(|v| v["type"] == "control_response" && v["response"]["request_id"] == "r2")
}

/// What Claude's answer says, as of `now`: its plan and the windows (each
/// used 0 to 100, and when it resets); None if no plan's limits apply.
fn read_usage(answer: &Value, now: i64) -> Result<Option<Usage>> {
    let response = &answer["response"];
    if response["subtype"] == "error" {
        let error = response["error"].as_str().unwrap_or("an error");
        bail!("Claude answered {error}");
    }
    let usage = &response["response"];
    if usage["rate_limits_available"] != true {
        return Ok(None);
    }
    // Claude couldn't ask the usage service, and had nothing kept from before.
    let limits = &usage["rate_limits"];
    if !limits.is_object() {
        bail!("Claude couldn't ask its usage service; roster asks again later");
    }
    let windows: Vec<Window> = [("five_hour", 300), ("seven_day", 10_080)]
        .into_iter()
        .filter_map(|(key, minutes)| {
            let window = &limits[key];
            Some(Window {
                minutes,
                used: window["utilization"].as_f64()?,
                resets_at: window["resets_at"].as_str().and_then(iso_ms),
            })
        })
        .collect();
    Ok((!windows.is_empty()).then(|| Usage {
        plan: usage["subscription_type"].as_str().map(String::from),
        windows,
        notes: Vec::new(),
        at: now,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_answers_its_plans_limits() {
        // Claude 2.1.311's answer, as its SDK schema has it (cut short).
        // Among other things it prints, before and between.
        let printed = br#"not json
{"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{}}}
[1]
{"type":"system","subtype":"init"}
{"type":"control_response","response":{"subtype":"success","request_id":"r2","response":{"session":{},"behaviors":null,"subscription_type":"max","rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":31.0,"resets_at":"2026-10-25T18:20:00.471005+00:00"},"seven_day":{"utilization":52.0,"resets_at":null},"seven_day_opus":null}}}}
"#;
        let answer = get_usage(printed).unwrap();
        let usage = read_usage(&answer, 7).unwrap().unwrap();
        assert_eq!((usage.plan.as_deref(), usage.at), (Some("max"), 7));
        assert_eq!(
            usage.windows,
            [
                Window {
                    minutes: 300,
                    used: 31.0,
                    resets_at: Some(1_792_952_400_471)
                },
                Window {
                    minutes: 10_080,
                    used: 52.0,
                    resets_at: None
                },
            ]
        );
        // Not a line yet: no answer.
        assert!(get_usage(&printed[..printed.len() - 1]).is_none());
        // No plan's limits (an API key): nothing to show.
        let none = serde_json::json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "r2", "response": {"rate_limits_available": false, "rate_limits": null}}});
        assert_eq!(read_usage(&none, 0).unwrap(), None);
        let answer = |limits| {
            serde_json::json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": "r2", "response": {"rate_limits_available": true, "rate_limits": limits}}})
        };
        // A window it has no figure for isn't shown; with neither, nothing is.
        let usage = read_usage(
            &answer(
                serde_json::json!({"five_hour": {"utilization": null}, "seven_day": {"utilization": 5}}),
            ),
            0,
        );
        assert_eq!(usage.unwrap().unwrap().windows.len(), 1);
        let no_figures = serde_json::json!({"five_hour": null, "seven_day": {"utilization": null}});
        assert_eq!(read_usage(&answer(no_figures), 0).unwrap(), None);
        // A plan's limits it couldn't read (the service busy): said, so what
        // it said before is kept.
        let err = read_usage(&answer(serde_json::Value::Null), 0).unwrap_err();
        assert!(err.to_string().contains("couldn't ask"), "{err}");
        // Refused: said.
        let refused = serde_json::json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": "r2", "error": "get_usage is not supported in this context"}});
        assert!(
            read_usage(&refused, 0)
                .unwrap_err()
                .to_string()
                .contains("not supported")
        );
    }
}
