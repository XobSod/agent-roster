//! Installing a release over the binary that runs, where it is: what's
//! downloaded matches the checksum published with it and has run before it
//! takes the binary's place, in one rename, so a roster starting meanwhile
//! finds the old or the new, never part of either. A running program's file
//! can't be written over, but can be renamed over: what runs goes on with
//! the file it started from.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::Release;
use super::github::{self, Published};

/// Set as ci/dist.sh builds it: this build is a release's. Its version can't
/// tell, as a checkout on a release's tag, a source archive and a package
/// built from one all say what a release says, and each is for what made it
/// to update.
const RELEASE_BUILD: Option<&str> = option_env!("ROSTER_RELEASE_BUILD");

/// Lets any other build update as a release's would, for tests: theirs is
/// one.
const CHECKOUT_VAR: &str = "ROSTER_UPDATE_CHECKOUT";

/// How long what was downloaded may take to say its version.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

/// What a new binary is unpacked as, beside the one it replaces, before
/// this process's id and something random (`temp`).
const TEMP: &str = ".roster-update-";

/// One left this long is left over whoever made it (`sweep`).
const LEFT_OVER: Duration = Duration::from_secs(60 * 60);

/// Refuses a build that can't update itself (`refusal`), or one whose
/// binary isn't there any more (`running_exe`); that binary's path, links
/// followed.
pub fn can_update() -> Result<PathBuf> {
    let release = RELEASE_BUILD.is_some() || std::env::var_os(CHECKOUT_VAR).is_some();
    if let Some(why) = refusal(release, std::env::consts::OS, std::env::consts::ARCH) {
        bail!(why);
    }
    running_exe()
}

/// The binary that runs, links followed to it, refused if what's at its
/// path now is another (replaced since it started, by an update or `cargo
/// install`): installing over that could take it back to an older release.
fn running_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("can't find roster's own path")?;
    // How Linux names the file of a process once it's gone from its path.
    let (path, gone) = match exe.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(path) if !exe.exists() => (PathBuf::from(path), true),
        _ => (exe, false),
    };
    let path = (path.canonicalize()).with_context(|| format!("can't find {}", path.display()))?;
    if gone || !same_file(Path::new("/proc/self/exe"), &path) {
        bail!(
            "roster at {} was replaced since this one started: restart it",
            path.display()
        );
    }
    Ok(path)
}

/// Whether `a` and `b` are the one file, links followed.
fn same_file(a: &Path, b: &Path) -> bool {
    identity(a).is_some() && identity(a) == identity(b)
}

/// Which file `path` is, links followed: its device and inode.
fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Why a build on `os` and `arch` can't update itself, if it can't: one
/// that isn't a `release`'s is for what made it to update; and there's a
/// release only for x86_64 Linux.
fn refusal(release: bool, os: &str, arch: &str) -> Option<String> {
    if !release {
        return Some(
            "not a release's build: update it as it was installed (git, cargo, a package)".into(),
        );
    }
    if (os, arch) != ("linux", "x86_64") {
        return Some(format!("there's no release of roster for {os} on {arch}"));
    }
    None
}

/// Installs `release` over `exe`, the binary that runs (`can_update`).
pub fn install(release: &Published, exe: &Path) -> Result<()> {
    let name = format!("agent-roster-v{}-linux-x86_64.tar.gz", release.version);
    let asset = |name: &str| {
        (release.assets.iter())
            .find(|a| a.name == name)
            .with_context(|| format!("roster {} has no {name}", release.version))
    };
    let archive = asset(&name)?;
    let checksum = asset(&format!("{name}.sha256"))?;
    let dir = exe.parent().context("roster's binary is in no directory")?;
    if let Err(e) = writable(dir) {
        bail!(
            "can't write {} ({e}), where roster is: to update it by hand, download {}, check it \
             against {}, and put the roster in it at {}",
            dir.display(),
            archive.url,
            checksum.url,
            exe.display()
        );
    }
    let running = identity(Path::new("/proc/self/exe")).context("can't find roster's own file")?;
    let bytes = github::download(&archive.url)?;
    let sum = github::download(&checksum.url)?;
    check_sum(&bytes, &String::from_utf8_lossy(&sum))?;
    replace(exe, &bytes, &release.version, running)
}

/// Whether files can be made and renamed in `dir`.
fn writable(dir: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    // SAFETY: access(2) only reads the path, a valid C string.
    match unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Whether `archive` is what its checksum file (`sha256sum`'s line: the
/// digest, then the file's name) says it is.
fn check_sum(archive: &[u8], published: &str) -> Result<()> {
    let expected = published.split_whitespace().next().unwrap_or_default();
    let actual: String = (Sha256::digest(archive).iter())
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("what was downloaded doesn't match the checksum published with it");
    }
    Ok(())
}

/// Unpacks the `roster` in `archive` (a tar.gz) beside `exe` under a name
/// of its own, with `exe`'s permissions (executable by its owner at least),
/// runs it to see it's `expected`, and renames it over `exe`, if that's
/// still the file `running` (`identity`): what another install put there
/// while this one downloaded may be newer. Failing, it's removed, and `exe`
/// is left as it was.
pub fn replace(exe: &Path, archive: &[u8], expected: &Release, running: (u64, u64)) -> Result<()> {
    let dir = exe.parent().context("roster's binary is in no directory")?;
    let mode = (fs::metadata(exe)).map_or(0o755, |m| m.permissions().mode() & 0o777 | 0o100);
    sweep(dir);
    let (new, file) = temp(dir)?;
    let done = unpack(archive, file)
        .and_then(|()| {
            fs::set_permissions(&new, fs::Permissions::from_mode(mode))
                .with_context(|| format!("can't write {}", new.display()))
        })
        .and_then(|()| says(&new, expected))
        .and_then(|()| {
            if identity(exe) != Some(running) {
                bail!(
                    "roster at {} was replaced as this one updated it: restart it",
                    exe.display()
                );
            }
            fs::rename(&new, exe).with_context(|| format!("can't replace {}", exe.display()))
        });
    if done.is_err() {
        fs::remove_file(&new).ok();
    }
    done
}

/// A file of its own in `dir` to unpack into, made anew: never one that was
/// there, nor where a link there points (`create_new` makes it or fails).
/// Its name says which process made it, for `sweep`.
fn temp(dir: &Path) -> Result<(PathBuf, fs::File)> {
    let random = uuid::Uuid::new_v4().simple().to_string();
    let path = dir.join(format!("{TEMP}{}-{}", std::process::id(), &random[..12]));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)
        .with_context(|| format!("can't write {}", path.display()))?;
    Ok((path, file))
}

/// Removes from `dir` what updates that didn't finish left (killed, or a
/// TUI quit meanwhile): those whose process is gone, or left an hour.
fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(TEMP)) else {
            continue;
        };
        let pid = rest.split('-').next().and_then(|p| p.parse::<u32>().ok());
        let gone = pid.is_none_or(|pid| !Path::new(&format!("/proc/{pid}")).exists());
        // Not followed, were it a link.
        let age = (entry.metadata().ok())
            .and_then(|m| m.modified().ok())
            .and_then(|at| at.elapsed().ok());
        if gone || age.is_some_and(|age| age > LEFT_OVER) {
            fs::remove_file(entry.path()).ok();
        }
    }
}

/// Writes the `roster` in `archive` to `file`: at its top or in its one
/// directory, as ci/dist.sh packs it
/// (`agent-roster-v0.2.0-linux-x86_64/roster`).
fn unpack(archive: &[u8], mut file: fs::File) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().context("the archive can't be read")? {
        let mut entry = entry.context("the archive can't be read")?;
        let path = entry.path().context("the archive can't be read")?;
        let parts: Vec<Component> = (path.components())
            .filter(|c| *c != Component::CurDir)
            .collect();
        let roster = matches!(parts.last(), Some(Component::Normal(name)) if *name == "roster");
        if !roster || parts.len() > 2 || !entry.header().entry_type().is_file() {
            continue;
        }
        io::copy(&mut entry, &mut file).context("the archive can't be read")?;
        // On the disk before it's renamed into place, so a crash can't
        // leave roster's name on a file not all written.
        file.sync_all()?;
        return Ok(());
    }
    bail!("the archive has no roster in it")
}

/// Whether `file` runs and says it's `expected`, as `--version` does on its
/// first line.
fn says(file: &Path, expected: &Release) -> Result<()> {
    let mut tries = 0;
    let mut child = loop {
        let spawned = Command::new(file)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            // Busy: a process another thread started while the file was
            // open for writing has it open till it runs its own program.
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            spawned => break spawned.context("what was downloaded doesn't run")?,
        }
    };
    let since = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if since.elapsed() > RUN_TIMEOUT {
            child.kill().ok();
            child.wait().ok();
            bail!("what was downloaded didn't say its version");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        io::Read::read_to_string(&mut stdout, &mut out).ok();
    }
    let said = out.lines().next().and_then(|l| l.strip_prefix("roster "));
    if !status.success() || said != Some(&expected.to_string()) {
        let said = said.unwrap_or("nothing");
        bail!("what was downloaded says it's {said}, not roster {expected}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tar.gz with `files` (path, text, mode), as ci/dist.sh packs one.
    fn archive(files: &[(&str, &str, u32)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, text, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            tar.append_data(&mut header, path, text.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn script(version: &str) -> String {
        format!("#!/bin/sh\necho \"roster {version}\"\n")
    }

    fn release(version: &str) -> Release {
        Release::parse(version).unwrap()
    }

    /// The files in `dir`, by name.
    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = (fs::read_dir(dir).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_roster_downloaded_replaces_the_binary_once_it_says_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("roster");
        fs::write(&exe, "the old one").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o750)).unwrap();
        let name = "agent-roster-v9.9.9-linux-x86_64";
        let packed = archive(&[
            (&format!("{name}/LICENSE"), "MIT", 0o644),
            (&format!("{name}/roster"), &script("9.9.9"), 0o755),
            (&format!("{name}/README.md"), "# roster", 0o644),
        ]);
        replace(&exe, &packed, &release("9.9.9"), identity(&exe).unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), script("9.9.9"));
        // As the one it replaced was.
        let mode = fs::metadata(&exe).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
        assert_eq!(files(dir.path()), ["roster"]);
    }

    #[test]
    fn what_an_update_that_didnt_finish_left_goes_with_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let ours = format!("{TEMP}{}-0123456789ab", std::process::id());
        // 4194304 is the most a pid can be.
        let gone = format!("{TEMP}4194305-0123456789ab");
        for name in [&ours, &gone, &format!("{TEMP}unnamed"), &"roster".into()] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        sweep(dir.path());
        assert_eq!(files(dir.path()), [ours.as_str(), "roster"]);
        // A new one is never one there.
        let (path, _) = temp(dir.path()).unwrap();
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&ours[..ours.len() - 12])
        );
        std::os::unix::fs::symlink(dir.path().join("roster"), dir.path().join("link")).unwrap();
        assert!(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.path().join("link"))
                .is_err()
        );
    }

    #[test]
    fn a_binary_replaced_since_it_started_is_told_from_the_one_that_runs() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        std::os::unix::fs::symlink(&a, dir.path().join("to-a")).unwrap();
        assert!(same_file(&a, &a));
        assert!(same_file(&dir.path().join("to-a"), &a));
        assert!(!same_file(&a, &b));
        // Renamed over: what's at the path is another file.
        let was = fs::File::open(&a).unwrap();
        fs::rename(&b, &a).unwrap();
        let fd = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&was));
        assert!(!same_file(Path::new(&fd), &a));
        assert!(!same_file(&a, &dir.path().join("missing")));
        // This test's own binary is where it ran from.
        assert!(running_exe().is_ok());
    }

    #[test]
    fn what_fails_to_say_its_version_leaves_the_binary_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("roster");
        fs::write(&exe, "the old one").unwrap();
        let cases = [
            // Another version than the release's.
            archive(&[(
                "agent-roster-v9.9.9-linux-x86_64/roster",
                &script("9.9.8"),
                0o755,
            )]),
            // One that doesn't run.
            archive(&[(
                "agent-roster-v9.9.9-linux-x86_64/roster",
                "\x7fELF garbage",
                0o755,
            )]),
            // None in it, nor deeper.
            archive(&[(
                "agent-roster-v9.9.9-linux-x86_64/README.md",
                "# roster",
                0o644,
            )]),
            archive(&[("a/b/roster", &script("9.9.9"), 0o755)]),
        ];
        for packed in cases
            .iter()
            .map(Vec::as_slice)
            .chain([&b"not a tar.gz"[..]])
        {
            let running = identity(&exe).unwrap();
            assert!(replace(&exe, packed, &release("9.9.9"), running).is_err());
            assert_eq!(fs::read_to_string(&exe).unwrap(), "the old one");
            assert_eq!(files(dir.path()), ["roster"]);
        }
        // At the archive's top, it's taken too.
        let packed = archive(&[("roster", &script("9.9.9"), 0o644)]);
        replace(&exe, &packed, &release("9.9.9"), identity(&exe).unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), script("9.9.9"));
    }

    #[test]
    fn a_download_not_matching_its_checksum_is_refused() {
        let packed = archive(&[("roster", &script("9.9.9"), 0o755)]);
        let sum: String = (Sha256::digest(&packed).iter())
            .map(|b| format!("{b:02x}"))
            .collect();
        let line = format!("{sum}  agent-roster-v9.9.9-linux-x86_64.tar.gz\n");
        check_sum(&packed, &line).unwrap();
        check_sum(&packed, &line.to_uppercase()).unwrap();
        let mut changed = packed.clone();
        changed[20] ^= 1;
        assert!(check_sum(&changed, &line).is_err());
        assert!(check_sum(&packed, "").is_err());
        assert!(check_sum(&packed, "not a checksum").is_err());
    }

    #[test]
    fn what_another_install_put_there_meanwhile_isnt_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("roster");
        fs::write(&exe, "the old one").unwrap();
        let running = identity(&exe).unwrap();
        // Put there as this one downloaded, as an install does: a new file.
        let other = dir.path().join("other");
        fs::write(&other, "a newer one").unwrap();
        fs::rename(&other, &exe).unwrap();
        let packed = archive(&[("roster", &script("9.9.9"), 0o755)]);
        let refused = replace(&exe, &packed, &release("9.9.9"), running).unwrap_err();
        assert!(
            format!("{refused:#}").contains("was replaced"),
            "{refused:#}"
        );
        assert_eq!(fs::read_to_string(&exe).unwrap(), "a newer one");
        assert_eq!(files(dir.path()), ["roster"]);
    }

    #[test]
    fn a_build_not_a_releases_or_for_another_system_is_refused() {
        let linux = ("linux", "x86_64");
        assert_eq!(refusal(true, linux.0, linux.1), None);
        assert!(
            refusal(false, linux.0, linux.1)
                .is_some_and(|why| why.starts_with("not a release's build"))
        );
        assert_eq!(
            refusal(true, "linux", "aarch64").as_deref(),
            Some("there's no release of roster for linux on aarch64")
        );
        assert!(refusal(true, "macos", "x86_64").is_some());
    }

    #[test]
    fn a_directory_that_cant_be_written_is_said() {
        let dir = tempfile::tempdir().unwrap();
        assert!(writable(dir.path()).is_ok());
        assert!(writable(&dir.path().join("missing")).is_err());
    }
}
