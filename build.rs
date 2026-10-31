//! Works out which version this build is (`src/version.rs`) from the
//! checkout's git, for the code as `env!("ROSTER_VERSION")`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/version.rs"]
mod version;

fn main() {
    let package = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    // Run again only for what's said here, not for any file changed in the
    // package, as cargo would otherwise: the build directory changes with each build.
    println!("cargo:rerun-if-changed=build.rs");
    let describe = describe(&dir);
    println!(
        "cargo:rustc-env=ROSTER_VERSION={}",
        version::version(&package, describe.as_deref())
    );
}

/// What `git describe` says of the package's checkout, none if it isn't in
/// one of its own (no git, or unpacked inside another repository, whose
/// version it isn't), telling cargo to run this again when that may change:
/// a commit, a checkout or a tag, and the files git has, changed or not.
fn describe(dir: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        let out = String::from_utf8(out.stdout)
            .ok()
            .filter(|_| out.status.success())?;
        Some(out.trim_end_matches('\n').to_string())
    };
    let top = git(&["rev-parse", "--show-toplevel"])?;
    if Path::new(&top).canonicalize().ok()? != dir.canonicalize().ok()? {
        return None;
    }
    // A worktree's HEAD, index and HEAD's log are its own; the refs are the
    // repository's. Of the branches only the one checked out, so a commit in
    // another worktree doesn't rebuild this one.
    let git_dir = dir.join(git(&["rev-parse", "--git-dir"])?);
    let common = dir.join(git(&["rev-parse", "--git-common-dir"])?);
    let log = git_dir.join("logs/HEAD");
    let mut watched = vec![
        git_dir.join("HEAD"),
        git_dir.join("index"),
        common.join("packed-refs"),
        common.join("refs/tags"),
    ];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(common.join(branch));
    }
    // Without HEAD's log (core.logAllRefUpdates off), a commit on a packed
    // branch would only add its ref's file: every branch, then.
    watched.push(if log.exists() {
        log
    } else {
        common.join("refs/heads")
    });
    // Each top-level file and directory git has (`src`, `Cargo.toml`, ...):
    // an edit to any of them changes whether it's dirty. Not the build
    // directory, which git hasn't, nor the whole checkout, which holds it.
    let mut tops: Vec<String> = git(&["ls-files", "-z"])?
        .split('\0')
        .filter_map(|path| path.split('/').next())
        .filter(|top| !top.is_empty())
        .map(str::to_string)
        .collect();
    tops.dedup();
    watched.extend(tops.iter().map(|top| dir.join(top)));
    // One that isn't there would have cargo run this every time.
    for path in watched.iter().filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    git(&[
        "describe", "--tags", "--long", "--dirty", "--always", "--match", "v[0-9]*",
    ])
}
