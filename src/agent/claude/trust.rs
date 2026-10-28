//! Claude's trust of a folder, as its trust dialog keeps it: in its global
//! config, `.claude.json` (in `$CLAUDE_CONFIG_DIR`, else your home), the
//! folder's entry under `projects` with `hasTrustDialogAccepted`. A folder
//! with no entry gets one as Claude makes it, its defaults with the trust:
//! Claude takes the defaults only for a folder it has no entry for.
//!
//! Claude rewrites that file itself, often, under a lock of its own,
//! `.claude.json.lock`, which roster takes too (`locked::update`), from
//! reading the file to replacing it, whole, in one rename. All of it but that folder's entry is written back as it was read,
//! each value's text as it was and in its place: read as numbers, Claude's
//! figures (a cost to the last digit) wouldn't all come back the same.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

/// Claude's global config file.
pub(super) fn config_file() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir).join(".claude.json")),
        None => dirs::home_dir().map(|h| h.join(".claude.json")),
    }
}

/// A JSON object's members as they were written, in order, each value's
/// text untouched unless it's replaced.
#[derive(Default)]
struct Object(Vec<(String, Member)>);

enum Member {
    Raw(Box<RawValue>),
    Object(Object),
    New(Value),
}

impl Object {
    fn parse(raw: &str) -> Option<Object> {
        serde_json::from_str(raw).ok()
    }

    fn raw(&self, key: &str) -> Option<&RawValue> {
        self.0.iter().find_map(|(k, m)| match m {
            Member::Raw(raw) if k == key => Some(raw.as_ref()),
            _ => None,
        })
    }

    /// Sets `key` to `member`, where it was, else last.
    fn set(&mut self, key: &str, member: Member) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, m)) => *m = member,
            None => self.0.push((key.to_string(), member)),
        }
    }
}

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Object, D::Error> {
        struct Members;
        impl<'de> serde::de::Visitor<'de> for Members {
            type Value = Object;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Object, A::Error> {
                let mut members = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    members.push((key, Member::Raw(value)));
                }
                Ok(Object(members))
            }
        }
        d.deserialize_map(Members)
    }
}

impl Serialize for Object {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|(k, m)| (k, m)))
    }
}

impl Serialize for Member {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Member::Raw(raw) => raw.serialize(s),
            Member::Object(object) => object.serialize(s),
            Member::New(value) => value.serialize(s),
        }
    }
}

/// Trusts `dir` in the config at `file`, unless it's trusted already (or
/// there's no config yet: Claude hasn't been run, and makes its own as it
/// first is): whether it was written.
pub(super) fn trust(file: &Path, dir: &Path) -> Result<bool> {
    if !file.exists() {
        return Ok(false);
    }
    crate::agent::locked::update(file, |text| trusted(file, text, dir))
}

/// The config `text` with `dir` trusted; None if it's trusted already.
fn trusted(file: &Path, text: &str, dir: &Path) -> Result<Option<String>> {
    let unlike = || format!("{} isn't laid out as Claude's config", file.display());
    let mut config = Object::parse(text).with_context(unlike)?;
    let mut projects = match config.raw("projects") {
        Some(raw) => Object::parse(raw.get()).with_context(unlike)?,
        None => Object::default(),
    };
    let key = dir.to_string_lossy();
    let entry = match projects.raw(&key) {
        Some(raw) => {
            let mut entry = Object::parse(raw.get()).with_context(unlike)?;
            if entry
                .raw("hasTrustDialogAccepted")
                .is_some_and(|t| t.get() == "true")
            {
                return Ok(None);
            }
            entry.set("hasTrustDialogAccepted", Member::New(Value::Bool(true)));
            Member::Object(entry)
        }
        None => Member::New(json!({
            "allowedTools": [],
            "mcpContextUris": [],
            "mcpServers": {},
            "enabledMcpjsonServers": [],
            "disabledMcpjsonServers": [],
            "hasTrustDialogAccepted": true,
            "hasClaudeMdExternalIncludesApproved": false,
            "hasClaudeMdExternalIncludesWarningShown": false,
        })),
    };
    projects.set(&key, entry);
    config.set("projects", Member::Object(projects));
    Ok(Some(serde_json::to_string_pretty(&config)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_folder_is_trusted_as_claudes_dialog_trusts_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let work = Path::new("/work/api");
        // No config yet: Claude's to make.
        assert!(!trust(&file, work).unwrap());
        assert!(!file.exists());
        fs::write(
            &file,
            r#"{"numStartups":3,"projects":{"/work/web":{"allowedTools":["Bash"],"hasTrustDialogAccepted":false}}}"#,
        )
        .unwrap();
        // A new entry, with Claude's defaults; what else is there, kept.
        assert!(trust(&file, work).unwrap());
        let config: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let entry = &config["projects"]["/work/api"];
        assert_eq!(entry["hasTrustDialogAccepted"], true);
        assert_eq!(entry["allowedTools"], json!([]));
        assert_eq!(entry["mcpServers"], json!({}));
        assert_eq!(config["numStartups"], 3);
        // Trusted already: nothing written.
        assert!(!trust(&file, work).unwrap());
        // One there, untrusted: trusted, the rest of it as it was.
        assert!(trust(&file, Path::new("/work/web")).unwrap());
        let config: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let entry = &config["projects"]["/work/web"];
        assert_eq!(entry["hasTrustDialogAccepted"], true);
        assert_eq!(entry["allowedTools"], json!(["Bash"]));
        // The rest written back as it was, its figures to the last digit and
        // in their order: Claude's own layout, the entry added at the end.
        let claude = "{\n  \"numStartups\": 3,\n  \"projects\": {\n    \"/work/web\": {\n      \"lastCost\": 2.9129635999999994,\n      \"hasTrustDialogAccepted\": false,\n      \"allowedTools\": []\n    }\n  },\n  \"cost\": 207.12857750000006\n}";
        fs::write(&file, claude).unwrap();
        assert!(trust(&file, Path::new("/work/new")).unwrap());
        let text = fs::read_to_string(&file).unwrap();
        let entry = text.find(",\n    \"/work/new\": {").unwrap();
        assert_eq!(&text[..entry], &claude[..claude.find("\n  },").unwrap()]);
        assert!(
            text.ends_with("\n  },\n  \"cost\": 207.12857750000006\n}"),
            "{text}"
        );
        // One there: its trust set where it was, the rest of it as it was.
        assert!(trust(&file, Path::new("/work/web")).unwrap());
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("\"lastCost\": 2.9129635999999994,\n      \"hasTrustDialogAccepted\": true,\n      \"allowedTools\": []"),
            "{text}"
        );
        // What isn't Claude's config is left alone.
        fs::write(&file, "[1]").unwrap();
        assert!(trust(&file, work).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "[1]");
    }
}
