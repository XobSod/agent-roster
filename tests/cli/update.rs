//! Updating: asking for the newest release, and installing it over the
//! roster that runs, from a server on 127.0.0.1 standing in for GitHub's.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use sha2::Digest;

use crate::common::{NO_RELEASES, Terminal, TestEnv};

/// What the server answers, by path (and query).
type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Serves `files` from a thread for as long as the tests run, a request
/// per connection; its address.
fn serve(files: Files) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).ok();
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|n| n > 0) && header != "\r\n" {
                header.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or_default();
            let found = files.lock().unwrap().get(path).cloned();
            let (status, body) = match found {
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", b"{\"message\": \"Not Found\"}".to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).ok();
            stream.write_all(&body).ok();
        }
    });
    address
}

/// The release archive ci/dist.sh would pack, with `roster` in it.
fn archive(roster: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let dir = "agent-roster-v9.9.9-linux-x86_64";
    for (name, text, mode) in [("LICENSE", "MIT", 0o644), ("roster", roster, 0o755)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, format!("{dir}/{name}"), text.as_bytes())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

/// GitHub's answer for the releases: `9.9.9` published with its archive
/// and checksum there, and `5.0.0` before it, both newer than the tests'
/// build; a pre-release and a draft newer still, and one older.
fn releases(url: &str) -> String {
    let name = "agent-roster-v9.9.9-linux-x86_64.tar.gz";
    serde_json::json!([
        {"tag_name": "v10.0.0", "draft": true, "prerelease": false, "body": "draft", "assets": []},
        {"tag_name": "v9.10.0-rc.1", "draft": false, "prerelease": true,
         "published_at": "2026-12-02T10:00:00Z", "body": "rc", "assets": []},
        {"tag_name": "v9.9.9", "draft": false, "prerelease": false,
         "published_at": "2026-12-01T10:00:00Z", "body": "## Nines\r\n- all of them",
         "assets": [
            {"name": name, "browser_download_url": format!("{url}/download/{name}")},
            {"name": format!("{name}.sha256"),
             "browser_download_url": format!("{url}/download/{name}.sha256")}
         ]},
        {"tag_name": "v0.0.1", "draft": false, "prerelease": false,
         "published_at": "2026-01-01T10:00:00Z", "body": "first", "assets": []},
        {"tag_name": "v5.0.0", "draft": false, "prerelease": false,
         "published_at": "2026-11-20T10:00:00Z", "body": "Five, **at last**.", "assets": []}
    ])
    .to_string()
}

/// `roster` run by `exe`, asking the server at `url`.
fn run(t: &TestEnv, exe: &Path, url: &str, args: &[&str], checkout: bool) -> Output {
    let mut cmd: Command = t.command_of(exe, args);
    cmd.env("ROSTER_UPDATE_URL", url);
    if checkout {
        cmd.env("ROSTER_UPDATE_CHECKOUT", "1");
    } else {
        cmd.env_remove("ROSTER_UPDATE_CHECKOUT");
    }
    // Copied just before, `exe` can still be open for writing in a process
    // another test is forking meanwhile: running it then fails as busy.
    for _ in 0..100 {
        match cmd.output() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => sleep(Duration::from_millis(20)),
            out => return out.unwrap(),
        }
    }
    cmd.output().unwrap()
}

fn said(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn update_installs_the_newest_release_over_the_roster_that_runs() {
    let Some(t) = TestEnv::new("update") else {
        return;
    };
    let files: Files = Default::default();
    let url = serve(files.clone());
    let script = "#!/bin/sh\necho 'roster 9.9.9'\n";
    let packed = archive(script);
    let sum: String = (sha2::Sha256::digest(&packed).iter())
        .map(|b| format!("{b:02x}"))
        .collect();
    let name = "agent-roster-v9.9.9-linux-x86_64.tar.gz";
    let checksum = format!("{sum}  {name}\n");
    {
        let mut files = files.lock().unwrap();
        files.insert("/releases?per_page=30".into(), releases(&url).into_bytes());
        files.insert(format!("/download/{name}"), packed);
        files.insert(
            format!("/download/{name}.sha256"),
            checksum.clone().into_bytes(),
        );
    }
    let real = Path::new(env!("CARGO_BIN_EXE_roster"));

    // Asked whether: the newest published, and the notes of each newer
    // than this, newest first, as plain text; drafts and pre-releases left
    // out.
    let notes = "roster 9.9.9 · 2026-12-01\nNines\n• all of them\n\nroster 5.0.0 · 2026-11-20\nFive, at last.\n";
    let out = run(&t, real, &url, &["update", "--check"], false);
    let (stdout, stderr) = said(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(
        stdout,
        format!("roster 9.9.9 is out: roster update\n\n{notes}")
    );
    // Kept, for --version to say after its own line.
    let out = t.run(&["--version"]);
    let (stdout, stderr) = said(&out);
    assert_eq!(stdout, format!("roster {}\n", version(real)));
    assert_eq!(stderr, "roster 9.9.9 is out: roster update\n");
    // Printed to a pipe, no one may have read them: they still open in the
    // TUI on their own.
    assert_eq!(shown(&t), None);
    // At a terminal, read, as the TUI's would be: they don't.
    let shell = t.shell_command().replace(NO_RELEASES, &url);
    let command = format!("{shell} update --check; echo CHECKED; sleep 600");
    let term = Terminal::start(&t, &command, (120, 30));
    term.wait_for("the notes printed", "CHECKED");
    drop(term);
    assert_eq!(shown(&t).as_deref(), Some("9.9.9"));

    // A copy of roster, never the one the tests run.
    let bin = t.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let copy = bin.join("roster");
    fs::copy(real, &copy).unwrap();
    let before = fs::metadata(&copy).unwrap();
    let untouched = |why: &str| {
        let now = fs::metadata(&copy).unwrap();
        assert_eq!(
            (now.ino(), now.len()),
            (before.ino(), before.len()),
            "{why}"
        );
        let left: Vec<_> = fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["roster"], "{why}");
    };

    // The tests' build isn't a release's, whatever its version says.
    let out = run(&t, &copy, &url, &["update"], false);
    let (_, stderr) = said(&out);
    assert!(!out.status.success());
    assert_eq!(
        stderr,
        "roster: not a release's build: update it as it was installed (git, cargo, a package)\n"
    );
    untouched("a build not a release's");

    // What was downloaded isn't what was published.
    let wrong = format!("{}  {name}\n", "0".repeat(64));
    files
        .lock()
        .unwrap()
        .insert(format!("/download/{name}.sha256"), wrong.into_bytes());
    let out = run(&t, &copy, &url, &["update"], true);
    let (_, stderr) = said(&out);
    assert!(!out.status.success());
    assert!(
        stderr.contains("doesn't match the checksum published with it"),
        "{stderr}"
    );
    untouched("a checksum not matching");

    // As published: in the copy's place.
    files
        .lock()
        .unwrap()
        .insert(format!("/download/{name}.sha256"), checksum.into_bytes());
    let out = run(&t, &copy, &url, &["update"], true);
    let (stdout, stderr) = said(&out);
    assert!(out.status.success(), "{stderr}");
    // What it brings first, then what was done.
    let installed = format!("{notes}\nInstalled roster 9.9.9 at ");
    assert!(stdout.starts_with(&installed), "{stdout}");
    assert!(stdout.contains("bin/roster"), "{stdout}");
    assert_eq!(fs::read_to_string(&copy).unwrap(), script);
    assert_eq!(version(&copy), "9.9.9");
    let left: Vec<_> = fs::read_dir(&bin)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["roster"]);

    // Nothing newer: said so, and --version says no more.
    let older = serde_json::json!([
        {"tag_name": "v0.0.1", "draft": false, "prerelease": false, "body": "first", "assets": []}
    ]);
    files.lock().unwrap().insert(
        "/releases?per_page=30".into(),
        older.to_string().into_bytes(),
    );
    let newest = format!("roster {} is the newest\n", version(real));
    let out = run(&t, real, &url, &["update", "--check"], false);
    let (stdout, stderr) = said(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout, newest);
    fs::copy(real, &copy).unwrap();
    let out = run(&t, &copy, &url, &["update"], true);
    let (stdout, stderr) = said(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout, newest);
    let (_, stderr) = said(&t.run(&["--version"]));
    assert_eq!(stderr, "");
    // The notes read before stay read.
    assert_eq!(shown(&t).as_deref(), Some("9.9.9"));

    // GitHub not answering is said.
    files.lock().unwrap().clear();
    let out = run(&t, real, &url, &["update", "--check"], false);
    let (_, stderr) = said(&out);
    assert!(!out.status.success());
    assert!(
        stderr.contains("can't ask GitHub for roster's releases"),
        "{stderr}"
    );
}

/// The newest release whose notes were shown, as the database has it.
fn shown(t: &TestEnv) -> Option<String> {
    let db = rusqlite::Connection::open(t.db()).unwrap();
    let key = "update:shown";
    db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

/// The version the roster at `exe` says it is.
fn version(exe: &Path) -> String {
    let out = Command::new(exe)
        .arg("--version")
        .env("XDG_STATE_HOME", "/nonexistent")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    said.trim_end().strip_prefix("roster ").unwrap().to_string()
}
