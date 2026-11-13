//! What roster keeps secret: the CLI Proxy's management key (`cli_proxy.key`
//! in the settings). Not in config.toml, which may be shared or kept in a
//! dotfiles repository, nor beside it: in `secrets.toml` in roster's state
//! directory, a file of its own (never written through a link), readable by
//! the user alone, and not used if others can read it. Changed under its
//! lock, as config.toml is (`locked`); read each time it's needed, so a key
//! set from the CLI is used by a running TUI too.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use toml_edit::DocumentMut;

use crate::agent::locked;

/// The setting the key is, as `roster config set` takes it.
pub const PROXY_KEY: &str = "cli_proxy.key";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Secrets {
    cli_proxy: ProxySecrets,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProxySecrets {
    key: Option<String>,
}

/// Whether the key is set, as the settings show it.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyState {
    NotSet,
    Set,
    /// The file is there but can't be used: why.
    Unreadable(String),
}

impl KeyState {
    pub fn read(path: &Path) -> KeyState {
        match proxy_key(path) {
            Ok(Some(_)) => KeyState::Set,
            Ok(None) => KeyState::NotSet,
            Err(e) => KeyState::Unreadable(format!("{e:#}")),
        }
    }
}

/// The CLI Proxy's management key, if one is set.
pub fn proxy_key(path: &Path) -> Result<Option<String>> {
    let text = read(path)?;
    let secrets: Secrets = toml::from_str(&text).map_err(|e| invalid(path, &text, e.span()))?;
    Ok(secrets.cli_proxy.key.filter(|k| !k.is_empty()))
}

/// That the file at `path` (`text`) can't be read, and where (`span`): not
/// the parser's own words, which quote the line, and may name the value,
/// either of them the key.
fn invalid(path: &Path, text: &str, span: Option<std::ops::Range<usize>>) -> anyhow::Error {
    let at = span.map_or(String::new(), |span| {
        let before = text.get(..span.start).unwrap_or(text);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        format!(" at line {line}, column {column}")
    });
    anyhow!(
        "{} can't be read{at} (what's there isn't shown: it may be the key)",
        path.display()
    )
}

/// Sets the key to `key`; None (or empty) takes it out. The rest of the file
/// stays as it was, comments included.
pub fn set_proxy_key(path: &Path, key: Option<&str>) -> Result<()> {
    let key = key.map(str::trim).filter(|k| !k.is_empty());
    if key.is_some_and(|k| k.chars().any(char::is_control)) {
        bail!("a key is one line of text, without control characters");
    }
    locked::update_with(path, write, |text| {
        let mut doc: DocumentMut =
            (text.parse()).map_err(|e: toml_edit::TomlError| invalid(path, text, e.span()))?;
        match key {
            Some(key) => {
                let table = doc
                    .entry("cli_proxy")
                    .or_insert_with(toml_edit::table)
                    .as_table_like_mut()
                    .with_context(|| format!("{}: cli_proxy isn't a table", path.display()))?;
                table.insert("key", toml_edit::value(key));
            }
            None => {
                let Some(table) = doc.get_mut("cli_proxy").and_then(|t| t.as_table_like_mut())
                else {
                    return Ok(None);
                };
                // The table stays, with what's written above it.
                if table.remove("key").is_none() {
                    return Ok(None);
                }
            }
        }
        let changed = doc.to_string();
        toml::from_str::<Secrets>(&changed).map_err(|e| invalid(path, &changed, e.span()))?;
        Ok(Some(changed))
    })?;
    Ok(())
}

/// The file's text, "" if there's none: refused if it's a link, or if others
/// may read it.
fn read(path: &Path) -> Result<String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e).with_context(|| format!("can't read {}", path.display())),
    };
    if meta.file_type().is_symlink() {
        bail!(
            "{} is a link: roster keeps it as a file of its own",
            path.display()
        );
    }
    if meta.permissions().mode() & 0o077 != 0 {
        bail!(
            "{} may be read by others, so it isn't used: chmod 600 it",
            path.display()
        );
    }
    fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))
}

/// Writes `text` to `path` in one step, readable by the user alone from the
/// start, refusing a link there rather than writing where it points.
fn write(path: &Path, text: &str) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!(
            "{} is a link: roster keeps it as a file of its own",
            path.display()
        );
    }
    let dir = path
        .parent()
        .with_context(|| format!("{} has no directory", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let partial = path.with_file_name(format!("{name}.{}.partial", std::process::id()));
    // Left by a process gone, its pid now this one's.
    fs::remove_file(&partial).ok();
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| {
            // Exactly that, whatever umask took away.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&partial, path))
        .with_context(|| format!("can't write {}", path.display()));
    if written.is_err() {
        fs::remove_file(&partial).ok();
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_key_is_kept_readable_by_the_user_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/secrets.toml");
        assert_eq!(proxy_key(&file).unwrap(), None);
        assert_eq!(KeyState::read(&file), KeyState::NotSet);
        set_proxy_key(&file, Some("  made-up-key \n")).unwrap();
        assert_eq!(proxy_key(&file).unwrap().as_deref(), Some("made-up-key"));
        assert_eq!(mode(&file), 0o600);
        assert_eq!(KeyState::read(&file), KeyState::Set);
        // Made readable by others: not used, and said why; written again,
        // it's the user's alone again.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let refused = proxy_key(&file).unwrap_err().to_string();
        assert!(refused.contains("chmod 600"), "{refused}");
        assert!(matches!(KeyState::read(&file), KeyState::Unreadable(_)));
        set_proxy_key(&file, Some("another-made-up-key")).unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(
            proxy_key(&file).unwrap().as_deref(),
            Some("another-made-up-key")
        );
        // None of the partial copies is left.
        let left: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["secrets.toml"]);
    }

    #[test]
    fn taking_the_key_out_keeps_what_else_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secrets.toml");
        set_proxy_key(&file, Some("made-up-key")).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        fs::write(&file, format!("# Mine.\n{text}")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        set_proxy_key(&file, Some("")).unwrap();
        assert_eq!(proxy_key(&file).unwrap(), None);
        let taken_out = fs::read_to_string(&file).unwrap();
        assert_eq!(taken_out, "# Mine.\n[cli_proxy]\n");
        // Taking out what isn't there writes nothing.
        set_proxy_key(&file, None).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), taken_out);
        assert!(set_proxy_key(&file, Some("two\nlines")).is_err());
    }

    #[test]
    fn a_file_that_cant_be_read_never_shows_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secrets.toml");
        for text in [
            "[cli_proxy]\nkey = \"MADE-UP-SENTINEL\n",
            "[cli_proxy]\nkey = [\"MADE-UP-SENTINEL\"]\n",
            "[cli_proxy]\nkey = 13572468\n",
            "[cli_proxy]\nMADE-UP-SENTINEL = 1\n",
        ] {
            fs::write(&file, text).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
            let said = format!("{:#}", proxy_key(&file).unwrap_err());
            assert!(
                !said.contains("MADE-UP-SENTINEL") && !said.contains("13572468"),
                "{said}"
            );
            assert!(said.contains("at line 2"), "{said}");
            let KeyState::Unreadable(why) = KeyState::read(&file) else {
                panic!("readable: {text}");
            };
            assert!(
                !why.contains("MADE-UP-SENTINEL") && !why.contains("13572468"),
                "{why}"
            );
            // Setting a key over a wrong one mends it; over a file that
            // isn't TOML, it's refused, saying no more.
            if let Err(e) = set_proxy_key(&file, Some("another")) {
                let said = format!("{e:#}");
                assert!(
                    !said.contains("MADE-UP-SENTINEL") && !said.contains("13572468"),
                    "{said}"
                );
            }
        }
    }

    #[test]
    fn a_link_is_neither_read_nor_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("dotfiles/secrets.toml");
        fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        fs::write(&elsewhere, "[cli_proxy]\nkey = \"made-up-key\"\n").unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).unwrap();
        let file = dir.path().join("secrets.toml");
        std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
        assert!(proxy_key(&file).unwrap_err().to_string().contains("link"));
        assert!(set_proxy_key(&file, Some("another-made-up-key")).is_err());
        assert_eq!(
            fs::read_to_string(&elsewhere).unwrap(),
            "[cli_proxy]\nkey = \"made-up-key\"\n"
        );
    }
}
