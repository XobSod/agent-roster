//! roster's private tmux server: one tmux session per agent
//! session, a generated config with no prefix and only the back key bound, and a
//! clean environment.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use std::os::unix::fs::OpenOptionsExt;

use anyhow::{Context, Result, bail};

/// Variables never passed into roster's tmux: the ones that make tmux think it's
/// nested, and the per-session ones of Claude Code and Codex, which leak in
/// when roster itself is run from inside one of them.
pub(crate) const LEAKY_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_TOOL_CALL_ID",
];

/// A line `type_line` couldn't type in: whether its text got into the pane
/// first (then only its Enter is missing), and why.
#[derive(Debug)]
pub struct Untyped {
    pub pasted: bool,
    pub error: anyhow::Error,
}

/// An argument of a tmux command, as tmux reads it: every call says which
/// it gives, so a value is never read as a format, a pattern, or the end of
/// a command where it isn't meant as one. Each is given as `render` makes
/// it, with a `;` at its end escaped too (`literal_end`), as tmux takes one
/// for the end of a command wherever it is.
#[derive(Clone, Copy, Debug)]
enum Arg<'a> {
    /// A word of roster's own (a command, a flag, an option's name), as the
    /// code writes it: only a `&'static str` makes one.
    Word(&'static str),
    /// A number roster gives (a size, a time, a line).
    Number(i64),
    /// A name roster made (a session's, `r-` and an id; a buffer's): never a
    /// user's, which `-s` and `-b` would read as formats.
    Name(&'a str),
    /// The session of that name exactly (`=name`), as a target.
    Session(&'a str),
    /// That session's pane (`=name:`), as a target.
    Pane(&'a str),
    /// Text tmux takes as it is: the command after `--` (an agent and its
    /// arguments, a first prompt), which it doesn't expand.
    Text(&'a OsStr),
    /// A variable the session gets (`-e`), which tmux doesn't expand either.
    Env(&'a str, &'a str),
    /// A path where tmux takes a format (`-c`, `load-buffer`'s file).
    Path(&'a Path),
    /// A path where tmux takes a pattern (`source-file`).
    Pattern(&'a Path),
    /// Text tmux shows (`display-message`), which it reads as a format and
    /// a time, then again as it draws it.
    Message(&'a str),
    /// A tmux format roster means as one: `-F`, or the status line's markup,
    /// which roster makes with what it shows escaped (`tui::status_line`).
    Format(&'a str),
}

impl From<&'static str> for Arg<'_> {
    fn from(word: &'static str) -> Self {
        Arg::Word(word)
    }
}

impl Arg<'_> {
    /// As tmux is given it, to read it as meant.
    fn render(self) -> std::ffi::OsString {
        let given = match self {
            Arg::Word(word) => word.into(),
            Arg::Number(n) => n.to_string().into(),
            Arg::Name(name) => name.into(),
            Arg::Session(name) => format!("={name}").into(),
            Arg::Pane(name) => format!("={name}:").into(),
            Arg::Text(text) => text.to_os_string(),
            Arg::Env(name, value) => format!("{name}={value}").into(),
            Arg::Path(path) => literal_path(path),
            Arg::Pattern(path) => glob_literal(path.as_os_str()),
            Arg::Message(text) => message_text(text).into(),
            Arg::Format(format) => format.into(),
        };
        literal_end(&given)
    }
}

/// State of the pane that runs an agent.
#[derive(Clone, Debug)]
pub struct Pane {
    pub dead: bool,
    pub dead_status: Option<i32>,
    /// The signal that ended it (tmux gives no status then).
    pub dead_signal: Option<i32>,
    pub pid: Option<i32>,
    /// The window's size.
    pub size: (u16, u16),
    /// Clients attached to its session.
    pub clients: u32,
    /// The program draws full screen: in the terminal's alternate screen, which
    /// keeps no scrollback.
    pub alternate: bool,
    /// The terminal title the program set (tmux's default is the host name).
    pub title: String,
}

pub struct Tmux {
    socket: String,
    conf: PathBuf,
}

/// The running server, for diagnostics.
#[derive(Clone, Debug)]
pub struct Server {
    pub pid: i64,
    pub version: String,
    /// Unix time in seconds.
    pub started: i64,
    pub socket_path: String,
}

/// One pane with everything diagnostics show about it.
#[derive(Clone, Debug)]
pub struct PaneDetail {
    pub session: String,
    pub clients: u32,
    pub dead: bool,
    pub dead_status: Option<i32>,
    /// The signal that killed it, if one did (tmux 3.3+).
    pub dead_signal: Option<i32>,
    pub pid: Option<i32>,
    pub command: String,
    pub width: u16,
    pub height: u16,
    pub history: u64,
}

impl Tmux {
    pub fn new(socket: String, conf: PathBuf) -> Tmux {
        Tmux { socket, conf }
    }

    /// Writes the generated config, which tmux reads when the server starts,
    /// and has a running server read it, when it changed (including edits to
    /// the user's tmux.conf). Some(error) if the server didn't take it (the
    /// user's tmux.conf has a mistake, say), what tmux said on one line. Said
    /// once, not tried again: that would run the user's file again, adding
    /// twice what it adds; a fix changes the file, which is reloaded then. An
    /// error writing it is an error.
    pub fn apply_config(
        &self,
        detach_key: &str,
        user_conf: &Path,
        status_line: bool,
    ) -> Result<Option<anyhow::Error>> {
        let text = config_text(detach_key, user_conf, status_line);
        if fs::read(&self.conf).ok().as_deref() == Some(&text[..]) {
            return Ok(None);
        }
        self.write_config(&text)?;
        Ok(self
            .reload_config()
            .err()
            .map(|e| anyhow::anyhow!(crate::util::one_line(&format!("{e:#}")))))
    }

    /// For a server that just started: what tmux found wrong in the config it
    /// started with (the user's tmux.conf, or a file that sources). It says
    /// nothing then, but keeps it, and the next `source-file` prints it, so
    /// sourcing nothing (`-n /dev/null`) asks without running the user's file
    /// again, which would add twice what it adds. Said once, as
    /// `apply_config`'s.
    pub fn check_config(&self) -> Option<anyhow::Error> {
        let flushed = self.run(&["source-file".into(), "-n".into(), "/dev/null".into()]);
        match flushed {
            Ok(said) if said.trim().is_empty() => None,
            Ok(said) => Some(anyhow::anyhow!("tmux: {}", crate::util::one_line(&said))),
            Err(e) if is_missing(&e) => None,
            Err(e) => Some(e),
        }
    }

    /// Writes `text` as the generated config, whole, so a server starting
    /// meanwhile never reads half of it.
    fn write_config(&self, text: &[u8]) -> Result<()> {
        let part = self
            .conf
            .with_extension(format!("{}.part", std::process::id()));
        fs::write(&part, text)
            .and_then(|()| fs::rename(&part, &self.conf))
            .with_context(|| format!("can't write {}", self.conf.display()))
    }

    /// Makes a running server re-read the generated config.
    fn reload_config(&self) -> Result<()> {
        match self.run(&["source-file".into(), Arg::Pattern(&self.conf)]) {
            Err(e) if is_missing(&e) => Ok(()),
            other => other.map(drop),
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        // -u: in a locale without UTF-8 (LANG unset, say), tmux would otherwise
        // replace tabs and non-ASCII characters in what it prints with `_`, so
        // listing panes, their titles and captured screens would all break.
        cmd.arg("-u")
            .arg("-L")
            .arg(&self.socket)
            .arg("-f")
            .arg(&self.conf);
        for var in LEAKY_ENV {
            cmd.env_remove(var);
        }
        cmd
    }

    /// Runs a tmux command, each argument given as its kind says (`Arg`).
    fn run(&self, args: &[Arg<'_>]) -> Result<String> {
        self.run_args(args.iter().map(|a| a.render()))
    }

    /// Runs tmux commands one after the other, in one call: only the `;`
    /// between them is tmux's own.
    fn run_all(&self, commands: &[&[Arg<'_>]]) -> Result<String> {
        let mut args = Vec::new();
        for (i, command) in commands.iter().enumerate() {
            if i > 0 {
                args.push(std::ffi::OsString::from(";"));
            }
            args.extend(command.iter().map(|a| a.render()));
        }
        self.run_args(args.into_iter())
    }

    fn run_args(&self, args: impl Iterator<Item = std::ffi::OsString>) -> Result<String> {
        let mut cmd = self.command();
        cmd.args(args);
        let out = cmd.output().context("can't run tmux (is it installed?)")?;
        if !out.status.success() {
            // A config file's mistakes come on stdout (`source-file`).
            let said = match String::from_utf8_lossy(&out.stderr).trim() {
                "" => String::from_utf8_lossy(&out.stdout).trim().to_string(),
                err => err.to_string(),
            };
            bail!("tmux: {said}");
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Panes of roster's sessions by tmux session name; empty if the server isn't running.
    pub fn panes(&self) -> Result<HashMap<String, Pane>> {
        let Some(out) = self.query(&[
            "list-panes".into(),
            "-a".into(),
            "-F".into(),
            Arg::Format(
                "#{session_name}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\t\
                 #{pane_pid}\t#{window_width}\t#{window_height}\t#{session_attached}\t\
                 #{alternate_on}\t#{pane_title}",
            ),
        ])?
        else {
            return Ok(HashMap::new());
        };
        let mut panes = HashMap::new();
        for line in out.lines() {
            // The title comes last and may itself contain tabs.
            let fields: Vec<&str> = line.splitn(10, '\t').collect();
            if let [
                name,
                dead,
                status,
                signal,
                pid,
                width,
                height,
                clients,
                alternate,
                title,
            ] = fields[..]
            {
                panes.entry(name.to_string()).or_insert(Pane {
                    dead: dead == "1",
                    dead_status: status.parse().ok(),
                    dead_signal: signal.parse().ok(),
                    pid: pid.parse().ok(),
                    size: (width.parse().unwrap_or(0), height.parse().unwrap_or(0)),
                    clients: clients.parse().unwrap_or(0),
                    alternate: alternate == "1",
                    title: title.to_string(),
                });
            }
        }
        Ok(panes)
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    pub fn conf_path(&self) -> &Path {
        &self.conf
    }

    /// Runs a query; None if the server isn't running.
    fn query(&self, args: &[Arg<'_>]) -> Result<Option<String>> {
        match self.run(args) {
            Err(e) if is_missing(&e) => Ok(None),
            other => other.map(Some),
        }
    }

    pub fn server(&self) -> Result<Option<Server>> {
        let Some(out) = self.query(&[
            "list-sessions".into(),
            "-F".into(),
            Arg::Format("#{pid}\t#{version}\t#{start_time}\t#{socket_path}"),
        ])?
        else {
            return Ok(None);
        };
        Ok(out.lines().next().and_then(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            match f[..] {
                [pid, version, started, path] => Some(Server {
                    pid: pid.parse().ok()?,
                    version: version.to_string(),
                    started: started.parse().unwrap_or(0),
                    socket_path: path.to_string(),
                }),
                _ => None,
            }
        }))
    }

    pub fn pane_details(&self) -> Result<Vec<PaneDetail>> {
        let format = "#{session_name}\t#{session_attached}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\t#{pane_pid}\t#{pane_current_command}\t#{pane_width}\t#{pane_height}\t#{history_size}";
        let Some(out) = self.query(&[
            "list-panes".into(),
            "-a".into(),
            "-F".into(),
            Arg::Format(format),
        ])?
        else {
            return Ok(Vec::new());
        };
        Ok(out
            .lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.split('\t').collect();
                match f[..] {
                    [
                        session,
                        clients,
                        dead,
                        status,
                        signal,
                        pid,
                        command,
                        width,
                        height,
                        history,
                    ] => Some(PaneDetail {
                        session: session.to_string(),
                        clients: clients.parse().unwrap_or(0),
                        dead: dead == "1",
                        dead_status: status.parse().ok(),
                        dead_signal: signal.parse().ok(),
                        pid: pid.parse().ok(),
                        command: command.to_string(),
                        width: width.parse().unwrap_or(0),
                        height: height.parse().unwrap_or(0),
                        history: history.parse().unwrap_or(0),
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    /// Whether `key` is bound to detach-client in the root table.
    pub fn detach_bound(&self, key: &str) -> Result<Option<bool>> {
        let Some(out) = self.query(&["list-keys".into(), "-T".into(), "root".into()])? else {
            return Ok(None);
        };
        Ok(Some(out.lines().any(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.contains(&"detach-client") && words.get(3) == Some(&key)
        })))
    }

    /// The server's global environment variables (names), e.g. to spot TMUX leaks.
    pub fn global_env(&self) -> Result<Option<Vec<String>>> {
        let Some(out) = self.query(&["show-environment".into(), "-g".into()])? else {
            return Ok(None);
        };
        // Removed variables show as "-NAME".
        Ok(Some(
            out.lines()
                .filter(|l| !l.starts_with('-'))
                .filter_map(|l| l.split('=').next().map(String::from))
                .collect(),
        ))
    }

    /// Starts a detached session running `argv` (no shell in between, so arguments
    /// such as JSON need no quoting).
    pub fn new_session(
        &self,
        name: &str,
        workdir: &Path,
        size: (u16, u16),
        env: &[(&str, String)],
        argv: &[String],
    ) -> Result<()> {
        let mut args = vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            Arg::Name(name),
            "-x".into(),
            Arg::Number(size.0.into()),
            "-y".into(),
            Arg::Number(size.1.into()),
        ];
        args.extend(started(workdir, env, argv));
        self.run(&args).map(drop)
    }

    /// Replaces the (dead) pane's process with `argv`.
    pub fn respawn(
        &self,
        name: &str,
        workdir: &Path,
        env: &[(&str, String)],
        argv: &[String],
    ) -> Result<()> {
        let mut args = vec![
            "respawn-pane".into(),
            "-k".into(),
            "-t".into(),
            Arg::Pane(name),
        ];
        args.extend(started(workdir, env, argv));
        self.run(&args).map(drop)
    }

    /// Puts a line into the pane as a bracketed paste, then presses Enter. A plain
    /// carriage return right after a burst of typed text is taken by Claude as
    /// part of a paste (a newline, not a submit); the paste markers and a pause
    /// make the Enter a key press. Programs that don't use bracketed paste get the
    /// plain text.
    /// Failing, it says whether the text got into the pane (`Untyped::pasted`).
    pub fn type_line(&self, name: &str, text: &str) -> Result<(), Untyped> {
        let buffer = format!("roster-{name}");
        let unpasted = |error| Untyped {
            pasted: false,
            error,
        };
        // Through a file of roster's own: tmux takes a command line of about
        // 16 KB at most (`command too long`), which a prompt can outgrow, and
        // a file needs no escaping (an argument ending in `;` would).
        let file = self
            .conf
            .with_file_name(format!("prompt-{name}.{}", std::process::id()));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
            .with_context(|| format!("can't write {}", file.display()));
        let loaded = written.and_then(|()| {
            self.run(&[
                "load-buffer".into(),
                "-b".into(),
                Arg::Name(&buffer),
                Arg::Path(&file),
            ])
        });
        let _ = std::fs::remove_file(&file);
        loaded.map_err(unpasted)?;
        self.run(&[
            "paste-buffer".into(),
            "-p".into(),
            "-d".into(),
            "-b".into(),
            Arg::Name(&buffer),
            "-t".into(),
            Arg::Pane(name),
        ])
        .map_err(unpasted)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        self.run(&[
            "send-keys".into(),
            "-t".into(),
            Arg::Pane(name),
            "Enter".into(),
        ])
        .map(drop)
        .map_err(|error| Untyped {
            pasted: true,
            error,
        })
    }

    /// Shows `text` to whoever is attached to session `name`, for a while,
    /// where tmux shows its own messages. Shown if it can be, else not.
    pub fn say(&self, name: &str, text: &str) {
        let _ = self.run(&[
            "display-message".into(),
            "-d".into(),
            Arg::Number(10_000),
            "-t".into(),
            Arg::Pane(name),
            Arg::Message(text),
        ]);
    }

    /// Sizes the session's window while no one is attached. Unsetting
    /// `window-size` afterwards (resize-window sets it to manual) leaves the size
    /// as it is until a client attaches, which then sizes the window to itself
    /// as usual (verified on tmux 3.5a).
    pub fn resize(&self, name: &str, size: (u16, u16)) -> Result<()> {
        self.run_all(&[
            &[
                "resize-window".into(),
                "-t".into(),
                Arg::Pane(name),
                "-x".into(),
                Arg::Number(size.0.into()),
                "-y".into(),
                Arg::Number(size.1.into()),
            ],
            &[
                "set-option".into(),
                "-w".into(),
                "-u".into(),
                "-t".into(),
                Arg::Pane(name),
                "window-size".into(),
            ],
        ])
        .map(drop)
    }

    /// What the status line shows while attached to the session
    /// (`config_text`): tmux markup for its left and right.
    pub fn label(&self, name: &str, left: &str, right: &str) -> Result<()> {
        let set = |option, markup| {
            [
                "set-option".into(),
                "-t".into(),
                Arg::Pane(name),
                Arg::Word(option),
                Arg::Format(markup),
            ]
        };
        self.run_all(&[&set("@roster_left", left), &set("@roster_right", right)])
            .map(drop)
    }

    pub fn kill_session(&self, name: &str) -> Result<()> {
        match self.run(&["kill-session".into(), "-t".into(), Arg::Session(name)]) {
            Err(e) if is_missing(&e) => Ok(()),
            other => other.map(drop),
        }
    }

    /// The visible screen plus `history` lines above it, with colours as escape
    /// sequences if `ansi`.
    pub fn capture(&self, name: &str, ansi: bool, history: u32) -> Result<String> {
        let mut args = vec![
            "capture-pane".into(),
            "-p".into(),
            "-t".into(),
            Arg::Pane(name),
        ];
        if history > 0 {
            args.extend(["-S".into(), Arg::Number(-i64::from(history))]);
        }
        if ansi {
            args.push("-e".into());
        }
        self.run(&args)
    }

    /// Attaches the current terminal until the user detaches (or the session ends).
    /// The client draws through the terminal it opens itself; its stdout only
    /// gets the message it prints on leaving ("[detached (from session …)]"),
    /// which would pile up behind the TUI, one per attach. Errors still show.
    pub fn attach(&self, name: &str) -> Result<ExitStatus> {
        self.command()
            .args(["attach-session".into(), "-t".into(), Arg::Session(name)].map(Arg::render))
            .stdout(std::process::Stdio::null())
            .status()
            .context("can't run tmux (is it installed?)")
    }
}

/// `text` as `display-message` shows it as it is: not a format (`#{…}`),
/// nor a time (`%H`). tmux reads `#`s twice, as a format and as it draws
/// it, so each run of them is four times as long, but for one before a `[`
/// (a style), which both keep as it is: twice.
fn message_text(text: &str) -> String {
    let doubled = hash_runs(text.as_bytes(), 2, 4);
    String::from_utf8_lossy(&doubled).replace('%', "%%")
}

/// `bytes` with each run of `#`s `times` as long, or `before_style` times
/// before a `[`.
fn hash_runs(bytes: &[u8], before_style: usize, times: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(start) = rest.iter().position(|&b| b == b'#') {
        out.extend_from_slice(&rest[..start]);
        let run = rest[start..].iter().take_while(|&&b| b == b'#').count();
        rest = &rest[start + run..];
        let n = if rest.first() == Some(&b'[') {
            before_style
        } else {
            times
        };
        out.resize(out.len() + run * n, b'#');
    }
    out.extend_from_slice(rest);
    out
}

/// `path` as `source-file` takes it: a pattern, so `[`, `*` and `?` (and
/// `\`) in a directory's name are escaped.
fn glob_literal(path: &OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut out = Vec::with_capacity(path.len());
    for &b in path.as_bytes() {
        if matches!(b, b'\\' | b'[' | b']' | b'*' | b'?') {
            out.push(b'\\');
        }
        out.push(b);
    }
    std::ffi::OsString::from_vec(out)
}

/// `path` where tmux takes a format (`-c`, `load-buffer`'s file): each run of
/// `#`s doubled, else a directory named with `#S` would be the session's
/// name, and with `#(…)` a command tmux runs; but for a run before a `[`
/// (a style), which tmux keeps as it is.
fn literal_path(path: &Path) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    std::ffi::OsString::from_vec(hash_runs(path.as_os_str().as_bytes(), 1, 2))
}

/// `arg` as tmux should read it, as text: tmux takes an argument ending in
/// `;` for the end of a command and drops the `;`, a lone one too, where
/// `\;` keeps it (`Tmux::run_all` puts tmux's own between commands).
fn literal_end(arg: &OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    match arg.as_bytes().strip_suffix(b";") {
        Some(rest) => std::ffi::OsString::from_vec([rest, b"\\;"].concat()),
        None => arg.to_os_string(),
    }
}

/// The rest of a command that starts a pane: where (`-c`), its variables
/// (`-e`), then its command, after `--`.
fn started<'a>(
    workdir: &'a Path,
    env: &'a [(&'a str, String)],
    argv: &'a [String],
) -> Vec<Arg<'a>> {
    let mut args = vec!["-c".into(), Arg::Path(workdir)];
    for (name, value) in env {
        args.extend(["-e".into(), Arg::Env(name, value)]);
    }
    args.push("--".into());
    args.extend(argv.iter().map(|a| Arg::Text(OsStr::new(a))));
    args
}

/// Whether tmux said there's no such server or session: at the start of what
/// it said, so a config file's mistake that mentions one isn't taken for it.
/// A server that exited while answering (killed, or its last session gone)
/// is no server either.
fn is_missing(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    let said = msg.strip_prefix("tmux: ").unwrap_or(&msg);
    [
        "can't find session",
        "no server running",
        "error connecting",
        "server exited unexpectedly",
    ]
    .iter()
    .any(|m| said.starts_with(m))
}

/// The generated config, as bytes: the user's tmux.conf's path written in a
/// double-quoted string, a byte that isn't printable ASCII as its number.
fn config_text(detach_key: &str, user_conf: &Path, status_line: bool) -> Vec<u8> {
    // Noted in the text so that editing the user's file changes it.
    let modified = fs::metadata(user_conf)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or("absent".to_string(), |d| d.as_nanos().to_string());
    // While attached, where you are, like the preview's header (set before
    // each attach, `Tmux::label`); the back key stands in for one not set yet.
    // Its spans bring their own colours; the base is theme::MUTED.
    let status = if status_line {
        format!(
            "set -g status on
set -g status-position top
set -g status-style 'bg=default,fg=#7d8296'
set -g status-left-length 300
set -g status-right-length 300
set -g status-left '#{{@roster_left}}'
set -g status-right '#{{?#{{@roster_right}},#{{@roster_right}},{detach_key} back }}'
set -g window-status-format ''
set -g window-status-current-format ''
set -g window-status-separator ''"
        )
    } else {
        "set -g status off".to_string()
    };
    // Shown in a comment, which a newline would end.
    let shown: String = (user_conf.display().to_string().chars())
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let text = format!(
        "# Generated by roster on every run; don't edit. Your own additions go in
# {shown} (see examples/tmux.conf; last modified: {modified}).

# No prefix and no default bindings: every key goes to the agent...
set -g prefix None
set -g prefix2 None
unbind -aq -T prefix
unbind -aq -T root
# ...except the back key.
bind -n {detach_key} detach-client

{status}
set -g mouse off
set -s escape-time 10
set -s extended-keys on
# Modified keys (Shift+Enter, say) in the format pi asks for, which Claude reads
# too; tmux before 3.5 doesn't know the option, and -q lets it pass.
set -sq extended-keys-format csi-u
set -s set-clipboard on
set -g history-limit 50000
set -g remain-on-exit on

source-file -q \""
    );
    // The path a pattern to source-file, in a string where `\`, `"` and `$`
    // are a config file's own. A control character, or a byte that isn't
    // ASCII, as its number (`\377`): one that isn't UTF-8 tmux reads
    // written as it is only as it starts, not on a reload (it splits the
    // line at a `;` after it, or finds no file), and a newline would end
    // the line.
    let mut text = text.into_bytes();
    for &b in glob_literal(user_conf.as_os_str()).as_encoded_bytes() {
        match b {
            b'\\' | b'"' | b'$' => text.extend([b'\\', b]),
            0..=0x1f | 0x7f..=0xff => text.extend(format!("\\{b:03o}").bytes()),
            _ => text.push(b),
        }
    }
    text.extend_from_slice(b"\"\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_server_or_session_counts_as_missing() {
        let missing = |said: &str| is_missing(&anyhow::anyhow!("tmux: {said}"));
        assert!(missing("no server running on /tmp/tmux-1000/agent-roster"));
        assert!(missing(
            "error connecting to /tmp/tmux-1000/agent-roster (No such file or directory)"
        ));
        assert!(missing("can't find session: r-1234"));
        assert!(missing("server exited unexpectedly"));
        // A config file's mistake that mentions one (tmux says those on stdout).
        assert!(!missing(
            "/home/u/.config/agent-roster/tmux.conf:3: can't find session: x"
        ));
    }

    #[test]
    fn config_binds_only_the_detach_key_and_sources_user_file() {
        let text = config_text(
            "C-q",
            Path::new("/home/u/.config/agent-roster/tmux.conf"),
            false,
        );
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("bind -n C-q detach-client"));
        assert!(text.contains("set -g status off"));
        assert!(text.contains("unbind -aq -T root"));
        assert!(text.contains("set -sq extended-keys-format csi-u"));
        assert!(text.contains("source-file -q \"/home/u/.config/agent-roster/tmux.conf\""));
    }

    #[test]
    fn config_escapes_the_user_path() {
        let text = config_text("C-q", Path::new("/we\"ird/$HOME/tmux.conf"), true);
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("set -g status on") && text.contains("#{@roster_left}"));
        assert!(text.contains("C-q back"));
        assert!(text.contains(r#"source-file -q "/we\"ird/\$HOME/tmux.conf""#));
    }

    #[test]
    fn each_kind_of_argument_is_given_as_tmux_reads_it() {
        let given = |a: Arg| a.render().into_string().unwrap();
        assert_eq!(given("list-panes".into()), "list-panes");
        assert_eq!(given(Arg::Number(-200)), "-200");
        assert_eq!(given(Arg::Name("r-1a2b")), "r-1a2b");
        assert_eq!(given(Arg::Session("r-1")), "=r-1");
        assert_eq!(given(Arg::Pane("r-1")), "=r-1:");
        // As it is, but for a `;` at its end, which would end the command.
        assert_eq!(
            given(Arg::Text(OsStr::new("fix #S; now;"))),
            "fix #S; now\\;"
        );
        assert_eq!(given(Arg::Env("K", "a#{b};")), "K=a#{b}\\;");
        // Read as a format, a pattern, or shown text.
        assert_eq!(
            given(Arg::Path(Path::new("/w/C#S a#[b;"))),
            "/w/C##S a#[b\\;"
        );
        assert_eq!(
            given(Arg::Pattern(Path::new(r"/w/b[1]*?\"))),
            r"/w/b\[1\]\*\?\\"
        );
        assert_eq!(given(Arg::Message("50% ## #[x]")), "50%% ######## ##[x]");
        // A format roster means, as it is.
        assert_eq!(given(Arg::Format("#{pane_pid}")), "#{pane_pid}");
    }

    /// A tmux server of a test's own, its config in `dir`, ended with it.
    struct Scratch {
        tmux: Tmux,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Where its socket is, before it's gone: tmux leaves the file.
            let socket = (self.tmux.command())
                .args(["display-message", "-p", "#{socket_path}"])
                .output()
                .ok()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|path| path.contains("roster-scratch-"));
            let _ = self.tmux.command().arg("kill-server").output();
            if let Some(socket) = socket {
                let _ = fs::remove_file(socket);
            }
        }
    }

    /// The bytes of a file a pane writes, once it has.
    fn written(file: &Path) -> Vec<u8> {
        let by = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match fs::read(file) {
                Ok(bytes) if !bytes.is_empty() => return bytes,
                _ if std::time::Instant::now() > by => panic!("{} never written", file.display()),
                _ => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }

    #[test]
    fn what_roster_gives_tmux_arrives_as_written() {
        use std::os::unix::ffi::OsStrExt;
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Everything tmux could read as a format, a style, a command, a
        // pattern, a time or a command's end; and with a byte that isn't
        // UTF-8, where tmux takes one (not in a file it sources once
        // running, where it splits the line there, or finds no file).
        let named = b"d #S #(echo no) #[b] #{pid} (a),b \\ * ? %H ;";
        let conf_dir = dir.path().join(OsStr::from_bytes(named));
        let odd = dir
            .path()
            .join(OsStr::from_bytes(&[&named[..], b"\xff;"].concat()));
        fs::create_dir(&conf_dir).unwrap();
        fs::create_dir(&odd).unwrap();
        let s = Scratch {
            tmux: Tmux::new(
                format!("roster-scratch-tm-{}", std::process::id()),
                conf_dir.join("tmux.conf"),
            ),
        };
        let t = &s.tmux;
        // The user's tmux.conf, sourced from a path that's a pattern to tmux
        // (and a byte that isn't UTF-8, and a newline), as the server starts,
        // then as it reloads the generated config.
        let user_dir = dir.path().join(OsStr::from_bytes(
            &[&named[..], b" \xff;x\nn\xc3\xa9 \xff1"].concat(),
        ));
        fs::create_dir(&user_dir).unwrap();
        let user = user_dir.join("user.conf");
        fs::write(&user, "set -g @user loaded\n").unwrap();
        let applied = |t: &Tmux, key| {
            t.apply_config(key, &user, true)
                .unwrap()
                .map(|e| e.to_string())
        };
        assert_eq!(applied(t, "C-q"), None);
        let shown = |t: &Tmux, option: &'static str| {
            t.run(&["show-options".into(), "-gv".into(), option.into()])
                .unwrap()
                .trim()
                .to_string()
        };
        // Started in that directory, with a variable and a command tmux
        // doesn't expand.
        let out = dir.path().join("out");
        let script = r#"pwd > "$1.cwd"; printf %s "$ROSTER_T" > "$1.env"; printf %s "$2" > "$1.arg"; exec cat > "$1.typed""#;
        let given = "#S #(echo no) %H #{pid};";
        let argv = |n: &str| {
            ["sh", "-c", script, "sh"]
                .iter()
                .map(|a| a.to_string())
                .chain([format!("{}{n}", out.display()), given.to_string()])
                .collect::<Vec<String>>()
        };
        let env = [("ROSTER_T", "v #S #(echo no);".to_string())];
        t.new_session("r-t1", &odd, (80, 24), &env, &argv("1"))
            .unwrap();
        let cwd = [odd.as_os_str().as_bytes(), b"\n"].concat();
        assert_eq!(written(&dir.path().join("out1.cwd")), cwd);
        assert_eq!(written(&dir.path().join("out1.env")), b"v #S #(echo no);");
        assert_eq!(written(&dir.path().join("out1.arg")), given.as_bytes());
        assert_eq!(shown(t, "@user"), "loaded");
        // A line typed in, through a file beside the config (load-buffer);
        // and a buffer loaded from the odd directory itself.
        t.type_line("r-t1", "typed #S ; %H #(echo no);").unwrap();
        assert_eq!(
            written(&dir.path().join("out1.typed")),
            b"typed #S ; %H #(echo no);\n"
        );
        let file = odd.join("buffer");
        fs::write(&file, "loaded #S;").unwrap();
        let buffer = |t: &Tmux, verb: &'static str| {
            let mut args = vec![verb.into(), "-b".into(), Arg::Name("t")];
            if verb == "load-buffer" {
                args.push(Arg::Path(&file));
            }
            t.run(&args).unwrap()
        };
        buffer(t, "load-buffer");
        assert_eq!(buffer(t, "show-buffer"), "loaded #S;");
        // Started again, by respawning its pane.
        t.respawn("r-t1", &odd, &env, &argv("2")).unwrap();
        assert_eq!(written(&dir.path().join("out2.cwd")), cwd);
        // Reloaded, the user's file is read again.
        fs::write(&user, "set -g @user reloaded\n").unwrap();
        assert_eq!(applied(t, "C-w"), None);
        assert_eq!(shown(t, "@user"), "reloaded");
        // A message: the format step halves each run of #s but one before a
        // `[`, which it keeps, and reads %% as %; what it leaves is twice
        // each run, which the drawing halves to what was written.
        let message = "50% #S ## #(echo no) #[bold]x #{pane_pid} ; end;";
        let formatted = t
            .run(&[
                "display-message".into(),
                "-p".into(),
                "-t".into(),
                Arg::Pane("r-t1"),
                Arg::Message(message),
            ])
            .unwrap();
        let drawn = hash_runs(message.as_bytes(), 2, 2);
        assert_eq!(formatted.trim_end_matches('\n').as_bytes(), drawn);
        // The status line's markup, kept as it's set, and not read again.
        let markup = "##S #[fg=red]x ##(echo no) ;";
        t.label("r-t1", markup, "right").unwrap();
        let left = t
            .run(&[
                "display-message".into(),
                "-p".into(),
                "-t".into(),
                Arg::Pane("r-t1"),
                Arg::Format("#{@roster_left}"),
            ])
            .unwrap();
        assert_eq!(left.trim_end_matches('\n'), markup);
    }

    #[test]
    fn a_message_is_shown_as_it_is() {
        assert_eq!(message_text("50% of #{pane}"), "50%% of ####{pane}");
        assert_eq!(message_text("x ## y #[bg=red]"), "x ######## y ##[bg=red]");
    }

    #[test]
    fn a_path_is_never_a_format() {
        let path = |p: &str| literal_path(Path::new(p)).into_string().unwrap();
        assert_eq!(path("/w/C#Sharp #(id)"), "/w/C##Sharp ##(id)");
        assert_eq!(path("/w/plain"), "/w/plain");
        // A run before a `[` tmux keeps as it is.
        assert_eq!(path("/w/a#[b ##[c d##"), "/w/a#[b ##[c d####");
    }

    #[test]
    fn a_config_path_of_any_bytes_is_written_as_numbers() {
        use std::os::unix::ffi::OsStrExt;
        let user = Path::new(OsStr::from_bytes(b"/w/\xff;x\nn\xc3\xa9/tmux.conf"));
        let text = config_text("C-q", user, false);
        let text = String::from_utf8(text).unwrap();
        assert!(
            text.contains(r#"source-file -q "/w/\377;x\012n\303\251/tmux.conf""#),
            "{text}"
        );
        // Shown in its comment, a newline doesn't end it.
        assert!(text.contains("# /w/\u{fffd};x?né/tmux.conf "), "{text}");
    }

    #[test]
    fn a_config_path_is_never_a_pattern() {
        let glob = |p: &str| glob_literal(OsStr::new(p)).into_string().unwrap();
        assert_eq!(glob("/w/br[1]x/*?.conf"), r"/w/br\[1\]x/\*\?.conf");
        assert_eq!(glob(r"/w/a\b"), r"/w/a\\b");
        let text =
            String::from_utf8(config_text("C-q", Path::new("/w/br[1]x/tmux.conf"), false)).unwrap();
        assert!(
            text.contains(r#"source-file -q "/w/br\\[1\\]x/tmux.conf""#),
            "{text}"
        );
    }

    #[test]
    fn a_last_semicolon_stays_text() {
        let end = |a: &str| literal_end(OsStr::new(a)).into_string().unwrap();
        assert_eq!(end("Fix the parser;"), "Fix the parser\\;");
        // A lone `;` too: an argument, not the end of a command.
        assert_eq!(
            (end(";"), end("a;b"), end("")),
            ("\\;".into(), "a;b".into(), "".into())
        );
    }
}
