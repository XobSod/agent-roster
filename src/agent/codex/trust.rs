//! Codex's trust of a folder, as its trust screen keeps it: in its
//! `config.toml` (in `$CODEX_HOME`, else `~/.codex`), the folder's table
//! under `projects` with `trust_level = "trusted"`. Only there: one given as
//! `-c` counts for nothing, nor do a sandbox and an approval policy given.
//! Codex looks for the folder's own table, and without one, its git
//! repository's (a worktree's is its repository's), which is what its screen
//! trusts for any folder in it. So a folder with neither is trusted alone,
//! and one whose own or repository's table says anything is left as it is:
//! trusted already, or told not to be (`"untrusted"`), which a folder of its
//! own trusted would override.
//!
//! The file is edited as it's written, its comments and layout kept, under a
//! lock of roster's (`locked::update`), so sessions starting at once each
//! keep the others' folders.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Trusts `dir` in the config at `file`, unless it's trusted already, or was
/// told not to be: whether it was written. No config yet is an empty one.
pub(super) fn trust(file: &Path, dir: &Path) -> Result<bool> {
    crate::agent::locked::update(file, |text| trusted(file, text, dir))
}

/// The config `text` with `dir` trusted; None if it's trusted already, or
/// was told not to be.
fn trusted(file: &Path, text: &str, dir: &Path) -> Result<Option<String>> {
    let mut config: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("{} isn't TOML roster can read", file.display()))?;
    let key = dir.to_string_lossy();
    let said = |key: &str| {
        (config.get("projects"))
            .and_then(|p| p.get(key))
            .and_then(|p| p.get("trust_level"))
            .is_some()
    };
    if said(&key) || repository(dir).is_some_and(|root| said(&root.to_string_lossy())) {
        return Ok(None);
    }
    let projects = config
        .entry("projects")
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            // Each folder's table on its own, as codex writes them.
            table.set_implicit(true);
            toml_edit::Item::Table(table)
        })
        .as_table_like_mut()
        .with_context(|| format!("{}'s projects isn't a table", file.display()))?;
    let project = projects
        .entry(key.as_ref())
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
        .with_context(|| format!("{}'s {key} isn't a table", file.display()))?;
    project.insert("trust_level", toml_edit::value("trusted"));
    Ok(Some(config.to_string()))
}

/// The git repository `dir` is in, as codex finds it for its trust: the
/// folder with the `.git` nearest up from it; for a worktree's (a `.git` file
/// pointing into `<repository>/.git/worktrees/`), its repository. None
/// outside one.
fn repository(dir: &Path) -> Option<PathBuf> {
    let root = dir.ancestors().find(|d| d.join(".git").exists())?;
    let worktree = std::fs::read_to_string(root.join(".git"))
        .ok()
        .and_then(|text| {
            let gitdir = root.join(text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim());
            // Git may write it relative to the worktree: resolved, it names
            // the repository as its key in the config does.
            let gitdir = std::fs::canonicalize(gitdir).ok()?;
            let worktrees = gitdir.parent()?;
            (worktrees.file_name()? == "worktrees")
                .then(|| worktrees.parent()?.parent().map(Path::to_path_buf))?
        });
    Some(worktree.unwrap_or_else(|| root.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_folder_is_trusted_as_codexs_screen_trusts_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let work = Path::new("/work/api");
        // No config yet: one with the folder alone.
        assert!(trust(&file, work).unwrap());
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "[projects.\"/work/api\"]\ntrust_level = \"trusted\"\n"
        );
        // Another, beside what's there, which stays as it was written.
        let mine = "# mine\nmodel = \"gpt-6-luna\"  # the one I use\n\n[projects.\"/work/old\"]\ntrust_level = \"untrusted\"\n";
        fs::write(&file, mine).unwrap();
        assert!(trust(&file, work).unwrap());
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with(mine), "{text}");
        assert!(
            text.ends_with("[projects.\"/work/api\"]\ntrust_level = \"trusted\"\n"),
            "{text}"
        );
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["projects"]["/work/api"]["trust_level"].as_str(),
            Some("trusted")
        );
        // Trusted already, or told not to be: nothing written.
        assert!(!trust(&file, work).unwrap());
        assert!(!trust(&file, Path::new("/work/old")).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
        // What isn't TOML is left alone.
        fs::write(&file, "model = ").unwrap();
        assert!(trust(&file, work).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "model = ");
    }

    #[test]
    fn a_folder_in_a_repository_goes_by_the_repositorys_trust_as_codex_does() {
        let tmp = tempfile::tempdir().unwrap();
        // As the folder a session runs in is: links resolved.
        let dir = fs::canonicalize(tmp.path()).unwrap();
        let file = dir.join("config.toml");
        let repo = dir.join("repo");
        let svc = repo.join("svc");
        fs::create_dir_all(&svc).unwrap();
        // Worktrees of it: a `.git` file pointing into the repository's, as
        // git writes it, or relative to the worktree.
        let wt = dir.join("wt");
        let rel = dir.join("rel");
        for (worktree, gitdir) in [
            (&wt, repo.join(".git/worktrees/wt").display().to_string()),
            (&rel, "../repo/.git/worktrees/rel".to_string()),
        ] {
            let name = worktree.file_name().unwrap();
            fs::create_dir_all(repo.join(".git/worktrees").join(name)).unwrap();
            fs::create_dir_all(worktree.join("svc")).unwrap();
            fs::write(worktree.join(".git"), format!("gitdir: {gitdir}\n")).unwrap();
        }
        // A folder that isn't a repository: codex doesn't go up from one.
        let plain = dir.join("plain");
        fs::create_dir_all(plain.join("sub")).unwrap();
        let level = |path: &Path, level: &str| {
            format!(
                "[projects.\"{}\"]\ntrust_level = \"{level}\"\n",
                path.display()
            )
        };
        // The repository told not to be trusted: none of it is.
        let told = level(&repo, "untrusted") + &level(&plain, "untrusted");
        fs::write(&file, &told).unwrap();
        for folder in [&svc, &wt, &wt.join("svc"), &rel.join("svc")] {
            assert!(!trust(&file, folder).unwrap(), "{}", folder.display());
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), told);
        // Under a plain folder told so, a folder is trusted all the same.
        assert!(trust(&file, &plain.join("sub")).unwrap());
        // The repository trusted already: nothing to write for a folder of it.
        let trusted = level(&repo, "trusted");
        fs::write(&file, &trusted).unwrap();
        assert!(!trust(&file, &svc).unwrap());
        assert!(!trust(&file, &wt).unwrap());
        assert!(!trust(&file, &rel).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), trusted);
        // Neither has a say yet: the folder alone is trusted.
        fs::write(&file, "").unwrap();
        assert!(trust(&file, &svc).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), level(&svc, "trusted"));
    }
}
