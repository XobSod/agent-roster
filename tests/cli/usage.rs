//! What the agents' accounts have used of their limits: Claude's asked of
//! Claude itself, codex's of codex's own server.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::Value;

use crate::codex::with_fake_codex;
use crate::common::TestEnv;

/// What the fake Claude answers `get_usage` with, for a plan.
fn plan(name: &str) -> String {
    format!(
        r#"{{"subscription_type":"{name}","rate_limits_available":true,"rate_limits":{{"five_hour":{{"utilization":31.0,"resets_at":"2099-01-01T00:00:00+00:00"}},"seven_day":{{"utilization":95.0,"resets_at":"2099-01-02T00:00:00Z"}}}}}}"#
    )
}

#[test]
fn usage_is_asked_of_each_agent() {
    let Some(t) = TestEnv::new("usage") else {
        return;
    };
    with_fake_codex(&t);
    fs::create_dir_all(t.claude_dir()).unwrap();
    fs::write(t.claude_dir().join("usage.json"), plan("pro")).unwrap();
    // Claude by its name, found on the PATH the env file gives, as its
    // sessions find it.
    let bin = t.path("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(t.path("fake-agent"), bin.join("claude")).unwrap();
    t.ok(&["config", "set", "agents.claude.command", "claude"]);
    let path = format!("PATH=\"{}:$PATH\"\n", bin.display());
    fs::write(t.config_dir().join("env"), &path).unwrap();
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

    let json: Value = serde_json::from_str(&t.ok(&["usage", "--json"])).unwrap();
    assert_eq!(json[0]["agent"], "codex");
    assert_eq!(json[0]["windows"][1]["minutes"], 10_080);
    assert_eq!(json[1]["windows"][1]["pace"], "fast");

    // The env file, printing as it goes, is sourced once for every agent
    // asked, and what it prints isn't taken for what they print.
    let runs = t.path("env-runs");
    fs::write(
        t.config_dir().join("env"),
        format!(
            "echo run >> \"{}\"\necho env loaded\n{path}",
            runs.display()
        ),
    )
    .unwrap();
    let out = t.ok(&["usage"]);
    assert!(
        out.contains("codex  plus\n") && out.contains("claude  pro\n"),
        "{out}"
    );
    assert_eq!(fs::read_to_string(&runs).unwrap(), "run\n");

    // The env file failing is said as that, for each agent, not as what
    // each couldn't do.
    fs::write(t.config_dir().join("env"), "false\n").unwrap();
    let out = t.run(&["usage"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let failed = format!(
        "can't tell: the env file {} failed: ",
        t.config_dir().join("env").display()
    );
    assert!(
        !out.status.success()
            && stdout.contains(&format!("codex  {failed}"))
            && stdout.contains(&format!("claude  {failed}")),
        "{stdout}"
    );

    // Claude's sessions are given another account by the env file: that's
    // the one asked about.
    let elsewhere = t.path("claude-elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("usage.json"), plan("max")).unwrap();
    fs::write(
        t.config_dir().join("env"),
        format!("{path}CLAUDE_CONFIG_DIR=\"{}\"\n", elsewhere.display()),
    )
    .unwrap();
    let out = t.ok(&["usage"]);
    assert!(out.contains("claude  max\n"), "{out}");

    // Turned off, nothing is asked: neither Claude nor codex run, nor the
    // env file sourced.
    let ended = t.claude_dir().join("sdk-ended");
    fs::remove_file(&ended).unwrap();
    let (codex, real) = (t.path("fake-codex"), t.path("fake-codex-real"));
    let (codex_runs, env_runs) = (t.path("codex-runs"), t.path("env-runs-off"));
    fs::rename(&codex, &real).unwrap();
    fs::write(
        &codex,
        format!(
            "#!/bin/sh\necho run >> '{}'\nexec '{}' \"$@\"\n",
            codex_runs.display(),
            real.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        t.config_dir().join("env"),
        format!("echo run >> '{}'\n{path}", env_runs.display()),
    )
    .unwrap();
    t.ok(&["config", "set", "account_limits", "false"]);
    let out = t.run(&["usage"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("account_limits is off"),
        "{err}"
    );
    assert!(!ended.exists());
    assert!(!codex_runs.exists() && !env_runs.exists());
    // Turned on again, all of them run.
    t.ok(&["config", "set", "account_limits", "true"]);
    t.ok(&["usage"]);
    assert!(ended.exists() && codex_runs.exists() && env_runs.exists());

    // Claude can't tell, codex can: the command fails, codex's still shown.
    fs::write(
        t.claude_dir().join("usage.json"),
        r#"{"subscription_type":"pro","rate_limits_available":true,"rate_limits":null}"#,
    )
    .unwrap();
    let out = t.run(&["usage"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success()
            && stdout.contains("codex  plus\n")
            && stdout.contains("claude  can't tell: Claude couldn't ask its usage service"),
        "{stdout}"
    );
}

#[test]
fn claude_is_asked_its_limits_itself() {
    let Some(t) = TestEnv::new("usage-claude") else {
        return;
    };
    // No plan's limits (an API key): Claude says nothing.
    fs::create_dir_all(t.claude_dir()).unwrap();
    let out = t.ok(&["usage"]);
    assert!(!out.contains("claude"), "{out}");
    // A plan's: what Claude answered.
    fs::write(
        t.claude_dir().join("usage.json"),
        r#"{"subscription_type":"max","rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":31.0,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"utilization":95.0,"resets_at":null}}}"#,
    )
    .unwrap();
    // An API key in the environment, which Claude would take over its login.
    let out = t
        .command(&["usage"])
        .env("ANTHROPIC_API_KEY", "sk-test")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("claude  max\n"), "{out}");
    assert!(out.contains("5 hours   69% left"), "{out}");
    assert!(out.contains("Weekly     5% left"), "{out}");
    let said = |file| fs::read_to_string(t.claude_dir().join(file)).unwrap();
    assert_eq!(said("sdk-env"), "ANTHROPIC_API_KEY=unset\n");
    // In its SDK mode, without MCP servers or settings.
    assert_eq!(
        said("sdk-args"),
        "-p\n--input-format\nstream-json\n--output-format\nstream-json\n--verbose\n\
         --strict-mcp-config\n--setting-sources\n\n"
    );
    // Asked as its SDK asks, without the scan of its transcripts.
    assert_eq!(
        said("sdk-input"),
        concat!(
            r#"{"type":"control_request","request_id":"r1","request":{"subtype":"initialize"}}"#,
            "\n",
            r#"{"type":"control_request","request_id":"r2","request":{"subtype":"get_usage","skip_behaviors":true}}"#,
            "\n",
        )
    );
    // Answered, its input ended, for it to exit as it does.
    assert!(t.claude_dir().join("sdk-ended").exists());

    // Its answer without the figures (it couldn't ask its usage service):
    // said, not taken for no plan.
    fs::write(
        t.claude_dir().join("usage.json"),
        r#"{"subscription_type":"max","rate_limits_available":true,"rate_limits":null}"#,
    )
    .unwrap();
    let out = t.run(&["usage"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(
        stdout.contains("claude  can't tell: Claude couldn't ask its usage service"),
        "{stdout}"
    );
}
