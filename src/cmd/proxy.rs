//! `roster proxy`: how the CLI Proxy stands, and the accounts it holds with
//! their labels; `roster proxy label` names one.

use crate::*;
use serde_json::json;

/// `roster proxy`.
pub fn proxy(r: &mut Roster, change: Option<ProxyCmd>, json: bool) -> Result<()> {
    match change {
        Some(ProxyCmd::Label {
            provider,
            email,
            label,
        }) => {
            r.set_proxy_label(&provider, &email, label.as_deref())?;
            match label.filter(|l| !l.trim().is_empty()) {
                Some(label) => println!("{provider}'s {email} is labelled {label}"),
                None => println!("{provider}'s {email} has no label"),
            }
            Ok(())
        }
        None => list(r, json),
    }
}

/// Its accounts, as it lists them now (nothing asked of their services), and
/// those labelled that it doesn't list; with account_limits off, or the
/// proxy off, it isn't asked, and the labels alone are. An error if it
/// can't be asked.
fn list(r: &Roster, json: bool) -> Result<()> {
    let url = r.config.cli_proxy().map(String::from);
    let asked = match &url {
        Some(url) if r.config.account_limits() => Some(accounts(url, &r.paths.secrets_file())),
        _ => None,
    };
    let listed: &[proxy::Account] = match &asked {
        Some(Ok(accounts)) => accounts,
        _ => &[],
    };
    let rows = proxy::rows(listed, &r.config.cli_proxy.labels);
    let (state, why) = match (&url, &asked) {
        _ if !r.config.account_limits() => ("not asked", Some(usage::OFF.to_string())),
        (None, _) => ("off", None),
        (Some(_), Some(Ok(_))) => ("connected", None),
        (Some(_), Some(Err(e))) => ("can't be asked", Some(format!("{e:#}"))),
        (Some(_), None) => ("not asked", None),
    };
    if json {
        let accounts: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "provider": row.provider, "email": row.email, "label": row.label,
                    "listed": row.account.is_some(),
                    "id": row.account.map(|a| &a.id),
                    "disabled": row.account.map(|a| a.disabled),
                    "sign_in": row.account.map(|a| a.sign_in),
                })
            })
            .collect();
        let said = json!({
            "url": r.config.cli_proxy.url, "enabled": r.config.cli_proxy.enabled.unwrap_or(false),
            "state": state, "error": why, "accounts": accounts,
        });
        println!("{}", serde_json::to_string_pretty(&said)?);
    } else {
        let at = (r.config.cli_proxy.url.as_deref())
            .filter(|u| !u.trim().is_empty())
            .map(|u| format!(" {}", u.trim()))
            .unwrap_or_default();
        match &why {
            Some(why) => println!("CLI Proxy{at}: {state}: {why}"),
            None if state == "connected" => {
                let n = listed.len();
                let accounts = if n == 1 { "account" } else { "accounts" };
                println!("CLI Proxy{at}: connected, {n} {accounts}");
            }
            None => println!("CLI Proxy{at}: {state} (cli_proxy.enabled, cli_proxy.url)"),
        }
        if !rows.is_empty() {
            println!();
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|row| {
                    let state = match row.account {
                        Some(a) if a.disabled => "disabled",
                        Some(a) if a.sign_in => "needs signing in again",
                        Some(_) => "",
                        None => "not listed",
                    };
                    vec![
                        row.provider.clone(),
                        row.email.clone().unwrap_or_else(|| "-".into()),
                        row.label.clone().unwrap_or_else(|| "-".into()),
                        state.to_string(),
                    ]
                })
                .collect();
            print::print_table(&["PROVIDER", "EMAIL", "LABEL", "STATE"], &table);
        }
    }
    if let (Some(Err(e)), Some(url)) = (asked, url) {
        return Err(e.context(format!("the CLI Proxy at {url} can't be asked")));
    }
    Ok(())
}

/// The accounts the CLI Proxy at `url` lists, its key read from `secrets`.
fn accounts(url: &str, secrets: &std::path::Path) -> Result<Vec<proxy::Account>> {
    let key = secrets::proxy_key(secrets)?
        .context("its key isn't set (roster config set cli_proxy.key)")?;
    proxy::Proxy::new(url, key).accounts()
}
