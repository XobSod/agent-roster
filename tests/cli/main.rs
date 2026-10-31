//! End-to-end tests of the CLI against a real tmux server on a private socket,
//! with a stand-in script as the agent and `roster report` called the way Claude's
//! hooks would call it.

#[path = "../common/mod.rs"]
mod common;

mod claude;
mod codex;
mod config;
mod generic;
mod pi;
mod sessions;
mod tokens;
mod usage;
mod worktrees;

/// `--version` is a SemVer version (`build.rs`): three numbers, maybe a
/// pre-release, maybe build metadata (how far past a release, and the commit).
#[test]
fn the_version_is_semver() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_roster"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let shown = String::from_utf8(out.stdout).unwrap();
    let version = shown.trim_end().strip_prefix("roster ").unwrap();
    let identifiers = |part: &str| {
        part.split('.')
            .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    };
    let (rest, meta) = version.split_once('+').unwrap_or((version, ""));
    let (core, pre) = rest.split_once('-').unwrap_or((rest, ""));
    let numbers: Vec<&str> = core.split('.').collect();
    assert!(
        numbers.len() == 3
            && numbers
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && (pre.is_empty() || identifiers(pre))
            && (meta.is_empty() || identifiers(meta)),
        "{version}"
    );
}
