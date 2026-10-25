//! What the agents' accounts have used of their limits: Claude's asked of
//! Claude.ai's usage service with Claude's login, through curl; codex's of
//! codex's own server.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::Value;

use crate::codex::with_fake_codex;
use crate::common::TestEnv;

/// Stand-in for curl as roster calls it: the headers on its stdin, its
/// status printed last. It answers only Claude's login, and says what
/// arguments it got, so the test can tell the token wasn't among them.
const FAKE_CURL: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/curl-args"
headers=$(cat)
case "$headers" in
  *"Authorization: Bearer t0k"*) ;;
  *) printf '{}\n401'; exit 0 ;;
esac
printf '{"five_hour":{"utilization":31.0,"resets_at":"2099-01-01T00:00:00+00:00"},"seven_day":{"utilization":95.0,"resets_at":"2099-01-02T00:00:00Z"}}\n200'
"#;

#[test]
fn usage_is_asked_of_each_agents_service() {
    let Some(t) = TestEnv::new("usage") else {
        return;
    };
    with_fake_codex(&t);
    // curl found through the env file, as an agent's own commands are.
    let bin = t.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let curl = bin.join("curl");
    fs::write(&curl, FAKE_CURL).unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        t.config_dir().join("env"),
        format!("PATH=\"{}:$PATH\"\n", bin.display()),
    )
    .unwrap();
    // Not logged in to Claude.ai: Claude says nothing.
    let out = t.ok(&["usage"]);
    assert!(
        out.starts_with("codex  plus\n") && !out.contains("claude"),
        "{out}"
    );

    fs::create_dir_all(t.claude_dir()).unwrap();
    let login = t.claude_dir().join(".credentials.json");
    fs::write(
        &login,
        r#"{"claudeAiOauth":{"accessToken":"t0k","expiresAt":4102444800000,"subscriptionType":"pro"}}"#,
    )
    .unwrap();

    let out = t.ok(&["usage"]);
    // Codex is the default agent here, so it comes first; codex's server,
    // serving until its stdin ends, is ended once it has answered.
    let (codex, claude) = out.split_once("\n\n").unwrap();
    assert!(codex.starts_with("codex  plus\n"), "{out}");
    assert!(
        codex.contains("5 hours   93% left  ▼ slow     resets "),
        "{out}"
    );
    assert!(
        codex.contains("Weekly    87% left  ▼ slow     resets "),
        "{out}"
    );
    assert!(claude.starts_with("claude  pro\n"), "{out}");
    assert!(
        claude.contains("5 hours   69% left  ▲ fast     resets "),
        "{out}"
    );
    // 95% of a week, its day only beginning: used faster than its time
    // goes, as `--json` says below too.
    assert!(
        claude.contains("Weekly     5% left  ▲ fast     resets "),
        "{out}"
    );
    assert!(!claude.contains(" · "), "{out}");
    let args = fs::read_to_string(bin.join("curl-args")).unwrap();
    assert!(args.contains("@-") && !args.contains("t0k"), "{args}");

    let json: Value = serde_json::from_str(&t.ok(&["usage", "--json"])).unwrap();
    assert_eq!(json[0]["agent"], "codex");
    assert_eq!(json[0]["windows"][1]["minutes"], 10_080);
    assert_eq!(json[1]["windows"][1]["pace"], "fast");

    // Claude's sessions are given another account by the env file: that's
    // the one asked about.
    let elsewhere = t.path("claude-elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(
        elsewhere.join(".credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"t0k","expiresAt":4102444800000,"subscriptionType":"max"}}"#,
    )
    .unwrap();
    fs::remove_file(&login).unwrap();
    fs::write(
        t.config_dir().join("env"),
        format!(
            "PATH=\"{}:$PATH\"\nCLAUDE_CONFIG_DIR=\"{}\"\n",
            bin.display(),
            elsewhere.display()
        ),
    )
    .unwrap();
    let out = t.ok(&["usage"]);
    assert!(out.contains("claude  max\n"), "{out}");
    fs::rename(elsewhere.join(".credentials.json"), &login).unwrap();
    fs::write(
        t.config_dir().join("env"),
        format!("PATH=\"{}:$PATH\"\n", bin.display()),
    )
    .unwrap();

    // Claude's login expired: said, and the command fails, codex's still
    // shown.
    fs::write(
        &login,
        r#"{"claudeAiOauth":{"accessToken":"old","expiresAt":1000}}"#,
    )
    .unwrap();
    let out = t.run(&["usage"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(
        stdout.contains("codex  plus")
            && stdout.contains("claude  can't tell: Claude's login has expired"),
        "{stdout}"
    );
}
