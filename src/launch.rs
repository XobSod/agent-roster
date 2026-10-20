//! The command a pane runs: source the env files, then exec the agent
//! (behaviour verified in bash, zsh and sh).

use std::path::Path;

use crate::config::Config;

/// Run as `$SHELL -c SCRIPT roster-launch <global env> <project env> <agent argv…>`.
/// Each `.` must stay the last command of its `&&` list: on the left of `&&`/`||`
/// the shell ignores `set -e`, also inside the sourced file.
const SCRIPT: &str = r#"trap 'echo "roster: preparing the environment failed (exit $?)" >&2' EXIT
set -ae
[ -f "$1" ] && . "$1"
[ -f "$2" ] && . "$2"
set +ae
trap - EXIT
shift 2
exec "$@""#;

/// Shells that can source the env files with the script above.
const POSIX_SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh"];

/// The shell that sources the env files: `shell` from the config, else `$SHELL`
/// if it's POSIX-like (the user's syntax works in their env files), else /bin/sh.
pub fn shell(config: &Config, env_shell: Option<&str>) -> String {
    if let Some(shell) = &config.shell {
        return shell.clone();
    }
    match env_shell {
        Some(s)
            if Path::new(s)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| POSIX_SHELLS.contains(&n)) =>
        {
            s.to_string()
        }
        _ => "/bin/sh".to_string(),
    }
}

/// The full pane argv: the env-sourcing wrapper around `agent_argv`.
pub fn wrap(
    shell: &str,
    global_env: &Path,
    project_env: &Path,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string(),
        "-c".to_string(),
        SCRIPT.to_string(),
        "roster-launch".to_string(),
        global_env.to_string_lossy().into_owned(),
        project_env.to_string_lossy().into_owned(),
    ];
    argv.extend(agent_argv.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn picks_a_posix_shell() {
        let config = Config::default();
        assert_eq!(shell(&config, Some("/usr/bin/zsh")), "/usr/bin/zsh");
        assert_eq!(shell(&config, Some("/usr/bin/fish")), "/bin/sh");
        assert_eq!(shell(&config, None), "/bin/sh");
        let config = Config {
            shell: Some("/opt/bash".into()),
            ..Config::default()
        };
        assert_eq!(shell(&config, Some("/usr/bin/zsh")), "/opt/bash");
    }

    #[test]
    fn sources_both_files_and_execs_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let (global, project) = (dir.path().join("env"), dir.path().join("project.env"));
        std::fs::write(&global, "A=1\nB=\"$(echo two)\"\n").unwrap();
        std::fs::write(&project, "A=override\n").unwrap();
        let agent = ["sh", "-c", r#"echo "$A $B $1""#, "x", "arg with spaces"].map(String::from);
        let argv = wrap("/bin/sh", &global, &project, &agent);
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "override two arg with spaces\n"
        );
    }

    #[test]
    fn a_failing_env_file_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("env");
        std::fs::write(&global, "TOKEN=\"$(false)\"\n").unwrap();
        let agent = ["echo", "agent started"].map(String::from);
        let argv = wrap("/bin/sh", &global, &dir.path().join("missing"), &agent);
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("roster: preparing the environment failed")
        );
    }
}
