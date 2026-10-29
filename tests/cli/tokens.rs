//! What the agents spent, read from every conversation each keeps: Claude's
//! transcripts (and its subagents'), codex's rollouts, pi's session files.

use std::fs;
use std::process::Command;

use serde_json::{Value, json};

use crate::common::TestEnv;

/// `ago` before now in UTC, as the agents write it (`date -d`'s words).
fn iso(ago: &str) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("now - {ago}"),
            "+%Y-%m-%dT%H:%M:%S.000Z",
        ])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn write(path: std::path::PathBuf, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(path, text).unwrap();
}

#[test]
fn tokens_are_summed_from_every_agents_conversations() {
    let Some(t) = TestEnv::new("tokens") else {
        return;
    };
    let out = t.ok(&["tokens"]);
    assert_eq!(out, "Nothing spent in the last 30 days.\n");

    // Claude: a reply written twice (counted once), and a subagent's.
    let reply = |id: &str, at: &str, model: &str, output: u64| {
        json!({"type": "assistant", "timestamp": at, "message": {"id": id, "model": model,
            "usage": {"input_tokens": 1000, "cache_read_input_tokens": 1_000_000,
                "cache_creation_input_tokens": 0, "output_tokens": output}}})
    };
    let project = t.claude_dir().join("projects/-repo");
    write(
        project.join("c1.jsonl"),
        &[
            reply("m1", &iso("2 hours"), "claude-opus-5-5", 100),
            reply("m1", &iso("2 hours"), "claude-opus-5-5", 10_000),
        ],
    );
    write(
        project.join("c1/subagents/agent-a.jsonl"),
        &[reply("m2", &iso("1 hour"), "claude-haiku-5-5", 0)],
    );
    // Codex: a call today, and one long ago in the same (resumed) rollout.
    let record = |at: &str, input: u64| {
        json!({"timestamp": at, "type": "token_usage_record", "payload": {"usage": {
            "input_tokens": input, "cached_input_tokens": 0, "output_tokens": 0, "total_tokens": input}}})
    };
    write(
        t.codex_dir().join(
            "sessions/2026/10/25/rollout-2026-10-25T10-00-00-01a17280-b500-7000-8000-000000000001.jsonl",
        ),
        &[
            json!({"timestamp": iso("40 days"), "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
            record(&iso("40 days"), 7_000_000),
            record(&iso("3 hours"), 500_000),
        ],
    );
    // Pi: a model without a price.
    write(
        t.pi_dir()
            .join("sessions/--repo--/2026-10-25T10-00-00-000Z_p1.jsonl"),
        &[
            json!({"type": "message", "id": "a1", "timestamp": iso("5 hours"), "message": {"role": "assistant",
            "provider": "vllm", "model": "qwen3", "usage": {"input": 2000, "output": 30, "cacheRead": 0,
                "cacheWrite": 0, "cost": {"total": 0}}}}),
        ],
    );

    let out = t.ok(&["tokens"]);
    // Claude: 1.011M on Opus ($4×0.001 + $0.20×1 + $20×0.01 = $0.404),
    // 1.001M on Haiku past its long rates' 100k ($0.0005 + $0.05); codex:
    // 500k on Sol past its 272k ($2): $2.4545 in all.
    assert!(
        out.starts_with("In the last 30 days: 2.51M tokens in 3 sessions, $2.45 at API prices"),
        "{out}"
    );
    assert!(
        out.contains("2.03k tokens are of models without a known price"),
        "{out}"
    );
    for row in [
        "claude  1         2.01M",
        "codex   1         500k",
        "pi      1         2.03k   0.1%   unpriced",
        "claude-opus-5-5   claude  1.00k  1.00M       0            10.0k   $0.40",
        "vllm/qwen3        pi      2.00k  0           0            30      unpriced",
    ] {
        assert!(out.contains(row), "{row:?} in\n{out}");
    }

    let json: Value = serde_json::from_str(&t.ok(&["tokens", "--days", "60", "--json"])).unwrap();
    // Sixty days back, codex's old call counts too.
    assert_eq!(json["agents"][0]["agent"], "codex");
    assert_eq!(json["agents"][0]["spent"]["tokens"]["input"], 7_500_000);
    assert_eq!(json["sessions"], 3);
    assert_eq!(json["total"]["unpriced"], 2030);

    // Gone from the agents' files (Claude's after 30 days, say), what was
    // spent stays: roster kept it.
    fs::remove_dir_all(t.claude_dir().join("projects")).unwrap();
    let out = t.ok(&["tokens"]);
    assert!(
        out.starts_with("In the last 30 days: 2.51M tokens in 3 sessions"),
        "{out}"
    );
    // A file that grew is counted again in place of what it was, not beside it.
    let session = t
        .pi_dir()
        .join("sessions/--repo--/2026-10-25T10-00-00-000Z_p1.jsonl");
    let mut text = fs::read_to_string(&session).unwrap();
    text.push_str(&format!(
        "{}\n",
        json!({"type": "message", "id": "a2", "timestamp": iso("1 hour"), "message": {"role": "assistant",
            "provider": "vllm", "model": "qwen3", "usage": {"input": 1000, "output": 0}}})
    ));
    fs::write(&session, text).unwrap();
    let json: Value = serde_json::from_str(&t.ok(&["tokens", "--json"])).unwrap();
    assert_eq!(json["total"]["unpriced"], 3030);
    assert_eq!(json["sessions"], 3);

    // Archived, a rollout moves: it's the same calls, where it is now.
    let rollout = "rollout-2026-10-25T10-00-00-01a17280-b500-7000-8000-000000000001.jsonl";
    fs::create_dir_all(t.codex_dir().join("archived_sessions")).unwrap();
    fs::rename(
        t.codex_dir().join("sessions/2026/10/25").join(rollout),
        t.codex_dir().join("archived_sessions").join(rollout),
    )
    .unwrap();
    let json: Value = serde_json::from_str(&t.ok(&["tokens", "--json"])).unwrap();
    let codex = json["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "codex")
        .unwrap();
    assert_eq!(codex["spent"]["tokens"]["input"], 500_000);
    assert_eq!(json["sessions"], 3);

    // Forked, pi copies the session with its entries as they were: not
    // spent again, until the fork spends.
    let mut fork = format!(
        "{}\n",
        json!({"type": "session", "version": 3, "id": "p2", "timestamp": iso("1 minute"),
            "parentSession": session.display().to_string()})
    );
    fork.push_str(&fs::read_to_string(&session).unwrap());
    fs::write(
        t.pi_dir()
            .join("sessions/--repo--/2026-10-25T18-00-00-000Z_p2.jsonl"),
        fork,
    )
    .unwrap();
    let json: Value = serde_json::from_str(&t.ok(&["tokens", "--json"])).unwrap();
    assert_eq!(json["total"]["unpriced"], 3030);
    assert_eq!(json["sessions"], 3);
}

#[test]
fn a_file_gone_keeps_what_it_spent_though_another_has_its_name() {
    let Some(t) = TestEnv::new("tokens-names") else {
        return;
    };
    let reply = |id: &str, input: u64| {
        json!({"type": "assistant", "timestamp": iso("1 hour"), "message": {"id": id,
            "model": "claude-opus-5-5", "usage": {"input_tokens": input, "output_tokens": 0}}})
    };
    // Two conversations' subagents, under one name.
    let project = t.claude_dir().join("projects/-repo");
    for (conversation, id, input) in [("c1", "m1", 1000), ("c2", "m2", 2000)] {
        write(project.join(format!("{conversation}.jsonl")), &[]);
        write(
            project.join(format!("{conversation}/subagents/agent-a.jsonl")),
            &[reply(id, input)],
        );
    }
    let json: Value = serde_json::from_str(&t.ok(&["tokens", "--json"])).unwrap();
    assert_eq!(json["total"]["tokens"]["input"], 3000);
    // One's gone: it didn't move to the other's.
    fs::remove_file(project.join("c1/subagents/agent-a.jsonl")).unwrap();
    let json: Value = serde_json::from_str(&t.ok(&["tokens", "--json"])).unwrap();
    assert_eq!(json["total"]["tokens"]["input"], 3000);
}
