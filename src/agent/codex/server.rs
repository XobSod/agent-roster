//! Asking codex's own server (`codex app-server`, its JSON-RPC protocol on
//! stdin and stdout) one thing: the handshake its protocol starts with, then
//! the request, answered as id 2. It serves until its stdin ends, which it's
//! given once it has answered.

use serde_json::{Value, json};

/// The server run as `command`, given `args` (`-c`s) too: never checking for
/// an update, which is yours to take as codex starts.
pub(super) fn argv(command: String, args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut argv = vec![
        command,
        "app-server".into(),
        "-c".into(),
        "check_for_update_on_startup=false".into(),
    ];
    argv.extend(args);
    argv
}

/// The lines asking it `method`, with `params` if it takes any.
pub(super) fn requests(method: &str, params: Option<Value>) -> String {
    let mut request = json!({"jsonrpc": "2.0", "id": 2, "method": method});
    if let Some(params) = params {
        request["params"] = params;
    }
    let lines = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "roster", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        request,
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// The answer to the request among the complete lines it printed.
pub(super) fn answer(printed: &[u8]) -> Option<Value> {
    let complete = &printed[..printed.iter().rposition(|b| *b == b'\n')? + 1];
    complete
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(|v| v["id"] == 2)
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
    fn a_request_goes_after_the_handshake_with_its_params() {
        let lines: Vec<Value> = requests("hooks/list", Some(json!({})))
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["method"], "initialize");
        assert_eq!(lines[1]["method"], "initialized");
        assert_eq!(
            lines[2],
            json!({"jsonrpc": "2.0", "id": 2, "method": "hooks/list", "params": {}})
        );
        let lines = requests("account/rateLimits/read", None);
        assert!(!lines.lines().nth(2).unwrap().contains("params"));
    }
}
