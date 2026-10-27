//! Codex's trust of a folder, as its trust screen keeps it: in its
//! `config.toml` (in `$CODEX_HOME`, else `~/.codex`), the folder's table
//! under `projects` with `trust_level = "trusted"`. Only there: one given as
//! `-c` counts for nothing, nor do a sandbox and an approval policy given.
//! Codex asks for a git repository's root, which covers the folders in it,
//! and trusts that; a folder of it trusted alone is enough for that folder.
//! One it was told not to trust (`"untrusted"`) is left as it was.
//!
//! The file is edited as it's written, its comments and layout kept, under a
//! lock of roster's (`locked::update`), so sessions starting at once each
//! keep the others' folders.

use std::path::Path;

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
    if config
        .get("projects")
        .and_then(|p| p.get(key.as_ref()))
        .and_then(|p| p.get("trust_level"))
        .is_some()
    {
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
}
