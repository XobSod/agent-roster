//! What Claude's account has used of its limits: asked of Claude.ai's usage
//! service as Claude's own `/usage` does, with Claude's login (the token in
//! its `.credentials.json`), as Claude's sessions find it: in the environment
//! they get, where an env file may point `CLAUDE_CONFIG_DIR` elsewhere.
//! Through curl, the token on its stdin rather than its command line, where
//! anyone could read it. The token is never renewed here: renewing replaces
//! the one Claude keeps for that, which could log Claude out.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::agent::{Ask, Question};
use crate::launch::Input;
use crate::usage::{Usage, Window};
use crate::util::{iso_ms, now_ms};

/// The service, and the beta its login's tokens are for.
const URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA: &str = "oauth-2025-04-20";

const EXPIRED: &str = "Claude's login has expired; Claude renews it the next time it runs";

/// Prints Claude's login, where Claude looks for it: nothing without one.
const PRINT_LOGIN: &str = r#"f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"; if [ -f "$f" ]; then cat "$f"; fi"#;

/// Asks the usage service with Claude's login. None without one (Claude on
/// an API key, or never logged in).
pub(super) fn ask(ask: &Ask<'_>) -> Option<Result<Usage>> {
    let argv = ["sh", "-c", PRINT_LOGIN].map(String::from).into();
    let login = match ask(Question::of(argv)) {
        Ok(file) => login(&file)?,
        Err(e) => return Some(Err(e.context("couldn't read Claude's login"))),
    };
    let now = now_ms();
    if login.expires_at <= now {
        return Some(Err(anyhow::anyhow!(EXPIRED)));
    }
    let headers = format!(
        "Authorization: Bearer {}\nanthropic-beta: {BETA}\n",
        login.token
    );
    let argv = [
        "curl",
        "--silent",
        "--show-error",
        "--max-time",
        "8",
        "--header",
        "@-",
        "--write-out",
        "\n%{http_code}",
        URL,
    ];
    let question = Question {
        argv: argv.map(String::from).into(),
        input: Some(Input {
            text: headers.as_bytes(),
            answered: None,
        }),
    };
    let printed = match ask(question) {
        Ok(printed) => printed,
        Err(e) => return Some(Err(e.context("couldn't ask Claude.ai's usage service"))),
    };
    read_answer(&printed, login.plan, now).transpose()
}

struct Login {
    token: String,
    /// In ms.
    expires_at: i64,
    plan: Option<String>,
}

/// Claude's login to Claude.ai, from the file it keeps it in.
fn login(file: &str) -> Option<Login> {
    let file: Value = serde_json::from_str(file).ok()?;
    let login = &file["claudeAiOauth"];
    Some(Login {
        token: login["accessToken"].as_str()?.to_string(),
        expires_at: login["expiresAt"].as_i64().unwrap_or(i64::MAX),
        plan: login["subscriptionType"].as_str().map(String::from),
    })
}

/// The service's answer, as curl printed it with its status on a last line,
/// as of `now`; None if it has neither window.
fn read_answer(printed: &str, plan: Option<String>, now: i64) -> Result<Option<Usage>> {
    let printed = printed.trim_end();
    let (body, status) = printed.rsplit_once('\n').unwrap_or(("", printed));
    match status {
        "200" => {}
        "401" => bail!(EXPIRED),
        "429" => bail!("Claude.ai's usage service is busy; roster asks again later"),
        status => bail!("Claude.ai's usage service answered {status}"),
    }
    let answer: Value = serde_json::from_str(body)
        .context("Claude.ai's usage service answered what roster can't read")?;
    let windows: Vec<Window> = [("five_hour", 300), ("seven_day", 10_080)]
        .into_iter()
        .filter_map(|(key, minutes)| {
            let window = &answer[key];
            Some(Window {
                minutes,
                used: window["utilization"].as_f64()?,
                resets_at: window["resets_at"].as_str().and_then(iso_ms),
            })
        })
        .collect();
    Ok((!windows.is_empty()).then(|| Usage {
        plan,
        windows,
        notes: Vec::new(),
        at: now,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_gives_both_windows() {
        // As the service answered on 2026-10-25 (cut short).
        let printed = r#"{"five_hour":{"utilization":31.0,"resets_at":"2026-10-25T18:20:00.471005+00:00","limit_dollars":null},"seven_day":{"utilization":52.0,"resets_at":"2026-10-25T16:00:00.471026+00:00"},"seven_day_opus":null,"extra_usage":{"is_enabled":false}}
200"#;
        let usage = read_answer(printed, Some("pro".into()), 7)
            .unwrap()
            .unwrap();
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        assert_eq!(usage.at, 7);
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
                    resets_at: Some(1_792_944_000_471)
                },
            ]
        );
        // Neither: nothing to show.
        let printed = "{\"five_hour\":null,\"seven_day\":null}\n200";
        assert_eq!(read_answer(printed, None, 0).unwrap(), None);
    }

    #[test]
    fn a_refusal_says_why() {
        let said = |printed| read_answer(printed, None, 0).unwrap_err().to_string();
        assert_eq!(said("{\"error\":{}}\n401"), EXPIRED);
        assert!(said("{}\n429").contains("busy"));
        assert!(said("<html>\n502").ends_with("answered 502"));
        assert!(said("not json\n200").contains("can't read"));
    }

    #[test]
    fn the_login_is_claudes_and_without_one_nothing_is_asked() {
        // What the login file holds, as the session's environment finds it,
        // and whether curl was asked.
        let file = std::cell::RefCell::new(String::new());
        let curled = std::cell::Cell::new(false);
        let ask = |q: Question<'_>| {
            if q.argv[0] == "sh" {
                return Ok(file.borrow().clone());
            }
            curled.set(true);
            // The token goes on curl's stdin, never its command line.
            assert_eq!(q.argv[0], "curl");
            assert!(q.argv.iter().all(|a| !a.contains("t0k")));
            let input = q.input.unwrap();
            assert!(String::from_utf8_lossy(input.text).contains("Bearer t0k\n"));
            assert!(input.answered.is_none());
            Ok("{\"five_hour\":{\"utilization\":5.0,\"resets_at\":null}}\n200".into())
        };
        // No login: Claude on an API key.
        assert!(super::ask(&ask).is_none());
        // Expired: said, without asking.
        *file.borrow_mut() =
            r#"{"claudeAiOauth":{"accessToken":"t0k","expiresAt":1000,"subscriptionType":"pro"}}"#
                .into();
        let err = super::ask(&ask).unwrap().unwrap_err();
        assert_eq!(err.to_string(), EXPIRED);
        assert!(!curled.get());
        *file.borrow_mut() = r#"{"claudeAiOauth":{"accessToken":"t0k","expiresAt":99999999999999,"subscriptionType":"pro"}}"#.into();
        let usage = super::ask(&ask).unwrap().unwrap();
        assert!(curled.get());
        assert_eq!(usage.windows[0].used, 5.0);
        assert_eq!(usage.windows[0].resets_at, None);
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        // Its login couldn't be read: said.
        let failing = |_: Question<'_>| anyhow::bail!("env file failed");
        let err = super::ask(&failing).unwrap().unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "couldn't read Claude's login: env file failed"
        );
    }

    #[test]
    fn the_login_is_found_where_claude_looks() {
        let dir = tempfile::tempdir().unwrap();
        let printed = |envs: &[(&str, &std::path::Path)]| {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", PRINT_LOGIN])
                .env_remove("CLAUDE_CONFIG_DIR");
            for (var, value) in envs {
                cmd.env(var, value);
            }
            let out = cmd.output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap()
        };
        // None there: nothing, and no error.
        assert_eq!(printed(&[("HOME", dir.path())]), "");
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join(".claude/.credentials.json"), "home").unwrap();
        assert_eq!(printed(&[("HOME", dir.path())]), "home");
        // `CLAUDE_CONFIG_DIR` wins.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(".credentials.json"), "other").unwrap();
        assert_eq!(
            printed(&[("HOME", dir.path()), ("CLAUDE_CONFIG_DIR", &other)]),
            "other"
        );
    }
}
