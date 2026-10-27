//! The command a pane runs: source the env files, then exec the agent
//! (behaviour verified in bash, zsh and sh).

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::Config;

/// Run as `$SHELL -c SCRIPT roster-launch <global env> <project env> <prompt
/// file, or ""> <agent argv…>`. The prompt file's text goes last in the
/// agent's argv, and the file is removed; it's read first, so neither an env
/// file nor `set -a` makes it a variable the agent gets.
/// Each `.` must stay the last command of its `&&` list: on the left of `&&`/`||`
/// the shell ignores `set -e`, also inside the sourced file.
const SCRIPT: &str = r#"trap 'echo "roster: preparing the environment failed (exit $?)" >&2' EXIT
if [ -n "$3" ]; then
  roster_prompt=$(cat -- "$3") || exit
  rm -f -- "$3"
  set -- "$@" "$roster_prompt"
  unset roster_prompt
fi
set -ae
[ -f "$1" ] && . "$1"
[ -f "$2" ] && . "$2"
set +ae
trap - EXIT
shift 3
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

/// The full pane argv: the env-sourcing wrapper around `agent_argv`, with
/// what's in `prompt` (a file, removed once read) as its last argument.
pub fn wrap(
    shell: &str,
    global_env: &Path,
    project_env: &Path,
    prompt: Option<&Path>,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string(),
        "-c".to_string(),
        SCRIPT.to_string(),
        "roster-launch".to_string(),
        global_env.to_string_lossy().into_owned(),
        project_env.to_string_lossy().into_owned(),
        prompt.map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
    ];
    argv.extend(agent_argv.iter().cloned());
    argv
}

/// Runs `cmd` (`what`, in messages) to its end, for at most `timeout`. Its
/// output is read meanwhile, so a full pipe can't hold it up, and what it
/// starts ends with it: on a timeout, or as it exits if something it left
/// running keeps its output open.
pub fn run_with_timeout(
    cmd: std::process::Command,
    what: &str,
    timeout: Duration,
) -> Result<std::process::Output> {
    converse(cmd, what, timeout, None)
}

/// What a command run by `converse` is given to read.
pub struct Input<'a> {
    pub text: &'a [u8],
    /// Whether what it has printed so far answers it: until then its stdin
    /// stays open, for a command that serves until its input ends (codex's
    /// server), and then it's ended, as if it had exited with 0. None: its
    /// stdin closes once it has the text, and it runs to its end.
    pub answered: Option<&'a Answered>,
}

/// Whether what a command printed so far answers it (`Input::answered`).
pub type Answered = dyn Fn(&[u8]) -> bool;

/// `run_with_timeout`, with `input` on the command's stdin.
pub fn converse(
    mut cmd: std::process::Command,
    what: &str,
    timeout: Duration,
    input: Option<Input<'_>>,
) -> Result<std::process::Output> {
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    use std::sync::{Arc, Mutex};
    // In a session of its own, so a process group of its own: what it starts
    // goes with it, on a timeout or when, its work done, something it left
    // running would hold its output open. Without a terminal, too: something
    // asking on one (an env file's `read </dev/tty`) fails at once, where in
    // a group of the terminal's but not its foreground it would stop until
    // the timeout. Ctrl+C on roster doesn't reach it, though.
    // So, where it can, it ends with roster, as it would have in its group.
    // SAFETY: setsid and prctl are async-signal-safe, as `pre_exec` needs.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    let mut child = cmd.spawn().with_context(|| format!("can't run {what}"))?;
    // Small enough for the pipe; one it doesn't read is its loss, which its
    // answer shows.
    let mut stdin = child.stdin.take();
    let mut answered = None;
    if let Some(input) = input {
        if let Some(pipe) = stdin.as_mut() {
            let _ = pipe.write_all(input.text);
        }
        answered = input.answered;
        if answered.is_none() {
            stdin = None;
        }
    }
    let group = child.id() as libc::pid_t;
    // SAFETY: kill only sends a signal, to the group the child leads.
    let end_group = || unsafe {
        libc::kill(-group, libc::SIGKILL);
    };
    type Drained = (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>);
    fn drain(pipe: Option<impl Read + Send + 'static>) -> Drained {
        let out = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&out);
        let reader = std::thread::spawn(move || {
            let Some(mut pipe) = pipe else { return };
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => into.lock().unwrap().extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        (out, reader)
    }
    let (stdout, stdout_reader) = drain(child.stdout.take());
    let (stderr, stderr_reader) = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        // Answered, it has done what it was run for.
        if answered.is_some_and(|done| done(&stdout.lock().unwrap())) {
            use std::os::unix::process::ExitStatusExt;
            end_group();
            let _ = child.wait();
            break std::process::ExitStatus::from_raw(0);
        }
        if Instant::now() > deadline {
            end_group();
            let _ = child.wait();
            bail!("{what} took longer than {}s", timeout.as_secs());
        }
        sleep(Duration::from_millis(if answered.is_some() {
            20
        } else {
            100
        }));
    };
    drop(stdin);
    // What it wrote before exiting is read at once; output held open longer
    // is something it left running, which goes.
    let read = || stdout_reader.is_finished() && stderr_reader.is_finished();
    let lingering = (Instant::now() + LINGER).min(deadline);
    while !read() && Instant::now() < lingering {
        sleep(Duration::from_millis(10));
    }
    if !read() {
        end_group();
        let let_go = Instant::now() + LINGER;
        while !read() && Instant::now() < let_go {
            sleep(Duration::from_millis(10));
        }
    }
    let taken = |out: &Arc<Mutex<Vec<u8>>>| std::mem::take(&mut *out.lock().unwrap());
    Ok(std::process::Output {
        status,
        stdout: taken(&stdout),
        stderr: taken(&stderr),
    })
}

/// How long a command's output may stay open after it exits.
const LINGER: Duration = Duration::from_millis(500);

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
        let argv = wrap("/bin/sh", &global, &project, None, &agent);
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "override two arg with spaces\n"
        );
    }

    #[test]
    fn a_prompt_file_is_the_agents_last_argument_and_goes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("prompt");
        // Longer than tmux takes on a command line, with what a shell or tmux
        // would read otherwise.
        let prompt = format!(
            "fix it; $(echo no) `echo no` \"quoted\" {}",
            "x".repeat(40_000)
        );
        std::fs::write(&file, &prompt).unwrap();
        // An env file of the same name changes nothing.
        let global = dir.path().join("env");
        std::fs::write(&global, "roster_prompt=from-env\n").unwrap();
        let agent = ["sh", "-c", r#"printf '%s|%s' "$1" "$2""#, "x", "--"].map(String::from);
        let argv = wrap(
            "/bin/sh",
            &global,
            &dir.path().join("none"),
            Some(&file),
            &agent,
        );
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("--|{prompt}"));
        assert!(!file.exists());
        // One that can't be read stops the start, and says so.
        let argv = wrap(
            "/bin/sh",
            &global,
            &dir.path().join("none"),
            Some(&file),
            &agent,
        );
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert!(!out.status.success() && out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("preparing the environment failed"));
    }

    #[test]
    fn a_failing_env_file_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("env");
        std::fs::write(&global, "TOKEN=\"$(false)\"\n").unwrap();
        let agent = ["echo", "agent started"].map(String::from);
        let argv = wrap(
            "/bin/sh",
            &global,
            &dir.path().join("missing"),
            None,
            &agent,
        );
        let out = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("roster: preparing the environment failed")
        );
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    #[test]
    fn what_a_command_leaves_running_doesnt_hold_its_output_past_the_timeout() {
        // It exits at once, its output whole, but a process it started keeps
        // its stdout open: that one goes, rather than being waited for.
        let started = Instant::now();
        let out = run_with_timeout(
            sh("echo hi; sleep 5 & exit 0"),
            "it",
            Duration::from_secs(3),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
    }

    #[test]
    fn a_command_is_given_its_input_and_ended_once_it_has_answered() {
        // It reads to its end: its input closes once given.
        let out = converse(
            sh("cat"),
            "it",
            Duration::from_secs(3),
            Some(Input {
                text: b"one\n",
                answered: None,
            }),
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "one\n");
        // One that serves until its input ends is ended once it answers.
        let started = Instant::now();
        let answered = |out: &[u8]| out.ends_with(b"two\n");
        let out = converse(
            sh("cat; echo after"),
            "it",
            Duration::from_secs(3),
            Some(Input {
                text: b"two\n",
                answered: Some(&answered),
            }),
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "two\n");
        assert!(out.status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_timeout_ends_what_the_command_started_too() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("sleep 5 & echo $! > '{}'; sleep 5", pid_file.display());
        let started = Instant::now();
        let err = run_with_timeout(sh(&script), "it", Duration::from_secs(1)).unwrap_err();
        assert!(err.to_string().contains("took longer"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Gone, or a zombie no one has reaped yet (where nothing reaps them).
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit(") ")
                    .next()
                    .is_some_and(|s| !s.starts_with('Z'))
            })
        };
        let gone_by = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < gone_by {
            sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "what it started is still running");
    }
}
