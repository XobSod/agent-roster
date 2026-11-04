//! Asking GitHub for roster's releases and downloading what one has: without
//! signing in, as anyone may, through the proxy the environment names
//! (`HTTPS_PROXY`, `NO_PROXY`), trusting the system's certificates.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use ureq::tls::{Certificate, RootCerts, TlsConfig};

use super::Release;

/// roster's repository in GitHub's API.
const API: &str = "https://api.github.com/repos/XobSod/agent-roster";

/// Stands in for `API`, for tests: a server of their own.
const API_VAR: &str = "ROSTER_UPDATE_URL";

/// How long asking for the releases may take, and a download.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most a download may be: an archive is a few MB.
const DOWNLOAD_LIMIT: u64 = 256 << 20;

/// A published release: neither a draft nor a pre-release.
#[derive(Clone, Debug)]
pub struct Published {
    pub version: Release,
    /// When it was published, as `2026-11-01`.
    pub date: String,
    /// Its notes, in Markdown.
    pub notes: String,
    pub assets: Vec<Asset>,
}

/// A file a release has, by its name, and where to download it.
#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// The published releases, newest first.
pub fn ask() -> Result<Vec<Published>> {
    let base = std::env::var(API_VAR).unwrap_or_else(|_| API.to_string());
    // Most recently made first; 30 reach back well past any roster still
    // running.
    let url = format!("{}/releases?per_page=30", base.trim_end_matches('/'));
    let text = agent(ASK_TIMEOUT)
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|mut answer| answer.body_mut().read_to_string())
        .map_err(|e| match e {
            ureq::Error::StatusCode(403 | 429) => {
                anyhow!("GitHub's limit for asking without signing in is reached: try again later")
            }
            e => e.into(),
        })
        .context("can't ask GitHub for roster's releases")?;
    published(&text, &Release::running())
}

/// The published releases in GitHub's answer, newest first: drafts,
/// pre-releases, and tags that aren't a release's left out. A tag that's a
/// pre-release's counts as one whatever GitHub says, unless `running` is a
/// pre-release itself.
fn published(json: &str, running: &Release) -> Result<Vec<Published>> {
    let releases: Vec<ApiRelease> =
        serde_json::from_str(json).context("GitHub's list of releases can't be read")?;
    let mut published: Vec<Published> = (releases.into_iter())
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = Release::parse(&r.tag_name)?;
            if version.is_pre() && !running.is_pre() {
                return None;
            }
            Some(Published {
                version,
                date: (r.published_at.as_deref().unwrap_or_default())
                    .chars()
                    .take(10)
                    .collect(),
                notes: r.body.unwrap_or_default().replace("\r\n", "\n"),
                assets: (r.assets.into_iter())
                    .map(|a| Asset {
                        name: a.name,
                        url: a.browser_download_url,
                    })
                    .collect(),
            })
        })
        .collect();
    published.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(published)
}

/// What's at `url`, all of it.
pub fn download(url: &str) -> Result<Vec<u8>> {
    agent(DOWNLOAD_TIMEOUT)
        .get(url)
        .call()
        .and_then(|mut answer| {
            (answer.body_mut().with_config())
                .limit(DOWNLOAD_LIMIT)
                .read_to_vec()
        })
        .with_context(|| format!("can't download {url}"))
}

fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        // An asset at an http URL, or a redirect to one, refused; a test's
        // server of its own is plain http.
        .https_only(std::env::var_os(API_VAR).is_none())
        // GitHub's API refuses a request without one. The release alone:
        // nothing of a checkout's commit leaves the machine.
        .user_agent(format!("roster/{}", Release::running()))
        .tls_config(TlsConfig::builder().root_certs(roots()).build())
        .build();
    ureq::Agent::new_with_config(config)
}

/// The system's certificates (`SSL_CERT_FILE`, `SSL_CERT_DIR`, else where
/// the distribution keeps them), which a company's proxy has its own among;
/// where there are none, Mozilla's, built in.
fn roots() -> RootCerts {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<Certificate<'static>> = (found.certs.iter())
        .map(|c| Certificate::from_der(c.as_ref()).to_owned())
        .collect();
    if certs.is_empty() {
        RootCerts::WebPki
    } else {
        RootCerts::new_with_certs(&certs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_published_releases_count_newest_first() {
        // As GitHub answers: the most recently made first, not by version.
        let json = r#"[
            {"tag_name": "v0.3.0-rc.1", "draft": false, "prerelease": true,
             "published_at": "2026-12-01T09:00:00Z", "body": "rc", "assets": []},
            {"tag_name": "v0.2.1", "draft": false, "prerelease": false,
             "published_at": "2026-11-20T10:00:00Z", "body": "Fixes.\r\n\r\n- one",
             "assets": [{"name": "agent-roster-v0.2.1-linux-x86_64.tar.gz",
                         "browser_download_url": "https://example.invalid/a.tar.gz"}]},
            {"tag_name": "v0.4.0", "draft": true, "prerelease": false,
             "published_at": null, "body": "draft", "assets": []},
            {"tag_name": "v0.1.0", "draft": false, "prerelease": false,
             "published_at": "2026-11-01T10:00:00Z", "body": null, "assets": []},
            {"tag_name": "v0.2.0", "draft": false, "prerelease": false,
             "published_at": "2026-11-10T10:00:00Z", "body": "Second.", "assets": [],
             "author": {"login": "someone"}},
            {"tag_name": "nightly", "draft": false, "prerelease": false,
             "published_at": "2026-11-11T00:00:00Z", "body": "", "assets": []},
            {"tag_name": "v0.3.0-rc.2", "draft": false, "prerelease": false,
             "published_at": "2026-12-02T09:00:00Z", "body": "rc, unflagged", "assets": []}
        ]"#;
        let found = published(json, &Release::parse("0.2.0").unwrap()).unwrap();
        let versions: Vec<String> = found.iter().map(|p| p.version.to_string()).collect();
        assert_eq!(versions, ["0.2.1", "0.2.0", "0.1.0"]);
        assert_eq!(found[0].date, "2026-11-20");
        assert_eq!(found[0].notes, "Fixes.\n\n- one");
        assert_eq!(found[0].assets[0].url, "https://example.invalid/a.tar.gz");
        assert_eq!(found[2].notes, "");

        // Those newer than a build past 0.2.0, and what's kept of them.
        let running = Release::parse("0.2.0+3.g7056428").unwrap();
        let known = super::super::Known::learned(&found, &running, 42);
        assert_eq!(known.checked, 42);
        assert_eq!(known.newest.as_deref(), Some("0.2.1"));
        let kept: Vec<&str> = known.releases.iter().map(|n| n.version.as_str()).collect();
        assert_eq!(kept, ["0.2.1", "0.2.0"]);
        assert_eq!(known.newer(&running), Release::parse("0.2.1"));
        assert_eq!(known.newer_notes(&running).len(), 1);

        // A pre-release's own: taken by its tag, not by GitHub's flag.
        let pre = Release::parse("0.3.0-rc.1").unwrap();
        let found = published(json, &pre).unwrap();
        let versions: Vec<String> = found.iter().map(|p| p.version.to_string()).collect();
        assert_eq!(versions, ["0.3.0-rc.2", "0.2.1", "0.2.0", "0.1.0"]);

        let running = Release::parse("0.2.0").unwrap();
        assert!(published("[]", &running).unwrap().is_empty());
        assert!(published("{\"message\": \"Not Found\"}", &running).is_err());
    }
}
