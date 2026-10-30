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
//!
//! Its trust of roster's own hooks is each hook's `trusted_hash` under
//! `hooks.state`, by its key, which codex reads from its `-c`s too, over its
//! config's: so they're trusted for each run alone, its config left as it
//! is, and a run of another roster (whose path is in the hook) untrusts
//! none of this one's. The hash is codex's own, of the hook as it's given
//! (its command, roster's path in it), which its server says (`hooks/list`),
//! as it says which aren't trusted. Only roster's are trusted so: given as
//! its `-c`s (codex's `sessionFlags`), and running this roster's `report`.
//! Those of the project and your own codex asks about as it does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::server;

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

/// One of roster's hooks codex doesn't trust: its key, and the hash its
/// trust is kept by.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Untrusted {
    pub key: String,
    pub hash: String,
}

/// How long codex's server is given to list its hooks.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Roster's own hooks in codex's command line `argv` that codex doesn't
/// trust, as its server lists them, given those hooks alone; roster run as
/// `exe`.
pub(super) fn untrusted(argv: &[String], exe: &str) -> Result<Vec<Untrusted>> {
    let mut hooks = Vec::new();
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "-c"
            && let Some(value) = args.next()
            && value.starts_with("hooks.")
        {
            hooks.extend(["-c".to_string(), value.clone()]);
        }
    }
    let Some(command) = argv.first().filter(|_| !hooks.is_empty()) else {
        return Ok(Vec::new());
    };
    let server = server::argv(command.clone(), hooks);
    let mut cmd = std::process::Command::new(&server[0]);
    cmd.args(&server[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let requests = server::requests("hooks/list", Some(json!({})));
    let answered = |printed: &[u8]| server::answer(printed).is_some();
    let input = crate::launch::Input {
        text: requests.as_bytes(),
        answered: &answered,
    };
    let printed = crate::launch::converse(cmd, "codex's server", LIST_TIMEOUT, Some(input))?;
    let Some(answer) = server::answer(&printed.stdout) else {
        let said = String::from_utf8_lossy(&printed.stderr);
        match said.lines().rfind(|l| !l.trim().is_empty()) {
            Some(line) => bail!("codex's server didn't answer: {}", line.trim()),
            None => bail!("codex's server didn't answer"),
        }
    };
    if let Some(message) = answer["error"]["message"].as_str() {
        bail!("codex's server: {message}");
    }
    Ok(listed(&answer["result"], exe))
}

/// Of the hooks codex's server listed (`hooks/list`'s result), roster's own
/// it doesn't trust: given on its command line, running `exe`'s `report`.
fn listed(result: &Value, exe: &str) -> Vec<Untrusted> {
    let ours = format!("{} report ", crate::util::sh_quote(exe));
    let mut found: Vec<Untrusted> = Vec::new();
    let hooks = (result["data"].as_array().into_iter().flatten())
        .flat_map(|folder| folder["hooks"].as_array().into_iter().flatten());
    for hook in hooks {
        let rosters = hook["source"] == "sessionFlags"
            && hook["command"]
                .as_str()
                .is_some_and(|c| c.starts_with(&ours));
        if !rosters || hook["trustStatus"] == "trusted" {
            continue;
        }
        if let (Some(key), Some(hash)) = (hook["key"].as_str(), hook["currentHash"].as_str())
            && !found.iter().any(|f| f.key == key)
        {
            found.push(Untrusted {
                key: key.to_string(),
                hash: hash.to_string(),
            });
        }
    }
    found
}

/// Codex's command line `argv` with `hooks` trusted for that run, as
/// answering its review of them would: as a `-c` after roster's own (a
/// table, as a key of codex's has dots in it, which a `-c`'s name can't).
pub(super) fn trusting(argv: &[String], hooks: &[Untrusted]) -> Vec<String> {
    let mut argv = argv.to_vec();
    let last = (0..argv.len().saturating_sub(1))
        .rfind(|&i| argv[i] == "-c" && argv[i + 1].starts_with("hooks."));
    let (Some(last), false) = (last, hooks.is_empty()) else {
        return argv;
    };
    let mut state = toml_edit::InlineTable::new();
    for hook in hooks {
        let mut trust = toml_edit::InlineTable::new();
        trust.insert("trusted_hash", hook.hash.as_str().into());
        state.insert(&hook.key, trust.into());
    }
    argv.splice(
        last + 2..last + 2,
        ["-c".into(), format!("hooks.state={state}")],
    );
    argv
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

    /// As 0.165's server lists them (`hooks/list`), but for fields roster
    /// doesn't read.
    fn hook(key: &str, source: &str, command: &str, status: &str) -> Value {
        json!({"key": key, "eventName": "stop", "handlerType": "command", "command": command,
            "source": source, "sourcePath": "/<session-flags>/config.toml",
            "currentHash": format!("sha256:{key}"), "trustStatus": status})
    }

    #[test]
    fn only_rosters_own_hooks_not_trusted_are_to_be() {
        let exe = "/opt/my roster";
        let ours = "'/opt/my roster' report idle";
        let result = json!({"data": [{"cwd": "/work", "hooks": [
            hook("/<session-flags>/config.toml:stop:0:0", "sessionFlags", ours, "modified"),
            hook("/<session-flags>/config.toml:session_start:0:0", "sessionFlags",
                "'/opt/my roster' report session", "untrusted"),
            // Trusted already.
            hook("/<session-flags>/config.toml:post_tool_use:0:0", "sessionFlags",
                "'/opt/my roster' report working", "trusted"),
            // Another roster's, and one of yours given the same way.
            hook("/<session-flags>/config.toml:stop:1:0", "sessionFlags",
                "'/elsewhere/roster' report idle", "untrusted"),
            hook("/<session-flags>/config.toml:stop:2:0", "sessionFlags", "notify-send done", "untrusted"),
            // The project's, running roster's command all the same.
            hook("/work/.codex/hooks.json:stop:0:0", "project", ours, "untrusted"),
        ]}, {"cwd": "/work", "hooks": [
            hook("/<session-flags>/config.toml:stop:0:0", "sessionFlags", ours, "modified"),
        ]}]});
        let keys: Vec<String> = listed(&result, exe).into_iter().map(|h| h.key).collect();
        assert_eq!(
            keys,
            [
                "/<session-flags>/config.toml:stop:0:0",
                "/<session-flags>/config.toml:session_start:0:0"
            ]
        );
        assert!(listed(&json!({}), exe).is_empty());
    }

    #[test]
    fn hooks_are_trusted_for_the_run_after_rosters_own() {
        let argv: Vec<String> = [
            "codex",
            "--no-daemon",
            "-c",
            "model=\"m\"",
            "-c",
            "hooks.Stop=[]",
            "-c",
            "hooks.SessionStart=[]",
            "resume",
            "01a1",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(trusting(&argv, &[]), argv);
        let hooks = [
            Untrusted {
                key: "/<session-flags>/config.toml:stop:0:0".into(),
                hash: "sha256:a1".into(),
            },
            Untrusted {
                key: "/<session-flags>/config.toml:session_start:0:0".into(),
                hash: "sha256:b2".into(),
            },
        ];
        let trusted = trusting(&argv, &hooks);
        assert_eq!(trusted[..8], argv[..8]);
        assert_eq!(trusted[8], "-c");
        assert_eq!(trusted[10..], argv[8..]);
        // As codex reads a `-c`: its name up to the first `=`, its value TOML.
        let (name, value) = trusted[9].split_once('=').unwrap();
        assert_eq!(name, "hooks.state");
        let state: toml::Table = toml::from_str(&format!("state = {value}")).unwrap();
        assert_eq!(
            state["state"]["/<session-flags>/config.toml:stop:0:0"]["trusted_hash"].as_str(),
            Some("sha256:a1")
        );
        assert_eq!(
            state["state"]["/<session-flags>/config.toml:session_start:0:0"]["trusted_hash"]
                .as_str(),
            Some("sha256:b2")
        );
        // No hooks of roster's to put them after: as it was.
        let bare: Vec<String> = ["codex", "resume", "01a1"].map(String::from).to_vec();
        assert_eq!(trusting(&bare, &hooks), bare);
    }

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
