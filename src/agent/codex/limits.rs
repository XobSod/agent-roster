//! What codex's account has used of its limits: asked of codex's own server
//! (`codex app-server`, its JSON-RPC protocol on stdin and stdout), and
//! written by codex in its rollouts with every response, the same figures.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::launch::Input;
use crate::usage::{Usage, Window};
use crate::util::{local_when, now_ms};

/// The requests asking codex's server for the limits: the handshake its
/// protocol starts with, then `account/rateLimits/read`, answered as id 2.
fn requests() -> String {
    let lines = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "roster", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read"}),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// The answer to request 2 among the complete lines codex's server printed.
fn answer(printed: &[u8]) -> Option<Value> {
    let complete = &printed[..printed.iter().rposition(|b| *b == b'\n')? + 1];
    complete
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(|v| v["id"] == 2)
}

/// Asks codex's server, run as `command`; it serves until its stdin ends, so
/// it's ended once it has answered. None if codex has no account to ask
/// about (not logged in, or on an API key).
pub(super) fn ask(command: String, ask: &crate::agent::Ask<'_>) -> Option<Result<Usage>> {
    let requests = requests();
    let answered = |printed: &[u8]| answer(printed).is_some();
    let question = crate::agent::Question {
        argv: vec![
            command,
            "app-server".into(),
            "-c".into(),
            "check_for_update_on_startup=false".into(),
        ],
        input: Some(Input {
            text: requests.as_bytes(),
            answered: Some(&answered),
        }),
    };
    let printed = match ask(question) {
        Ok(printed) => printed,
        Err(e) => return Some(Err(e.context("codex's server didn't answer"))),
    };
    let Some(answer) = answer(printed.as_bytes()) else {
        return Some(Err(anyhow::anyhow!("codex's server didn't answer")));
    };
    match read_answer(&answer, now_ms()) {
        Ok(usage) => usage.map(Ok),
        Err(e) => Some(Err(e)),
    }
}

/// What the server's answer says, as of `now`; None for no account.
fn read_answer(answer: &Value, now: i64) -> Result<Option<Usage>> {
    if let Some(message) = answer["error"]["message"].as_str() {
        if message.contains("authentication required") {
            return Ok(None);
        }
        bail!("codex's server: {message}");
    }
    let result = &answer["result"];
    let Some(mut usage) = limits(&result["rateLimits"], now) else {
        return Ok(None);
    };
    usage
        .notes
        .extend(reset_credits(&result["rateLimitResetCredits"], now));
    Ok(Some(usage))
}

/// The resets codex has granted the account, to use when it runs out:
/// "2 full resets left, the first expires Sun 1 Nov 10:34".
fn reset_credits(credits: &Value, now: i64) -> Option<String> {
    let available: Vec<&Value> = credits["credits"]
        .as_array()?
        .iter()
        .filter(|c| c["status"] == "available")
        .collect();
    let first = available
        .iter()
        .filter_map(|c| c["expiresAt"].as_i64())
        .min()
        .map(|at| local_when(at * 1000, now));
    Some(match (available.len(), first) {
        (0, _) => return None,
        (1, Some(at)) => format!("1 full reset left, it expires {at}"),
        (1, None) => "1 full reset left".into(),
        (n, Some(at)) => format!("{n} full resets left, the first expires {at}"),
        (n, None) => format!("{n} full resets left"),
    })
}

/// The account's limits, as codex's rollouts write them (snake_case) or its
/// server answers (camelCase), as of `at`: its windows (`primary`, five
/// hours; `secondary`, a week). None for none, or for a limit of another kind
/// than the account's (`limit_id` other than `codex`).
pub(super) fn limits(limits: &Value, at: i64) -> Option<Usage> {
    let field = |v: &'_ Value, names: [&str; 2]| -> Value {
        names
            .iter()
            .map(|n| &v[*n])
            .find(|f| !f.is_null())
            .cloned()
            .unwrap_or(Value::Null)
    };
    if let Some(id) = field(limits, ["limit_id", "limitId"]).as_str()
        && id != "codex"
    {
        return None;
    }
    let mut windows: Vec<Window> = ["primary", "secondary"]
        .iter()
        .filter_map(|key| {
            let w = &limits[*key];
            Some(Window {
                minutes: field(w, ["window_minutes", "windowDurationMins"]).as_u64()?,
                used: field(w, ["used_percent", "usedPercent"]).as_f64()?,
                resets_at: field(w, ["resets_at", "resetsAt"])
                    .as_i64()
                    .map(|s| s * 1000),
            })
        })
        .collect();
    if windows.is_empty() {
        return None;
    }
    windows.sort_by_key(|w| w.minutes);
    Some(Usage {
        plan: field(limits, ["plan_type", "planType"])
            .as_str()
            .map(String::from),
        windows,
        notes: Vec::new(),
        at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_answer_is_found_among_what_it_prints() {
        let printed = concat!(
            r#"{"id":1,"result":{"userAgent":"roster/0.164.0"}}"#,
            "\n",
            r#"{"method":"remoteControl/status/changed","params":{"status":"disabled"}}"#,
            "\n",
            r#"{"id":2,"result":{}}"#,
        );
        // Its line not finished yet.
        assert_eq!(answer(printed.as_bytes()), None);
        let printed = format!("{printed}\n");
        assert_eq!(
            answer(printed.as_bytes()),
            Some(json!({"id": 2, "result": {}}))
        );
    }

    #[test]
    fn the_servers_answer_gives_the_windows_and_resets_left() {
        // As codex 0.164 answers.
        let answer = json!({"id": 2, "result": {
            "rateLimits": {"limitId": "codex", "limitName": null,
                "primary": {"usedPercent": 7, "windowDurationMins": 300, "resetsAt": 1_792_956_866},
                "secondary": {"usedPercent": 13, "windowDurationMins": 10_080, "resetsAt": 1_793_525_659},
                "credits": {"hasCredits": false}, "planType": "plus"},
            "rateLimitResetCredits": {"availableCount": 2, "credits": [
                {"status": "available", "expiresAt": 1_794_001_279},
                {"status": "available", "expiresAt": 1_793_296_441},
                {"status": "used", "expiresAt": 1_700_000_000}]}}});
        let usage = read_answer(&answer, 5).unwrap().unwrap();
        assert_eq!(usage.plan.as_deref(), Some("plus"));
        assert_eq!(usage.at, 5);
        assert_eq!(
            usage.windows,
            [
                Window {
                    minutes: 300,
                    used: 7.0,
                    resets_at: Some(1_792_956_866_000)
                },
                Window {
                    minutes: 10_080,
                    used: 13.0,
                    resets_at: Some(1_793_525_659_000)
                },
            ]
        );
        let first = local_when(1_793_296_441_000, 5);
        assert_eq!(
            usage.notes,
            [format!("2 full resets left, the first expires {first}")]
        );
    }

    #[test]
    fn no_account_is_no_usage_and_another_error_is_said() {
        let answer = json!({"error": {"code": -32600, "message": "codex account authentication required to read rate limits"}, "id": 2});
        assert_eq!(read_answer(&answer, 0).unwrap(), None);
        let answer = json!({"error": {"code": -32603, "message": "upstream timed out"}, "id": 2});
        let err = read_answer(&answer, 0).unwrap_err().to_string();
        assert_eq!(err, "codex's server: upstream timed out");
        let answer =
            json!({"id": 2, "result": {"rateLimits": {"primary": null, "secondary": null}}});
        assert_eq!(read_answer(&answer, 0).unwrap(), None);
    }

    #[test]
    fn a_rollouts_limits_are_the_accounts_unless_another_kind() {
        // As codex 0.120 to 0.164 write them, with every `token_count`.
        let written = json!({"limit_id": "codex", "limit_name": null,
            "primary": {"used_percent": 17.0, "window_minutes": 300, "resets_at": 1_792_938_859},
            "secondary": {"used_percent": 3.0, "window_minutes": 10_080, "resets_at": 1_793_525_659},
            "credits": null, "plan_type": "plus"});
        let usage = limits(&written, 9).unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].used, 17.0);
        assert_eq!(usage.plan.as_deref(), Some("plus"));
        let mut other = written.clone();
        other["limit_id"] = "premium".into();
        assert_eq!(limits(&other, 9), None);
        assert_eq!(limits(&Value::Null, 9), None);
    }
}
