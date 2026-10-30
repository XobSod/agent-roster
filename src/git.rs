//! What roster asks of git for sessions in worktrees: run as `git -C <dir>`,
//! in an environment of its own, what git says when it refuses passed on.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

/// `git -C dir args…`, out of reach of a repository or a prompt the
/// environment would point it at (roster run from a git hook, say).
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_NAMESPACE",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null());
    cmd
}

fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    command(dir, args).output().context("can't run git")
}

/// What `git args…` printed; an error with what it said if it failed.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = run(dir, args)?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = said.trim().trim_start_matches("fatal: ");
        bail!("git {} failed: {said}", args.first().unwrap_or(&""));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Whether `git args…` succeeded: for its questions answered by the exit
/// status alone.
fn yes(dir: &Path, args: &[&str]) -> bool {
    run(dir, args).is_ok_and(|out| out.status.success())
}

/// The root of the working tree holding `dir` (a worktree's own), None
/// outside git.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(PathBuf::from)
}

/// The repository `dir` is in, as every worktree of it has it: its common
/// git directory, links resolved.
pub fn repository(dir: &Path) -> Option<PathBuf> {
    let common = git(dir, &["rev-parse", "--git-common-dir"]).ok()?;
    let common = Path::new(&common);
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        dir.join(common)
    };
    common.canonicalize().ok()
}

/// Whether `a` and `b` are in the same repository, worktrees included.
pub fn same_repository(a: &Path, b: &Path) -> bool {
    matches!((repository(a), repository(b)), (Some(a), Some(b)) if a == b)
}

/// The branch checked out in `dir`, None if its HEAD is detached.
pub fn current_branch(dir: &Path) -> Option<String> {
    git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()
}

/// The commit `rev` names in `dir`'s repository.
pub fn commit(dir: &Path, rev: &str) -> Result<String> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .with_context(|| format!("{rev} names no commit"))
}

/// The repository's default branch: its remote's, if it has a local
/// branch of that name, else `main` or `master`, whichever there is.
pub fn default_branch(dir: &Path) -> Option<String> {
    let remote = git(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .and_then(|r| r.strip_prefix("origin/").map(str::to_string));
    remote
        .into_iter()
        .chain(["main".to_string(), "master".to_string()])
        .find(|b| branch_exists(dir, b))
}

/// The local branches, the latest committed to first.
pub fn branches(dir: &Path) -> Vec<String> {
    let listed = git(
        dir,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ],
    );
    listed
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    yes(
        dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

/// Whether git takes `name` for a branch's.
pub fn branch_name_valid(dir: &Path, name: &str) -> bool {
    !name.starts_with('-') && yes(dir, &["check-ref-format", "--branch", name])
}

/// A worktree of a repository, as git lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub path: PathBuf,
    /// Its branch, None if its HEAD is detached.
    pub branch: Option<String>,
    /// Gone, as far as git knows: its directory isn't there.
    pub prunable: bool,
}

/// The repository's worktrees, its main one first.
pub fn worktrees(dir: &Path) -> Result<Vec<Listed>> {
    let text = git(dir, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktrees(&text))
}

fn parse_worktrees(text: &str) -> Vec<Listed> {
    let mut listed = Vec::new();
    for block in text.split("\n\n") {
        let mut path = None;
        let (mut branch, mut prunable) = (None, false);
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "prunable" || line.starts_with("prunable ") {
                prunable = true;
            }
        }
        if let Some(path) = path {
            listed.push(Listed {
                path,
                branch,
                prunable,
            });
        }
    }
    listed
}

/// Where `branch` is checked out among the repository's worktrees, if it is.
pub fn checked_out(dir: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let listed = worktrees(dir)?;
    Ok(listed
        .into_iter()
        .find(|w| !w.prunable && w.branch.as_deref() == Some(branch))
        .map(|w| w.path))
}

/// Makes a worktree of `repo` at `path`: on a new `branch` from `base`, or
/// with no base, on `branch` as it is.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: Option<&str>) -> Result<()> {
    let path = path.to_string_lossy();
    match base {
        Some(base) => git(repo, &["worktree", "add", "-b", branch, "--", &path, base]),
        None => git(repo, &["worktree", "add", "--", &path, branch]),
    }?;
    Ok(())
}

/// Lets git forget the worktrees whose directories are gone.
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"])?;
    Ok(())
}

/// Removes the worktree at `path`; with `force`, though it has changes
/// not committed. Git refuses a locked one either way.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let path = path.to_string_lossy();
    // Its check for untracked files would follow a config hiding them.
    let mut args = vec![
        "-c",
        "status.showUntrackedFiles=normal",
        "worktree",
        "remove",
    ];
    if force {
        args.push("--force");
    }
    args.extend(["--", &path]);
    git(repo, &args)?;
    Ok(())
}

/// How many files in the worktree at `dir` are changed and not committed,
/// untracked ones included.
pub fn changed_files(dir: &Path) -> Result<usize> {
    let text = git(
        dir,
        &[
            "-c",
            "status.showUntrackedFiles=all",
            "status",
            "--porcelain",
        ],
    )?;
    Ok(text.lines().filter(|l| !l.is_empty()).count())
}

/// How many commits `dir`'s HEAD has that `base` hasn't, and the other way
/// round.
pub fn ahead_behind(dir: &Path, base: &str) -> Result<(usize, usize)> {
    let text = git(
        dir,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{base}"),
        ],
    )?;
    let mut counts = text.split_whitespace().map(str::parse::<usize>);
    match (counts.next(), counts.next()) {
        (Some(Ok(ahead)), Some(Ok(behind))) => Ok((ahead, behind)),
        _ => bail!("git rev-list said {text:?}"),
    }
}

/// How many commits `branch` has that `base` hasn't: none if it's merged.
pub fn unmerged(repo: &Path, branch: &str, base: &str) -> Result<usize> {
    let text = git(repo, &["rev-list", "--count", &format!("{base}..{branch}")])?;
    text.trim()
        .parse()
        .with_context(|| format!("git rev-list said {text:?}"))
}

/// What the worktree at `dir` changed against `base`, committed or not:
/// files, lines added and lines removed (untracked files aside).
pub fn diff_stat(dir: &Path, base: &str) -> Result<(usize, usize, usize)> {
    let from = git(dir, &["merge-base", base, "HEAD"])?;
    let text = git(dir, &["diff", "--shortstat", from.trim()])?;
    Ok(parse_shortstat(&text))
}

fn parse_shortstat(text: &str) -> (usize, usize, usize) {
    let (mut files, mut added, mut removed) = (0, 0, 0);
    for part in text.split(',') {
        let mut words = part.split_whitespace();
        let (Some(n), Some(what)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(n) = n.parse() else { continue };
        if what.starts_with("file") {
            files = n;
        } else if what.starts_with("insertion") {
            added = n;
        } else if what.starts_with("deletion") {
            removed = n;
        }
    }
    (files, added, removed)
}

/// Deletes `branch`; with `force`, though it isn't merged.
pub fn delete_branch(repo: &Path, branch: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    git(repo, &["branch", flag, "--", branch])?;
    Ok(())
}

/// A directory's name for a worktree of `branch`: its `/` as `-`; a long
/// one cut, with a hash of the whole, so two long names don't meet.
pub fn directory_name(branch: &str) -> String {
    let name = branch.replace('/', "-");
    if name.chars().count() <= 40 {
        return name;
    }
    let start: String = name.chars().take(23).collect();
    format!("{start}-{}", crate::util::fnv1a(branch.as_bytes()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `git args…` in `dir`, which must work.
    pub(crate) fn sh(dir: &Path, args: &[&str]) -> String {
        let mut cmd = command(dir, args);
        cmd.env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repository in `dir`, on `main`, with one commit.
    pub(crate) fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        sh(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        sh(dir, &["add", "a.txt"]);
        sh(dir, &["commit", "-q", "-m", "first"]);
    }

    #[test]
    fn worktrees_are_made_listed_counted_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        repo(&main);
        assert_eq!(toplevel(&main), Some(main.canonicalize().unwrap()));
        assert_eq!(toplevel(tmp.path()), None);
        assert_eq!(current_branch(&main).as_deref(), Some("main"));
        assert_eq!(default_branch(&main).as_deref(), Some("main"));
        assert!(branch_name_valid(&main, "fix/auth"));
        assert!(!branch_name_valid(&main, "bad..name"));
        assert!(!branch_name_valid(&main, "-x"));
        // A new branch from main, in a worktree of its own.
        let wt = tmp.path().join("wt");
        add_worktree(&main, &wt, "fix", Some("main")).unwrap();
        assert!(same_repository(&main, &wt));
        assert_eq!(current_branch(&wt).as_deref(), Some("fix"));
        assert_eq!(
            checked_out(&main, "fix").unwrap(),
            Some(wt.canonicalize().unwrap())
        );
        assert_eq!(checked_out(&main, "nope").unwrap(), None);
        // Git won't check one branch out twice.
        assert!(add_worktree(&main, &tmp.path().join("again"), "fix", None).is_err());
        // A commit, and a change not committed.
        std::fs::write(wt.join("b.txt"), "b\nb\n").unwrap();
        sh(&wt, &["add", "b.txt"]);
        sh(&wt, &["commit", "-q", "-m", "b"]);
        std::fs::write(wt.join("c.txt"), "c\n").unwrap();
        assert_eq!(changed_files(&wt).unwrap(), 1);
        assert_eq!(ahead_behind(&wt, "main").unwrap(), (1, 0));
        assert_eq!(unmerged(&main, "fix", "main").unwrap(), 1);
        assert_eq!(diff_stat(&wt, "main").unwrap(), (1, 2, 0));
        assert_eq!(branches(&main).len(), 2);
        // Not with what's not committed, unless forced.
        assert!(remove_worktree(&main, &wt, false).is_err());
        remove_worktree(&main, &wt, true).unwrap();
        assert!(!wt.exists());
        assert!(delete_branch(&main, "fix", false).is_err());
        delete_branch(&main, "fix", true).unwrap();
        assert!(!branch_exists(&main, "fix"));
        // Gone by hand: listed as prunable till pruned.
        add_worktree(&main, &wt, "other", Some("main")).unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        assert!(worktrees(&main).unwrap()[1].prunable);
        prune_worktrees(&main).unwrap();
        assert_eq!(worktrees(&main).unwrap().len(), 1);
        add_worktree(&main, &wt, "other", None).unwrap();
        assert_eq!(current_branch(&wt).as_deref(), Some("other"));
    }

    #[test]
    fn git_says_how_much_changed_in_a_line() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)"),
            (3, 10, 2)
        );
        assert_eq!(parse_shortstat(" 1 file changed, 1 deletion(-)"), (1, 0, 1));
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[test]
    fn long_branch_names_are_cut_apart() {
        assert_eq!(directory_name("fix/auth"), "fix-auth");
        let a = directory_name(&format!("{}-one", "x".repeat(40)));
        let b = directory_name(&format!("{}-two", "x".repeat(40)));
        assert_ne!(a, b);
        assert!(a.chars().count() <= 40, "{a}");
    }
}
