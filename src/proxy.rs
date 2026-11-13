//! A CLI Proxy (CLIProxyAPI): a proxy the agents talk to in place of their
//! services, holding several accounts and spreading requests over them.
//! Roster only reads it, through its management API (`/v8/management`, with
//! its management key, `secrets`): the accounts it holds (`/credentials`),
//! and each one's usage, asked of the account's own service through it
//! (`/requests/api-call`), which puts the account's login where the request
//! says `$TOKEN$`, so roster never sees a login. Only the fixed requests
//! below are made through it, all reads; nothing of the proxy is changed.
//!
//! The usage services are what the agents' own `/usage` asks, undocumented:
//! an answer roster doesn't know is said as that, never misread.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ureq::tls::TlsConfig;

use crate::usage::{Usage, Window};
use crate::util::now_ms;

/// Where its management API is, under its URL.
const MANAGEMENT: &str = "/v8/management";
/// A Claude account's usage, as Claude Code's `/usage` asks it.
const CLAUDE_USAGE: &str = "https://api.anthropic.com/api/oauth/usage";
/// A Claude account's plan, which its usage doesn't say.
const CLAUDE_PROFILE: &str = "https://api.anthropic.com/api/oauth/profile";
/// What Claude's service wants told, asked with an account's login.
const CLAUDE_BETA: (&str, &str) = ("anthropic-beta", "oauth-2025-04-20");
/// A Codex account's usage, as Codex asks it.
const CODEX_USAGE: &str = "https://chatgpt.com/backend-api/wham/usage";

/// How long a request may take, the service's answer through the proxy
/// included, and connecting to the proxy.
const TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How much of an answer is read at most.
const ANSWER_LIMIT: u64 = 4 << 20;

/// An account the proxy holds, signed in to (an API key it holds has no
/// plan's limits, and isn't one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The proxy's name for its service: `claude`, `codex`, `gemini`.
    pub provider: String,
    /// The proxy's id for it (`auth_index`), to ask through it.
    pub id: String,
    #[serde(default)]
    pub email: Option<String>,
    /// Not used by the proxy, as set there.
    #[serde(default)]
    pub disabled: bool,
    /// The proxy says it needs signing in again.
    #[serde(default)]
    pub sign_in: bool,
}

impl Account {
    /// Whether roster can ask what it has used: Claude's and Codex's.
    pub fn asked(&self) -> bool {
        matches!(self.provider.as_str(), "claude" | "codex")
    }

    /// Whether it's the same account as `other`, whatever changed of it.
    pub fn same(&self, other: &Account) -> bool {
        self.provider == other.provider && self.id == other.id
    }
}

/// An account to show where accounts are labelled: one the proxy listed, or
/// one labelled that it doesn't list now.
pub struct Row<'a> {
    pub provider: String,
    pub email: Option<String>,
    pub label: Option<String>,
    /// As the proxy listed it; None if it doesn't now.
    pub account: Option<&'a Account>,
}

/// The `accounts` the proxy listed, each with its label if it has one, then
/// those of `labels` it didn't list; by service, then by name.
pub fn rows<'a>(
    accounts: impl IntoIterator<Item = &'a Account>,
    labels: &[crate::config::Label],
) -> Vec<Row<'a>> {
    let label = |provider: &str, email: Option<&str>| {
        let email = email?;
        (labels.iter())
            .find(|l| l.of(provider, email))
            .map(|l| l.label.clone())
    };
    let mut rows: Vec<Row> = accounts
        .into_iter()
        .map(|a| Row {
            provider: a.provider.clone(),
            email: a.email.clone(),
            label: label(&a.provider, a.email.as_deref()),
            account: Some(a),
        })
        .collect();
    for l in labels {
        let listed = |r: &Row| r.email.as_deref().is_some_and(|e| l.of(&r.provider, e));
        if !rows.iter().any(listed) {
            rows.push(Row {
                provider: l.provider.to_lowercase(),
                email: Some(l.email.clone()),
                label: Some(l.label.clone()),
                account: None,
            });
        }
    }
    rows.sort_by_cached_key(|r| {
        let (rank, other) = crate::usage::rank(&r.provider);
        let name = (r.label.as_deref())
            .or(r.email.as_deref())
            .or(r.account.map(|a| a.id.as_str()))
            .unwrap_or_default()
            .to_lowercase();
        (rank, other.to_string(), name)
    });
    rows
}

/// The proxy at a URL, asked with its management key.
pub struct Proxy {
    url: String,
    key: String,
    agent: ureq::Agent,
}

impl Proxy {
    pub fn new(url: &str, key: String) -> Proxy {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            // Its key goes to it alone: not through a proxy the environment
            // names, nor where a redirect points.
            .proxy(None)
            .max_redirects(0)
            // On this machine, or a container's port: plain http too.
            .https_only(false)
            .http_status_as_error(false)
            .user_agent(format!("roster/{}", crate::update::Release::running()))
            .tls_config(
                TlsConfig::builder()
                    .root_certs(crate::update::roots())
                    .build(),
            )
            .build();
        Proxy {
            url: url.trim().trim_end_matches('/').to_string(),
            key,
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// The accounts it holds, as it lists them.
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let sent = (self.agent)
            .get(format!("{}{MANAGEMENT}/credentials", self.url))
            .header("Authorization", format!("Bearer {}", self.key))
            .call();
        read_accounts(&self.answer(sent)?)
    }

    /// What `account` has used of its limits; None if roster can't ask its
    /// service, or it has no plan's limits.
    pub fn usage(&self, account: &Account) -> Result<Option<Usage>> {
        let at = now_ms();
        match account.provider.as_str() {
            "claude" => self
                .upstream(account, CLAUDE_USAGE, &[CLAUDE_BETA])
                .and_then(|body| read_claude_usage(&body, at))
                .context("Claude's usage service"),
            "codex" => self
                .upstream(account, CODEX_USAGE, &[])
                .and_then(|body| read_codex_usage(&body, at))
                .context("Codex's usage service"),
            _ => Ok(None),
        }
    }

    /// A Claude account's plan (`max 5x`), which its usage doesn't say.
    pub fn claude_plan(&self, account: &Account) -> Result<Option<String>> {
        let body = self.upstream(account, CLAUDE_PROFILE, &[CLAUDE_BETA])?;
        Ok(read_claude_plan(&body))
    }

    /// The service's answer to a GET of `url` with `account`'s login, made
    /// by the proxy.
    fn upstream(&self, account: &Account, url: &str, headers: &[(&str, &str)]) -> Result<Value> {
        let mut header = serde_json::Map::new();
        header.insert("Authorization".into(), "Bearer $TOKEN$".into());
        for (name, value) in headers {
            header.insert((*name).into(), (*value).into());
        }
        let request = json!({
            "auth_index": account.id, "method": "GET", "url": url, "header": header,
        });
        let sent = (self.agent)
            .post(format!("{}{MANAGEMENT}/requests/api-call", self.url))
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .send(request.to_string());
        read_api_call(&self.answer(sent)?)
    }

    /// What the proxy answered, as JSON; why not, said as the proxy's.
    fn answer(
        &self,
        sent: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Value> {
        let mut answer = sent.map_err(|e| anyhow!("can't reach it: {e}"))?;
        let status = answer.status().as_u16();
        let text = (answer.body_mut().with_config())
            .limit(ANSWER_LIMIT)
            .read_to_string()
            .map_err(|e| anyhow!("can't read its answer: {e}"));
        match status {
            200..=299 => {}
            401 | 403 => bail!("it refused the key ({status})"),
            404 => bail!(
                "it has no management API at {} ({status}): is its remote management on?",
                self.url
            ),
            _ => bail!("it answered {status}"),
        }
        serde_json::from_str(&text?).context("its answer isn't JSON")
    }
}

/// The accounts `/credentials` lists, signed-in ones (not API keys). An
/// answer roster doesn't know is refused, never read as fewer accounts, so a
/// proxy that answered before keeps what it said: no such list; an entry
/// that isn't an object, or whose fields are of other kinds than roster
/// knows (`disabled: "true"`); a list with entries but none it can read.
/// What's refused says where (which entry, which field), never what's
/// there, which may be an email. An entry with no id or service (none
/// roster could ask through it) is passed over.
pub fn read_accounts(answer: &Value) -> Result<Vec<Account>> {
    let unknown = |why: &str| anyhow!("its list of accounts isn't one roster knows ({why})");
    let listed = match answer {
        Value::Array(items) => items,
        Value::Object(fields) => ["credentials", "items", "files"]
            .iter()
            .find_map(|key| fields.get(*key)?.as_array())
            .or_else(|| {
                (fields.values().filter_map(Value::as_array))
                    .find(|items| items.iter().any(|i| i.get("auth_index").is_some()))
            })
            .ok_or_else(|| unknown("no list in its answer"))?,
        _ => return Err(unknown("no list in its answer")),
    };
    let mut accounts = Vec::new();
    let mut read = false;
    for (i, item) in listed.iter().enumerate() {
        let entry = read_entry(item).map_err(|why| unknown(&format!("entry {}: {why}", i + 1)))?;
        match entry {
            Entry::Account(account) => accounts.push(account),
            Entry::ApiKey => {}
            Entry::Other => continue,
        }
        read = true;
    }
    if !listed.is_empty() && !read {
        let entries = match listed.len() {
            1 => "its entry doesn't have",
            _ => "none of its entries has",
        };
        return Err(unknown(&format!("{entries} both an id and a service")));
    }
    Ok(accounts)
}

/// An entry of the proxy's list, as roster reads it.
enum Entry {
    Account(Account),
    /// An API key: no plan's limits.
    ApiKey,
    /// Without an id or a service: nothing roster could ask through it.
    Other,
}

/// An entry of the proxy's list; why not, if it isn't an object, or a field
/// roster reads is of another kind than it knows: by the field's name, not
/// what's in it.
fn read_entry(item: &Value) -> std::result::Result<Entry, String> {
    let Value::Object(fields) = item else {
        return Err("it isn't an object".into());
    };
    // Missing or null: none; text, trimmed (empty: none); anything else: why.
    let text = |key: &str| -> std::result::Result<Option<String>, String> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                Ok(Some(s.trim()).filter(|s| !s.is_empty()).map(String::from))
            }
            Some(_) => Err(format!("{key} isn't text")),
        }
    };
    let flag = |key: &str| -> std::result::Result<bool, String> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(format!("{key} isn't true or false")),
        }
    };
    if text("account_type")?.is_some_and(|kind| kind != "oauth") {
        return Ok(Entry::ApiKey);
    }
    let id = match fields.get("auth_index") {
        Some(Value::Number(n)) => Some(n.to_string()),
        None | Some(Value::Null | Value::String(_)) => text("auth_index")?,
        Some(_) => return Err("auth_index isn't text or a number".into()),
    };
    let provider = match text("provider")? {
        Some(provider) => Some(provider),
        None => text("type")?,
    };
    let (email, disabled, sign_in) = (text("email")?, flag("disabled")?, flag("requires_sign_in")?);
    let (Some(id), Some(provider)) = (id, provider) else {
        return Ok(Entry::Other);
    };
    Ok(Entry::Account(Account {
        provider: provider.to_lowercase(),
        id,
        email,
        disabled,
        sign_in,
    }))
}

/// What the service answered through `/requests/api-call`: its body, as
/// JSON, if it said yes.
fn read_api_call(answer: &Value) -> Result<Value> {
    let status = answer["status_code"]
        .as_u64()
        .context("its answer isn't one roster knows")?;
    match status {
        200..=299 => {}
        401 | 403 => bail!("it refused the account's login ({status})"),
        429 => bail!("it asked to wait ({status})"),
        _ => bail!("it answered {status}"),
    }
    match &answer["body"] {
        Value::String(body) => serde_json::from_str(body).context("its answer isn't JSON"),
        Value::Object(_) => Ok(answer["body"].clone()),
        _ => bail!("its answer isn't JSON"),
    }
}

/// A Claude account's usage, as its service answers: the windows Claude Code
/// shows (`agent::claude::limits::windows`); None without a plan's limits.
/// A window is null, or an object whose figure is null (none) or a number:
/// anything else is an answer roster doesn't know, not one without limits.
fn read_claude_usage(body: &Value, at: i64) -> Result<Option<Usage>> {
    if body.get("five_hour").is_none() && body.get("seven_day").is_none() {
        bail!("its answer isn't one roster knows");
    }
    for key in ["five_hour", "seven_day"] {
        let known = match &body[key] {
            Value::Null => true,
            Value::Object(w) => w
                .get("utilization")
                .is_none_or(|u| u.is_null() || u.is_number()),
            _ => false,
        };
        if !known {
            bail!("its {key} isn't one roster knows");
        }
    }
    let windows = crate::agent::claude::limits::windows(body);
    Ok((!windows.is_empty()).then(|| Usage {
        plan: None,
        windows,
        notes: Vec::new(),
        at,
    }))
}

/// A Codex account's usage, as its service answers: its plan, and its two
/// windows, each as long as it says; None without a plan's limits. A window
/// with none of its figures isn't there; one with only some of them is an
/// answer roster doesn't know, not one without limits.
fn read_codex_usage(body: &Value, at: i64) -> Result<Option<Usage>> {
    let Some(limits) = body
        .get("rate_limit")
        .filter(|l| l.is_null() || l.is_object())
    else {
        bail!("its answer isn't one roster knows");
    };
    let mut windows = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let w = &limits[key];
        let figures = ["limit_window_seconds", "used_percent"];
        match w {
            Value::Null => continue,
            Value::Object(fields)
                if figures
                    .iter()
                    .all(|f| fields.get(*f).is_none_or(Value::is_null)) =>
            {
                continue;
            }
            Value::Object(_) => {}
            _ => bail!("its {key} isn't one roster knows"),
        }
        let (Some(seconds), Some(used)) = (
            w["limit_window_seconds"].as_u64(),
            w["used_percent"].as_f64(),
        ) else {
            bail!("its {key} isn't one roster knows");
        };
        windows.push(Window {
            minutes: seconds.saturating_add(30) / 60,
            used,
            resets_at: w["reset_at"].as_i64().and_then(|s| s.checked_mul(1000)),
        });
    }
    if windows.is_empty() {
        return Ok(None);
    }
    windows.sort_by_key(|w| w.minutes);
    Ok(Some(Usage {
        plan: body["plan_type"].as_str().map(String::from),
        windows,
        notes: Vec::new(),
        at,
    }))
}

/// A Claude account's plan, from its profile: its organization's kind
/// (`claude_max`), and how many times a plan's use it has where its tier
/// says (`default_claude_max_5x`): `max 5x`. None if not said.
fn read_claude_plan(body: &Value) -> Option<String> {
    let organization = &body["organization"];
    let kind = organization["organization_type"].as_str()?.trim();
    let mut plan = kind
        .strip_prefix("claude_")
        .unwrap_or(kind)
        .replace('_', " ");
    if plan.is_empty() {
        return None;
    }
    let times = organization["rate_limit_tier"]
        .as_str()
        .and_then(|tier| tier.rsplit('_').next())
        .filter(|last| {
            last.strip_suffix('x')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
    if let Some(times) = times {
        plan.push(' ');
        plan.push_str(times);
    }
    Some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Answers in the shape the proxy and the services gave (CLIProxyAPI
    // 8.0.69), every value made up, most fields left out.

    #[test]
    fn the_accounts_listed_are_those_signed_in() {
        let listed = json!({"credentials": [
            {"auth_index": "1a2b", "provider": "claude", "type": "claude", "account_type": "oauth",
             "email": "one@example.com", "disabled": false, "requires_sign_in": false,
             "status": "active", "unavailable": false, "priority": 0, "label": "", "quota": {}},
            {"auth_index": "3c4d", "provider": "Codex", "account_type": "oauth",
             "email": "two@example.com", "disabled": true, "requires_sign_in": true},
            {"auth_index": "5e6f", "provider": "claude", "account_type": "api_key"},
            {"auth_index": "7a8b", "provider": "gemini", "email": ""},
            {"provider": "claude", "email": "no-index@example.com"},
        ]});
        let accounts = read_accounts(&listed).unwrap();
        assert_eq!(
            accounts,
            [
                Account {
                    provider: "claude".into(),
                    id: "1a2b".into(),
                    email: Some("one@example.com".into()),
                    disabled: false,
                    sign_in: false,
                },
                Account {
                    provider: "codex".into(),
                    id: "3c4d".into(),
                    email: Some("two@example.com".into()),
                    disabled: true,
                    sign_in: true,
                },
                Account {
                    provider: "gemini".into(),
                    id: "7a8b".into(),
                    email: None,
                    disabled: false,
                    sign_in: false,
                },
            ]
        );
        assert!(!accounts[2].asked());
        // A bare list too; something else, refused.
        let bare = json!([{"auth_index": 9, "provider": "codex"}]);
        assert_eq!(read_accounts(&bare).unwrap()[0].id, "9");
        assert_eq!(read_accounts(&json!({"credentials": []})).unwrap(), []);
        assert!(read_accounts(&json!({"error": "nope"})).is_err());
        // API keys alone: none signed in, said so. Entries of a kind roster
        // doesn't know, alone or among others: refused, never read as
        // fewer accounts; nothing readable at all, likewise.
        let keys = json!([{"auth_index": "k", "provider": "claude", "account_type": "api_key"}]);
        assert_eq!(read_accounts(&keys).unwrap(), []);
        let good = json!({"auth_index": "1a", "provider": "claude"});
        for listed in [
            json!([false]),
            json!(["unavailable"]),
            json!([null]),
            json!([[]]),
            json!([good, 7]),
            json!([{"auth_index": "1a", "provider": []}]),
            json!([{"auth_index": "1a", "provider": "claude", "disabled": "true"}]),
            json!([{"auth_index": "1a", "provider": "claude", "email": 7}]),
            json!([{"auth_index": true, "provider": "claude"}]),
            json!([{"auth_index": "1a", "provider": "claude", "account_type": 1}]),
            json!([{"provider": "claude", "email": "no-index@example.com"}]),
        ] {
            assert!(
                read_accounts(&json!({"credentials": listed})).is_err(),
                "{listed}"
            );
        }
        // Where, never what: the entry and the field, not its value.
        let said = |listed: Value| {
            let e = read_accounts(&json!({"credentials": listed})).unwrap_err();
            e.to_string()
        };
        let secret =
            json!({"auth_index": "2b", "provider": "claude", "email": {"a": "secret@example.com"}});
        let e = said(json!([good, secret]));
        assert!(
            e.ends_with("(entry 2: email isn't text)") && !e.contains("secret"),
            "{e}"
        );
        assert!(said(json!([good, 7])).ends_with("(entry 2: it isn't an object)"));
        let e = said(json!([{"auth_index": "1a", "provider": "claude", "disabled": "true"}]));
        assert!(
            e.ends_with("(entry 1: disabled isn't true or false)"),
            "{e}"
        );
        let e = said(json!([{"auth_index": true}]));
        assert!(
            e.ends_with("(entry 1: auth_index isn't text or a number)"),
            "{e}"
        );
        let e = said(json!([{"provider": "claude"}, {"type": "codex"}]));
        assert!(
            e.ends_with("(none of its entries has both an id and a service)"),
            "{e}"
        );
        for alone in [
            json!({"provider": "claude"}),
            json!({"auth_index": "1a"}),
            json!({"email": "one@example.com"}),
        ] {
            let e = said(json!([alone]));
            assert!(
                e.ends_with("(its entry doesn't have both an id and a service)"),
                "{e}"
            );
        }
        let e = read_accounts(&json!({"error": "nope"}))
            .unwrap_err()
            .to_string();
        assert!(e.ends_with("(no list in its answer)"), "{e}");
    }

    #[test]
    fn a_services_answer_is_its_body_if_it_said_yes() {
        let answer = |status: u64, body: &str| json!({"status_code": status, "header": {"Content-Type": ["application/json"]}, "body": body});
        assert_eq!(
            read_api_call(&answer(200, r#"{"a": 1}"#)).unwrap(),
            json!({"a": 1})
        );
        let refused = read_api_call(&answer(401, r#"{"error": "expired"}"#)).unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("refused the account's login (401)")
        );
        assert!(
            read_api_call(&answer(429, ""))
                .unwrap_err()
                .to_string()
                .contains("wait")
        );
        assert!(read_api_call(&answer(200, "<html>")).is_err());
        assert!(read_api_call(&json!({"error": "unknown auth_index"})).is_err());
    }

    #[test]
    fn a_claude_accounts_usage_and_plan() {
        let body = json!({
            "five_hour": {"utilization": 55.0, "resets_at": "2026-11-13T17:10:00.1+00:00"},
            "seven_day": {"utilization": 37.0, "resets_at": "2026-11-18T09:00:00+00:00"},
            "seven_day_opus": null,
            "extra_usage": {"is_enabled": false},
        });
        let usage = read_claude_usage(&body, 7).unwrap().unwrap();
        assert_eq!((usage.plan.as_deref(), usage.at), (None, 7));
        assert_eq!(
            usage
                .windows
                .iter()
                .map(|w| (w.minutes, w.used))
                .collect::<Vec<_>>(),
            [(300, 55.0), (10_080, 37.0)]
        );
        assert_eq!(usage.windows[0].resets_at, Some(1_794_589_800_100));
        // No figures: no plan's limits. Nothing it knows: not misread.
        let none = json!({"five_hour": null, "seven_day": null});
        assert_eq!(read_claude_usage(&none, 0).unwrap(), None);
        assert!(read_claude_usage(&json!({"limits": []}), 0).is_err());
        // A window, or its figure, of a shape it doesn't know: refused, not
        // taken for none.
        for not_one in [
            json!("unavailable"),
            json!([]),
            json!(7),
            json!({"utilization": "55"}),
        ] {
            let body = json!({"five_hour": not_one, "seven_day": null});
            assert!(read_claude_usage(&body, 0).is_err(), "{body}");
        }

        let plan = |kind: &str, tier: &str| {
            read_claude_plan(&json!({
                "account": {"email": "one@example.com", "has_claude_max": true},
                "organization": {"organization_type": kind, "rate_limit_tier": tier,
                    "name": "Made-up Org", "uuid": "00000000-0000-0000-0000-000000000000"},
            }))
        };
        assert_eq!(
            plan("claude_max", "default_claude_max_5x").as_deref(),
            Some("max 5x")
        );
        assert_eq!(
            plan("claude_max", "default_claude_max_20x").as_deref(),
            Some("max 20x")
        );
        assert_eq!(
            plan("claude_pro", "default_claude_ai").as_deref(),
            Some("pro")
        );
        assert_eq!(plan("claude_team", "").as_deref(), Some("team"));
        assert_eq!(read_claude_plan(&json!({"organization": {}})), None);
    }

    #[test]
    fn a_codex_accounts_usage() {
        let body = json!({
            "plan_type": "plus",
            "rate_limit": {"allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 7, "limit_window_seconds": 18_000, "reset_after_seconds": 9000, "reset_at": 1_794_606_630},
                "secondary_window": {"used_percent": 41, "limit_window_seconds": 604_800, "reset_at": 1_795_000_000}},
            "credits": {"has_credits": false, "unlimited": false, "balance": "0"},
            "additional_rate_limits": null,
        });
        let usage = read_codex_usage(&body, 3).unwrap().unwrap();
        assert_eq!(usage.plan.as_deref(), Some("plus"));
        assert_eq!(
            usage
                .windows
                .iter()
                .map(|w| (w.minutes, w.used, w.resets_at))
                .collect::<Vec<_>>(),
            [
                (300, 7.0, Some(1_794_606_630_000)),
                (10_080, 41.0, Some(1_795_000_000_000))
            ]
        );
        assert!(usage.notes.is_empty());
        assert_eq!(
            read_codex_usage(&json!({"plan_type": "free", "rate_limit": null}), 0).unwrap(),
            None
        );
        assert!(read_codex_usage(&json!({"detail": "nope"}), 0).is_err());
        // One window alone; one there without its figures, refused.
        let one = json!({"rate_limit": {"primary_window": {"used_percent": 7, "limit_window_seconds": 18_000},
            "secondary_window": null}});
        assert_eq!(read_codex_usage(&one, 0).unwrap().unwrap().windows.len(), 1);
        let empty = json!({"rate_limit": {"primary_window": {"used_percent": 7, "limit_window_seconds": 18_000},
            "secondary_window": {}}});
        assert_eq!(
            read_codex_usage(&empty, 0).unwrap().unwrap().windows.len(),
            1
        );
        let malformed = json!({"rate_limit": {"primary_window": {"used_percent": "7"}}});
        let e = read_codex_usage(&malformed, 0).unwrap_err().to_string();
        assert!(e.contains("primary_window"), "{e}");
        // Not limits, or not a window, at all: refused, not taken for none.
        for not_limits in [json!("unavailable"), json!([]), json!(true)] {
            let body = json!({"plan_type": "plus", "rate_limit": not_limits});
            assert!(read_codex_usage(&body, 0).is_err(), "{body}");
        }
        for not_one in [
            json!("temporarily unavailable"),
            json!([]),
            json!(true),
            json!(7),
        ] {
            let body = json!({"rate_limit": {"primary_window": not_one, "secondary_window": null}});
            assert!(read_codex_usage(&body, 0).is_err(), "{body}");
        }
    }

    #[test]
    fn its_key_goes_to_it_alone() {
        // A server that answers once, keeping what it was sent.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let serve = |answer: &'static str| {
            let listener = listener.try_clone().unwrap();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let (mut stream, _) = listener.accept().unwrap();
                let mut sent = Vec::new();
                let mut buf = [0; 4096];
                while !String::from_utf8_lossy(&sent).contains("\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    sent.extend_from_slice(&buf[..n]);
                }
                stream.write_all(answer.as_bytes()).unwrap();
                String::from_utf8_lossy(&sent).into_owned()
            })
        };
        let proxy = Proxy::new(&format!("http://127.0.0.1:{port}/"), "made-up-key".into());
        let body = r#"[{"auth_index":"1a","provider":"claude","email":"one@example.com"}]"#;
        let ok = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let server = serve(ok);
        assert_eq!(proxy.accounts().unwrap()[0].id, "1a");
        let sent = server.join().unwrap();
        assert!(
            sent.starts_with("GET /v8/management/credentials "),
            "{sent}"
        );
        assert!(sent.contains("Bearer made-up-key"), "{sent}");
        // Refused, said so; sent elsewhere, not followed.
        let server =
            serve("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let e = proxy.accounts().unwrap_err().to_string();
        assert!(e.contains("refused the key (401)"), "{e}");
        server.join().unwrap();
        let server = serve(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        assert!(proxy.accounts().unwrap_err().to_string().contains("302"));
        server.join().unwrap();
    }
}
