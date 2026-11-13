//! A CLI Proxy holding several accounts: asked for every one of them in
//! place of the agents, its accounts labelled, and fallen back from.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::common::TestEnv;

/// The key the fake proxy takes, made up.
const KEY: &str = "made-up-key";

/// A CLI Proxy on a port of its own, answering as CLIProxyAPI does (every
/// value made up), keeping each request it was sent.
struct FakeProxy {
    port: u16,
    sent: Arc<Mutex<Vec<String>>>,
}

impl FakeProxy {
    fn start() -> FakeProxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let kept = sent.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8_lossy(&body).into_owned();
                kept.lock().unwrap().push(format!("{head}\n{body}"));
                let (status, answer) = answer(&head, &body);
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(reply.as_bytes()).ok();
            }
        });
        FakeProxy { port, sent }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// What the fake proxy answers a request.
fn answer(head: &str, body: &str) -> (&'static str, String) {
    if !head.contains(&format!("Bearer {KEY}\r\n")) {
        return (
            "401 Unauthorized",
            r#"{"error":"invalid management key"}"#.into(),
        );
    }
    let first = head.lines().next().unwrap_or_default();
    if first.starts_with("GET /v8/management/credentials ") {
        let listed = json!({"credentials": [
            {"auth_index": "a1", "provider": "claude", "account_type": "oauth",
             "email": "alexandra@example.com", "disabled": false, "requires_sign_in": false},
            {"auth_index": "b2", "provider": "claude", "account_type": "oauth",
             "email": "sam@example.com", "disabled": false},
            {"auth_index": "c3", "provider": "codex", "account_type": "oauth",
             "email": "jordan.lee@example.com"},
            {"auth_index": "d4", "provider": "claude", "account_type": "oauth",
             "email": "off@example.com", "disabled": true},
            {"auth_index": "e5", "provider": "gemini", "account_type": "oauth",
             "email": "g@example.com"},
            {"auth_index": "f6", "provider": "claude", "account_type": "api_key"},
        ]});
        return ("200 OK", listed.to_string());
    }
    if !first.starts_with("POST /v8/management/requests/api-call ") {
        return ("404 Not Found", "{}".into());
    }
    let request: Value = serde_json::from_str(body).unwrap();
    assert_eq!(request["method"], "GET");
    assert_eq!(request["header"]["Authorization"], "Bearer $TOKEN$");
    let upstream = |status: u64, body: Value| {
        json!({"status_code": status, "header": {}, "body": body.to_string()}).to_string()
    };
    let said = match (
        request["url"].as_str().unwrap(),
        request["auth_index"].as_str().unwrap(),
    ) {
        ("https://api.anthropic.com/api/oauth/usage", "a1") => upstream(
            200,
            json!({
                "five_hour": {"utilization": 56.0, "resets_at": "2099-01-01T00:00:00+00:00"},
                "seven_day": {"utilization": 37.0, "resets_at": "2099-01-02T00:00:00+00:00"},
            }),
        ),
        ("https://api.anthropic.com/api/oauth/usage", "b2") => upstream(429, json!({})),
        ("https://api.anthropic.com/api/oauth/profile", "a1") => upstream(
            200,
            json!({
                "organization": {"organization_type": "claude_max", "rate_limit_tier": "default_claude_max_5x"},
            }),
        ),
        ("https://api.anthropic.com/api/oauth/profile", _) => upstream(
            200,
            json!({
                "organization": {"organization_type": "claude_pro", "rate_limit_tier": "default_claude_ai"},
            }),
        ),
        ("https://chatgpt.com/backend-api/wham/usage", "c3") => upstream(
            200,
            json!({
                "plan_type": "plus",
                "rate_limit": {"limit_reached": false,
                    "primary_window": {"used_percent": 7, "limit_window_seconds": 18000, "reset_at": 4_070_908_800_i64},
                    "secondary_window": {"used_percent": 41, "limit_window_seconds": 604_800, "reset_at": 4_070_995_200_i64}},
            }),
        ),
        (url, id) => panic!("asked {url} for {id}"),
    };
    ("200 OK", said)
}

/// Sets the proxy's key, given on stdin as a person would.
fn set_key(t: &TestEnv, key: &str) {
    let mut child = t
        .command(&["config", "set", "cli_proxy.key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    (child.stdin.take().unwrap())
        .write_all(format!("{key}\n").as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

#[test]
fn every_account_the_cli_proxy_holds_is_asked() {
    let Some(t) = TestEnv::new("proxy-usage") else {
        return;
    };
    let proxy = FakeProxy::start();
    set_key(&t, KEY);
    t.ok(&["config", "set", "cli_proxy.url", &proxy.url()]);
    t.ok(&["config", "set", "cli_proxy.enabled", "on"]);
    t.ok(&["proxy", "label", "claude", "Alexandra@example.com", "Max"]);
    let config = std::fs::read_to_string(t.config_dir().join("config.toml")).unwrap();
    assert!(
        config.contains("[[cli_proxy.labels]]") && config.contains("label = \"Max\""),
        "{config}"
    );

    // One couldn't say: said, the others shown, and an error in the end.
    let out = t.run(&["usage"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(!out.status.success());
    assert!(stderr.contains("1 couldn't say"), "{stderr}");
    for shown in [
        &format!("via CLI Proxy {}\n", proxy.url()),
        "claude · Max (alexandra@example.com)  max 5x\n  5 hours   44% left",
        "  Weekly    63% left",
        "claude · off@example.com  disabled in the CLI Proxy",
        "claude · sam@example.com  can't tell: Claude's usage service: it asked to wait (429)",
        "codex · jordan.lee@example.com  plus\n  5 hours   93% left",
        "gemini · g@example.com  usage unknown: roster can't ask its service",
    ] {
        assert!(stdout.contains(shown), "{shown:?} in\n{stdout}");
    }
    // An API key it holds has no plan's limits: not there. Nor the agents,
    // whose services it holds accounts of.
    assert!(
        !stdout.contains("f6") && !stdout.contains("\nclaude  "),
        "{stdout}"
    );
    let json: Value = serde_json::from_slice(&t.run(&["usage", "--json"]).stdout).unwrap();
    let max = (json.as_array().unwrap().iter())
        .find(|i| i["account"]["email"] == "alexandra@example.com")
        .unwrap();
    assert_eq!(max["source"], "cli_proxy");
    assert_eq!(max["provider"], "claude");
    assert_eq!(max["account"]["label"], "Max");
    assert_eq!(max["plan"], "max 5x");
    assert_eq!(max["windows"][1]["minutes"], 10_080);
    assert!(max.get("agent").is_none(), "{max}");

    // Only the fixed reads went through it, each with its key; the
    // accounts' logins, never seen, put in by the proxy.
    let sent = proxy.sent.lock().unwrap().clone();
    assert!(sent.iter().all(|r| r.contains(&format!("Bearer {KEY}"))));
    for request in sent.iter().filter(|r| r.starts_with("POST")) {
        let body: Value = serde_json::from_str(request.rsplit_once("\n").unwrap().1).unwrap();
        assert!(
            [
                "https://api.anthropic.com/api/oauth/usage",
                "https://api.anthropic.com/api/oauth/profile",
                "https://chatgpt.com/backend-api/wham/usage",
            ]
            .contains(&body["url"].as_str().unwrap()),
            "{request}"
        );
    }

    // A Claude account whose usage couldn't be read isn't asked its plan;
    // one whose usage could, is.
    let profile = |id: &str| {
        let id = format!(r#""auth_index":"{id}""#);
        sent.iter()
            .any(|r| r.contains("oauth/profile") && r.contains(&id))
    };
    assert!(profile("a1") && !profile("b2"), "{sent:#?}");

    // Its accounts, labelled, and how it stands; nothing asked upstream.
    let before = proxy.sent.lock().unwrap().len();
    let out = t.ok(&["proxy"]);
    assert!(out.contains("connected, 5 accounts"), "{out}");
    let row = |email: &str| {
        out.lines()
            .find(|l| l.contains(email))
            .unwrap_or_default()
            .to_string()
    };
    assert!(row("alexandra@example.com").contains(" Max"), "{out}");
    assert!(
        out.contains("off@example.com") && out.contains("disabled"),
        "{out}"
    );
    assert_eq!(proxy.sent.lock().unwrap().len(), before + 1);
    let listed: Value = serde_json::from_str(&t.ok(&["proxy", "--json"])).unwrap();
    assert_eq!(listed["state"], "connected");
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 5);

    // The key refused: the agents in its place, and why.
    set_key(&t, "another-made-up-key");
    let out = t.run(&["usage"]);
    assert!(
        text(&out.stderr)
            .contains("the CLI Proxy wasn't asked, the agents were: it refused the key (401)"),
        "{out:?}"
    );
    set_key(&t, KEY);

    // Not there: likewise; `roster proxy` fails, the labels still listed.
    let gone = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    t.ok(&[
        "config",
        "set",
        "cli_proxy.url",
        &format!("http://127.0.0.1:{gone}"),
    ]);
    let out = t.run(&["usage"]);
    assert!(text(&out.stderr).contains("can't reach it"), "{out:?}");
    let out = t.run(&["proxy"]);
    assert!(!out.status.success());
    let stdout = text(&out.stdout);
    assert!(stdout.contains("can't be asked"), "{stdout}");
    assert!(
        stdout.contains("Max") && stdout.contains("not listed"),
        "{stdout}"
    );

    // A label taken out; off, nothing is asked.
    t.ok(&["proxy", "label", "claude", "alexandra@example.com"]);
    let config = std::fs::read_to_string(t.config_dir().join("config.toml")).unwrap();
    assert!(!config.contains("cli_proxy.labels"), "{config}");
    t.ok(&["config", "set", "cli_proxy.enabled", "off"]);
    assert!(t.ok(&["proxy"]).contains(": off"));
}
