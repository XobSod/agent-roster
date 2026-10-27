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
    use std::sync::{Arc, Mutex};
    // What it starts goes with it, on a timeout or when, its work done,
    // something it left running would hold its output open.
    own_session(&mut cmd);
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

/// `cmd` to run in a session of its own, so a process group of its own, for
/// what it starts to be ended with it. Without a terminal, too: something
/// asking on one (an env file's `read </dev/tty`) fails at once, where in a
/// group of the terminal's but not its foreground it would stop until the
/// timeout. Ctrl+C on roster doesn't reach it, though. So, where it can, it
/// ends with roster, as it would have in its group.
fn own_session(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
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
}

/// What the global env file makes, for several commands to run in, so it's
/// sourced once for all of them: its variables, and the shell that sourced
/// it, waiting while they're used. What the file started (a tunnel, say)
/// lives in that shell's process group until this goes, as in a session it
/// lives with the agent's pane: the shell waits on a pipe only roster
/// writes to (`_held`), and once it's closed, as this goes or roster does,
/// however, it ends its group.
pub struct Environment {
    pub vars: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    shell: std::process::Child,
    _held: std::os::fd::OwnedFd,
}

impl Drop for Environment {
    fn drop(&mut self) {
        // SAFETY: kill only sends a signal, to the group the shell leads.
        unsafe {
            libc::kill(-(self.shell.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.shell.wait();
    }
}

/// A pipe: the end to read from, and the end to write to, neither of them
/// given to a program roster starts (but as it's given).
fn waiting_pipe() -> Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills `fds` with two descriptors, which are then ours.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("can't make a pipe");
    }
    // SAFETY: just made, and owned by nothing else.
    Ok(unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(fds[0]),
            std::os::fd::OwnedFd::from_raw_fd(fds[1]),
        )
    })
}

/// Sources `global_env` with `shell` (as `wrap` does, roster's own
/// variables kept out), in `dir`, for at most `timeout`: the variables it
/// makes. An error says why it failed, naming the file.
pub fn environment(
    shell: &str,
    global_env: &Path,
    dir: &Path,
    timeout: Duration,
) -> Result<Environment> {
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    let file = global_env.display();
    // A mark of its own each time, between which the variables come: what
    // the file prints itself comes before it, and can't be it by chance.
    let mark = format!("roster-environment-{}", uuid::Uuid::new_v4().simple());
    // The shell and env by their paths, the file's PATH having neither,
    // maybe; it waits on the pipe roster holds (fd 3), then ends its group.
    let script =
        r#"printf '%s\0' "$1"; /usr/bin/env -0; printf '%s\0' "$1"; read -r _ <&3; kill -KILL 0"#;
    let argv: Vec<String> = ["/bin/sh", "-c", script, "roster-environment", &mark]
        .map(String::from)
        .into();
    let argv = wrap(shell, global_env, Path::new(""), None, &argv);
    let mut cmd = std::process::Command::new(&argv[0]);
    // Nothing to read for the file: what reads (`ssh` without -n) gets none.
    cmd.args(&argv[1..])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for var in crate::tmux::LEAKY_ENV {
        cmd.env_remove(var);
    }
    let (wait_on, held) = waiting_pipe()?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&wait_on);
    // In a session of its own, and not ended with the thread that started
    // it (a signal at its parent's death is, on Linux): its pipe tells it.
    // SAFETY: setsid, dup2 and fcntl are async-signal-safe, as `pre_exec`
    // needs.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || {
            if libc::setsid() == -1
                || (fd != 3 && libc::dup2(fd, 3) == -1)
                || libc::fcntl(3, libc::F_SETFD, 0) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut shell = cmd
        .spawn()
        .with_context(|| format!("can't source the env file {file}"))?;
    drop(wait_on);
    let (stdout, stderr) = (shell.stdout.take(), shell.stderr.take());
    // Ended as it goes, whatever happens.
    let mut env = Environment {
        vars: Vec::new(),
        shell,
        _held: held,
    };
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let out = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&out);
        std::thread::spawn(move || {
            let Some(mut pipe) = pipe else { return };
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                into.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        out
    };
    let stdout = drain(stdout.map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(stderr.map(|p| Box::new(p) as Box<dyn Read + Send>));
    let mark = format!("{mark}\0").into_bytes();
    let find = |bytes: &[u8], from: usize| {
        bytes
            .get(from..)?
            .windows(mark.len())
            .position(|w| w == mark)
            .map(|at| from + at)
    };
    let deadline = Instant::now() + timeout;
    loop {
        {
            let printed = stdout.lock().unwrap();
            if let Some(start) = find(&printed, 0)
                && let Some(end) = find(&printed, start + mark.len())
            {
                env.vars = vars(&printed[start + mark.len()..end]);
                return Ok(env);
            }
        }
        if env.shell.try_wait()?.is_some() {
            // What it said as it failed, once it has all been read.
            sleep(Duration::from_millis(50));
            let said = String::from_utf8_lossy(&stderr.lock().unwrap()).into_owned();
            match said.lines().rfind(|l| !l.trim().is_empty()) {
                Some(line) => bail!("the env file {file} failed: {}", line.trim()),
                None => bail!("the env file {file} failed"),
            }
        }
        if Instant::now() > deadline {
            bail!(
                "the env file {file} took longer than {}s",
                timeout.as_secs()
            );
        }
        sleep(Duration::from_millis(10));
    }
}

/// Variables as `env -0` prints them: `NAME=value`, each ended by a NUL.
fn vars(printed: &[u8]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    use std::os::unix::ffi::OsStrExt;
    printed
        .split(|&b| b == 0)
        .filter_map(|var| {
            let eq = var.iter().position(|&b| b == b'=')?;
            let (name, value) = (&var[..eq], &var[eq + 1..]);
            Some((
                std::ffi::OsStr::from_bytes(name).to_os_string(),
                std::ffi::OsStr::from_bytes(value).to_os_string(),
            ))
        })
        .collect()
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
    fn the_env_files_variables_are_read_once_whatever_it_prints() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("env");
        // It prints, a mark of the kind roster's is among it, and its PATH
        // has neither sh nor env.
        std::fs::write(
            &file,
            "echo noise\nprintf 'roster-environment-x\\0B=2\\0'\nA='one two'\nPATH=/nowhere\n",
        )
        .unwrap();
        let env = environment("/bin/sh", &file, dir.path(), Duration::from_secs(10)).unwrap();
        let var = |name: &str| {
            env.vars
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(var("A").as_deref(), Some("one two"));
        assert_eq!(var("PATH").as_deref(), Some("/nowhere"));
        assert_eq!(var("B"), None);
        // Failing, it says so, naming the file.
        std::fs::write(&file, "false\n").unwrap();
        let err = environment("/bin/sh", &file, dir.path(), Duration::from_secs(10))
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.starts_with(&format!("the env file {} failed: ", file.display())),
            "{err}"
        );
    }

    #[test]
    fn what_the_env_file_started_lives_while_its_variables_are_used() {
        let dir = tempfile::tempdir().unwrap();
        let (file, pid) = (dir.path().join("env"), dir.path().join("pid"));
        // A tunnel, say, left running, its output the shell's.
        std::fs::write(
            &file,
            format!("sleep 30 &\necho $! > '{}'\n", pid.display()),
        )
        .unwrap();
        // Made on a thread that's done before they're used, as an ask's.
        let (env_file, cwd) = (file.clone(), dir.path().to_path_buf());
        let env = std::thread::spawn(move || {
            environment("/bin/sh", &env_file, &cwd, Duration::from_secs(10))
        })
        .join()
        .unwrap()
        .unwrap();
        let pid = read_pid(&pid);
        std::thread::sleep(Duration::from_millis(700));
        assert!(alive(pid), "gone before its variables were used");
        drop(env);
        assert!(gone_soon(pid), "left running after them");
    }

    #[test]
    fn what_the_env_file_started_ends_with_roster_however_it_goes() {
        let dir = tempfile::tempdir().unwrap();
        let (file, pid) = (dir.path().join("env"), dir.path().join("pid"));
        std::fs::write(
            &file,
            format!("sleep 30 &\necho $! > '{}'\n", pid.display()),
        )
        .unwrap();
        let env = environment("/bin/sh", &file, dir.path(), Duration::from_secs(10)).unwrap();
        let pid = read_pid(&pid);
        // Roster gone without letting go of it (killed): its end of the
        // pipe closes, as the system closes a gone process's files.
        let env = std::mem::ManuallyDrop::new(env);
        // SAFETY: closes roster's end, which nothing uses after.
        unsafe { libc::close(std::os::fd::AsRawFd::as_raw_fd(&env._held)) };
        assert!(gone_soon(pid), "left running after roster");
    }

    #[test]
    fn an_env_file_reading_its_input_gets_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("env");
        std::fs::write(&file, "cat > /dev/null\nA=1\n").unwrap();
        let started = Instant::now();
        let env = environment("/bin/sh", &file, dir.path(), Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(env.vars.iter().any(|(k, v)| k == "A" && v == "1"));
    }

    fn read_pid(file: &Path) -> libc::pid_t {
        std::fs::read_to_string(file)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only asks whether the process is there.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Whether `pid` is gone within a few seconds.
    fn gone_soon(pid: libc::pid_t) -> bool {
        let by = Instant::now() + Duration::from_secs(5);
        while alive(pid) && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(20));
        }
        !alive(pid)
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
