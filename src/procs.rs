//! What the agents' processes use, read from `/proc`: each process's parent,
//! memory and CPU time, and the roster session it belongs to. Each inherits
//! `ROSTER_SESSION` (and `ROSTER_DB`, telling which roster's), so one that
//! left its agent (`nohup`, a build daemon) is still found, as is one that
//! outlived it: a leak is as likely in what the agent ran as in the agent
//! itself.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result};

/// A process, as `/proc` says it.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub pid: i32,
    pub ppid: i32,
    /// When it started (clock ticks since boot): with the pid, which process
    /// it is, pids being reused.
    pub started: u64,
    /// Resident memory, in bytes.
    pub rss: u64,
    /// CPU time it has used, in ms.
    pub cpu_ms: u64,
    pub command: String,
    /// The session it was started in (`ROSTER_SESSION`) of the roster that
    /// reads it, if its environment can be read (only one's own processes').
    pub session: Option<String>,
    /// A session's agent, whose tree is its session's.
    pub agent: bool,
}

impl Process {
    fn key(&self) -> (i32, u64) {
        (self.pid, self.started)
    }
}

/// Every process running now; which session of the roster keeping its
/// database at `db` each was started in, read only for one's own processes
/// outside the trees of `roots` (the agents', whose processes are theirs
/// anyway): reading a process's environment waits on its memory, which a
/// process thrashing holds up.
pub fn processes(roots: &[i32], db: &Path) -> Result<Vec<Process>> {
    // SAFETY: sysconf only reads system constants.
    let (ticks, page) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let (ticks, page) = (ticks.max(1) as u64, page.max(1) as u64);
    // SAFETY: getuid can't fail.
    let me = unsafe { libc::getuid() };
    let mut all = Vec::new();
    let mut own = HashSet::new();
    for entry in fs::read_dir("/proc")
        .context("couldn't read /proc")?
        .flatten()
    {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        // Gone since it was listed: left out.
        let Ok(stat) = fs::read(entry.path().join("stat")) else {
            continue;
        };
        let Some(p) = parse_stat(pid, &String::from_utf8_lossy(&stat), ticks, page) else {
            continue;
        };
        if entry.metadata().is_ok_and(|m| m.uid() == me) {
            own.insert(pid);
        }
        all.push(p);
    }
    let agents: HashSet<i32> = (roots.iter())
        .flat_map(|&root| descendants(&all, root))
        .collect();
    // This roster's database however its path is spelled (a home reached
    // through a symlink, say): the same file.
    let file = |path: &Path| fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
    let ours = file(db);
    let mut known: HashMap<Vec<u8>, bool> = HashMap::new();
    for p in &mut all {
        p.agent = roots.contains(&p.pid);
        if !own.contains(&p.pid) || agents.contains(&p.pid) {
            continue;
        }
        let Ok(env) = fs::read(format!("/proc/{}/environ", p.pid)) else {
            continue;
        };
        let Some((id, its_db)) = session_in(&env) else {
            continue;
        };
        let same = *known.entry(its_db.to_vec()).or_insert_with(|| {
            its_db == db.as_os_str().as_bytes()
                || ours.is_some_and(|f| file(Path::new(OsStr::from_bytes(its_db))) == Some(f))
        });
        p.session = same.then_some(id);
    }
    Ok(all)
}

/// `root` and every process under it.
fn descendants(all: &[Process], root: i32) -> Vec<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in all.iter().filter(|p| p.ppid != p.pid) {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut found = vec![root];
    let mut i = 0;
    while i < found.len() {
        if let Some(under) = children.get(&found[i]) {
            found.extend(under);
        }
        i += 1;
    }
    found
}

/// A process from its `/proc/<pid>/stat`: its command in parentheses (which
/// may hold any character, parentheses too), then the fields `proc(5)`
/// numbers, CPU time in clock ticks of `ticks` a second, memory in pages of
/// `page` bytes.
fn parse_stat(pid: i32, stat: &str, ticks: u64, page: u64) -> Option<Process> {
    let command = &stat[stat.find('(')? + 1..stat.rfind(')')?];
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // From field 3 (the state) on.
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    Some(Process {
        pid,
        ppid: fields.get(1)?.parse().ok()?,
        started: field(22)?,
        rss: field(24)? * page,
        cpu_ms: (field(14)? + field(15)?) * 1000 / ticks,
        command: command.to_string(),
        session: None,
        agent: false,
    })
}

/// `ROSTER_SESSION` in an environment as `/proc/<pid>/environ` holds it, with
/// its `ROSTER_DB`, telling which roster's session it is (a test's isn't
/// this one's).
fn session_in(env: &[u8]) -> Option<(String, &[u8])> {
    let var = |name: &[u8]| (env.split(|b| *b == 0)).find_map(|var| var.strip_prefix(name));
    let id = var(b"ROSTER_SESSION=").filter(|id| !id.is_empty())?;
    Some((
        String::from_utf8_lossy(id).into_owned(),
        var(b"ROSTER_DB=")?,
    ))
}

/// The processes as read once, with the CPU each took lately.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub processes: Vec<Process>,
    /// Each process's share of a CPU since the reading before (100: one core
    /// all the time), by pid and start; none on a first reading.
    cpu: HashMap<(i32, u64), f64>,
}

/// What some processes use together.
#[derive(Clone, Debug, PartialEq)]
pub struct Totals {
    /// Resident memory, in bytes, all of it added up (pages they share
    /// counted for each).
    pub memory: u64,
    /// Their share of a CPU lately, if known.
    pub cpu: Option<f64>,
    pub processes: usize,
    /// The one using most memory, and how much.
    pub heaviest: (String, u64),
}

impl Snapshot {
    pub fn cpu(&self, p: &Process) -> Option<f64> {
        self.cpu.get(&p.key()).copied()
    }

    /// The session `id`'s processes, each before those under it, with how
    /// deep it is (0 for a root): those under its agent (`root`, if it runs),
    /// then any other carrying its id, which left it or outlived it.
    pub fn of_session(&self, id: &str, root: Option<i32>) -> Vec<(&Process, usize)> {
        let ours: HashSet<i32> = (self.processes.iter())
            .filter(|p| Some(p.pid) == root || p.session.as_deref() == Some(id))
            .map(|p| p.pid)
            .collect();
        let mut children: HashMap<i32, Vec<&Process>> = HashMap::new();
        for p in &self.processes {
            // A parent of itself (pid 0's) isn't its child.
            if p.ppid != p.pid {
                children.entry(p.ppid).or_default().push(p);
            }
        }
        let roots = (self.processes.iter())
            .filter(|p| ours.contains(&p.pid) && (Some(p.pid) == root || !ours.contains(&p.ppid)));
        let (agent, others): (Vec<&Process>, Vec<&Process>) =
            roots.partition(|p| Some(p.pid) == root);
        // Under a root, unless known to be another session's.
        let mut tree = Vec::new();
        let mut seen = HashSet::new();
        for start in agent.into_iter().chain(others) {
            let mut stack = vec![(start, 0)];
            while let Some((p, depth)) = stack.pop() {
                if !seen.insert(p.pid) {
                    continue;
                }
                tree.push((p, depth));
                // Not into another session's agent, or a process known to be
                // another session's.
                let under = (children.get(&p.pid).into_iter().flatten().rev())
                    .filter(|c| !c.agent || Some(c.pid) == root)
                    .filter(|c| c.session.as_deref().is_none_or(|s| s == id));
                stack.extend(under.map(|c| (*c, depth + 1)));
            }
        }
        tree
    }

    /// What `processes` use together; None if there are none.
    pub fn totals(&self, processes: &[(&Process, usize)]) -> Option<Totals> {
        let heaviest = processes.iter().max_by_key(|(p, _)| p.rss)?.0;
        let cpu: Vec<f64> = processes.iter().filter_map(|(p, _)| self.cpu(p)).collect();
        Some(Totals {
            memory: processes.iter().map(|(p, _)| p.rss).sum(),
            cpu: (!cpu.is_empty()).then(|| cpu.iter().sum()),
            processes: processes.len(),
            heaviest: (heaviest.command.clone(), heaviest.rss),
        })
    }
}

/// `process` (a pid and when it started) and those under it in `tree`, a
/// session's (`Snapshot::of_session`); None if it's not there (it ended).
pub fn subtree<'a>(
    tree: &[(&'a Process, usize)],
    (pid, started): (i32, u64),
) -> Option<Vec<&'a Process>> {
    let at = (tree.iter()).position(|(p, _)| p.pid == pid && p.started == started)?;
    let depth = tree[at].1;
    let under = tree[at + 1..].iter().take_while(|(_, d)| *d > depth);
    Some(
        std::iter::once(tree[at].0)
            .chain(under.map(|(p, _)| *p))
            .collect(),
    )
}

/// A process held on to, to signal it and no other, pids being reused:
/// through a pidfd, which holds on to the process its start time is then
/// checked of; without pidfds (Linux before 5.3), by its pid, its start time
/// checked each time.
#[derive(Debug)]
pub struct Held {
    process: Process,
    fd: Option<libc::c_int>,
}

impl Held {
    /// `p`, if it's still the process it was.
    pub fn new(p: &Process) -> Option<Held> {
        // SAFETY: pidfd_open takes a pid and flags, and gives a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, p.pid, 0) } as libc::c_int;
        let no_pidfd = std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS);
        if fd < 0 && !no_pidfd {
            return None;
        }
        let held = Held {
            process: p.clone(),
            fd: (fd >= 0).then_some(fd),
        };
        held.same().then_some(held)
    }

    pub fn process(&self) -> &Process {
        &self.process
    }

    /// Still the process it was.
    fn same(&self) -> bool {
        let p = &self.process;
        (fs::read(format!("/proc/{}/stat", p.pid)).ok())
            .and_then(|stat| parse_stat(p.pid, &String::from_utf8_lossy(&stat), 1, 1))
            .is_some_and(|now| now.started == p.started)
    }

    /// Sends it `sig`; whether it was sent (not to another user's process).
    pub fn signal(&self, sig: libc::c_int) -> bool {
        match self.fd {
            Some(fd) => {
                let none: *const libc::siginfo_t = std::ptr::null();
                // SAFETY: the fd is ours and open, the signal info none, the
                // flags 0.
                unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd, sig, none, 0) == 0 }
            }
            // SAFETY: kill(2) has no memory effects.
            None => self.same() && unsafe { libc::kill(self.process.pid, sig) } == 0,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(fd) = self.fd {
            // SAFETY: the fd is ours, closed once.
            unsafe { libc::close(fd) };
        }
    }
}

/// Readings shorter apart than this keep the last figures: a CPU share
/// worked out over a few ms is noise.
const MIN_INTERVAL_MS: i64 = 1000;

/// Readings longer apart than this give no figures: an average over minutes
/// (the view left and come back to) isn't what they took lately.
const MAX_INTERVAL_MS: i64 = 5000;

/// Reads processes again and again, for the CPU each took between readings:
/// what a process used over its life hides a spike now.
#[derive(Debug, Default)]
pub struct Sampler {
    /// Each process's CPU time at the reading counted from, and when.
    base: HashMap<(i32, u64), u64>,
    base_at: Option<i64>,
    /// The shares worked out then.
    last: HashMap<(i32, u64), f64>,
}

impl Sampler {
    /// `processes`, read at `now`, with each one's CPU share since the last
    /// reading at least `MIN_INTERVAL_MS` before; one started since took all
    /// its time in it.
    pub fn read(&mut self, processes: Vec<Process>, now: i64) -> Snapshot {
        if self.base_at.is_none_or(|at| now - at >= MIN_INTERVAL_MS) {
            self.last.clear();
            if let Some(at) = self.base_at.filter(|at| now - at <= MAX_INTERVAL_MS) {
                let elapsed = (now - at) as f64;
                self.last = (processes.iter())
                    .map(|p| {
                        let before = self.base.get(&p.key()).copied().unwrap_or(0);
                        let used = p.cpu_ms.saturating_sub(before) as f64;
                        (p.key(), used * 100.0 / elapsed)
                    })
                    .collect();
            }
            self.base = processes.iter().map(|p| (p.key(), p.cpu_ms)).collect();
            self.base_at = Some(now);
        }
        let cpu = (processes.iter())
            .filter_map(|p| Some((p.key(), *self.last.get(&p.key())?)))
            .collect();
        Snapshot { processes, cpu }
    }
}

/// Bytes as a person reads them: `640 MB`, `11.8 GB`.
pub fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb < 1000.0 {
        format!("{} MB", mb.round())
    } else {
        format!("{:.1} GB", mb / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, ppid: i32, rss_mb: u64, cpu_ms: u64, command: &str) -> Process {
        Process {
            pid,
            ppid,
            started: pid as u64,
            rss: rss_mb * 1024 * 1024,
            cpu_ms,
            command: command.into(),
            session: None,
            agent: false,
        }
    }

    #[test]
    fn stat_and_environ_are_read_as_proc_writes_them() {
        // A command with spaces and parentheses; times in ticks of 1/100 s,
        // memory in pages of 4 KiB.
        let stat = "4242 (node (claude) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 1234 266 0 0 20 0 11 0 98765 1234567 51200 18446744073709551615";
        let p = parse_stat(4242, stat, 100, 4096).unwrap();
        assert_eq!(
            (p.ppid, p.started, p.rss, p.cpu_ms, p.command.as_str()),
            (1, 98765, 51200 * 4096, 15_000, "node (claude) x")
        );
        assert_eq!(parse_stat(1, "1 (init) S", 100, 4096), None);
        let env = b"HOME=/h\0ROSTER_SESSION=3f2a\0ROSTER_DB=/state/roster.db\0";
        assert_eq!(
            session_in(env),
            Some(("3f2a".to_string(), &b"/state/roster.db"[..]))
        );
        // Of no roster known, or of none.
        assert_eq!(session_in(b"ROSTER_SESSION=3f2a\0"), None);
        assert_eq!(
            session_in(b"ROSTER_SESSION=\0ROSTER_DB=/state/roster.db\0"),
            None
        );
        // This process's own, read from /proc: its session, if it runs in
        // one, read from its environment; not under an agent's tree.
        let me = std::process::id() as i32;
        let db = std::env::var_os("ROSTER_DB").map_or("/nowhere".into(), std::path::PathBuf::from);
        let all = processes(&[], &db).unwrap();
        assert!(all.iter().any(|p| p.pid == me && p.rss > 0));
        let session = (std::env::var("ROSTER_SESSION").ok())
            .filter(|s| !s.is_empty() && db != Path::new("/nowhere"));
        assert_eq!(all.iter().find(|p| p.pid == me).unwrap().session, session);
        assert!(
            processes(&[], Path::new("/nowhere"))
                .unwrap()
                .iter()
                .all(|p| p.session.is_none())
        );
        // Its roster's database by another path to it: the same.
        if db != Path::new("/nowhere") {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("roster.db");
            std::os::unix::fs::symlink(&db, &link).unwrap();
            let all = processes(&[], &link).unwrap();
            assert_eq!(all.iter().find(|p| p.pid == me).unwrap().session, session);
        }
        let under = processes(&[me], &db).unwrap();
        assert_eq!(under.iter().find(|p| p.pid == me).unwrap().session, None);
    }

    #[test]
    fn a_sessions_processes_are_its_agents_tree_and_any_carrying_its_id() {
        let mut detached = process(300, 1, 50, 0, "node");
        detached.session = Some("s1".into());
        let mut other = process(400, 1, 1, 0, "sleep");
        other.session = Some("s2".into());
        // Under one that left, but started in another session, or another
        // session's agent: that one's.
        let mut elsewhere = process(301, 300, 1, 0, "roster");
        elsewhere.session = Some("s2".into());
        let mut agent = process(302, 300, 1, 0, "claude");
        agent.agent = true;
        let snapshot = Snapshot {
            processes: vec![
                process(0, 0, 0, 0, "kernel"),
                process(1, 0, 1, 0, "init"),
                process(100, 1, 300, 0, "claude"),
                process(101, 100, 3, 0, "bash"),
                process(102, 101, 9000, 0, "cargo"),
                process(103, 100, 96, 0, "tsserver"),
                detached,
                elsewhere,
                agent,
                other,
            ],
            cpu: HashMap::new(),
        };
        let tree: Vec<(i32, usize)> = (snapshot.of_session("s1", Some(100)).iter())
            .map(|(p, depth)| (p.pid, *depth))
            .collect();
        assert_eq!(tree, [(100, 0), (101, 1), (102, 2), (103, 1), (300, 0)]);
        // Its agent gone: what outlived it.
        let left: Vec<i32> = (snapshot.of_session("s1", None).iter())
            .map(|(p, _)| p.pid)
            .collect();
        assert_eq!(left, [300]);
        let totals = snapshot
            .totals(&snapshot.of_session("s1", Some(100)))
            .unwrap();
        assert_eq!((totals.memory, totals.processes), (9449 * 1024 * 1024, 5));
        assert_eq!(totals.heaviest, ("cargo".to_string(), 9000 * 1024 * 1024));
        assert_eq!(totals.cpu, None);
        assert_eq!(snapshot.totals(&[]), None);
    }

    #[test]
    fn the_cpu_share_is_each_processs_own_since_a_second_before() {
        let mut sampler = Sampler::default();
        let mut read = |procs: Vec<Process>, at| {
            let s = sampler.read(procs, at);
            let shares: Vec<Option<f64>> = s.processes.iter().map(|p| s.cpu(p)).collect();
            shares
        };
        assert_eq!(read(vec![process(1, 0, 1, 5_000, "rustc")], 0), [None]);
        // A second later: the first rustc ended (what it used since, gone
        // with it), and another took 0.9 s since it started.
        assert_eq!(
            read(vec![process(2, 0, 1, 900, "rustc")], 1_000),
            [Some(90.0)]
        );
        // Read again 50 ms later: the same figures, not noise.
        assert_eq!(
            read(vec![process(2, 0, 1, 950, "rustc")], 1_050),
            [Some(90.0)]
        );
        // Its pid reused by another, started since: that one's own time.
        let mut reused = process(2, 0, 1, 300, "cc");
        reused.started = 99;
        assert_eq!(read(vec![reused.clone()], 2_000), [Some(30.0)]);
        // Read again after minutes: no figure, an average over them not
        // being what it took lately; then again a second on.
        reused.cpu_ms = 60_000;
        assert_eq!(read(vec![reused.clone()], 200_000), [None]);
        reused.cpu_ms = 60_500;
        assert_eq!(read(vec![reused], 201_000), [Some(50.0)]);
    }

    #[test]
    fn a_process_is_taken_with_those_under_it() {
        let procs = [
            process(100, 1, 1, 0, "claude"),
            process(101, 100, 1, 0, "bash"),
            process(102, 101, 1, 0, "cargo"),
            process(103, 102, 1, 0, "rustc"),
            process(104, 100, 1, 0, "tsserver"),
            process(300, 1, 1, 0, "node"),
        ];
        let tree: Vec<(&Process, usize)> = [0, 1, 2, 3, 1, 0]
            .iter()
            .zip(&procs)
            .map(|(d, p)| (p, *d))
            .collect();
        let pids = |found: Option<Vec<&Process>>| {
            found.map(|f| f.iter().map(|p| p.pid).collect::<Vec<_>>())
        };
        assert_eq!(pids(subtree(&tree, (101, 101))), Some(vec![101, 102, 103]));
        assert_eq!(pids(subtree(&tree, (104, 104))), Some(vec![104]));
        assert_eq!(
            pids(subtree(&tree, (100, 100))),
            Some(vec![100, 101, 102, 103, 104])
        );
        // Another process under the same pid (started since): not it.
        assert_eq!(pids(subtree(&tree, (101, 7))), None);
    }

    #[test]
    fn a_signal_goes_to_the_process_it_was_only() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let all = processes(&[], Path::new("/nowhere")).unwrap();
        let p = all.iter().find(|p| p.pid == pid).unwrap().clone();
        // Taken for another started at another time: not held.
        let mut other = p.clone();
        other.started += 1;
        assert!(Held::new(&other).is_none());
        // Held, it's signalled, then gone: nothing more to send to.
        let held = Held::new(&p).unwrap();
        assert!(held.signal(libc::SIGSTOP));
        assert!(held.signal(libc::SIGKILL));
        assert!(!child.wait().unwrap().success());
        assert!(!held.signal(libc::SIGKILL));
        assert!(Held::new(&p).is_none());
    }

    #[test]
    fn sizes_read_as_megabytes_then_gigabytes() {
        assert_eq!(size(640 * 1024 * 1024), "640 MB");
        assert_eq!(size(12_390 * 1024 * 1024), "12.1 GB");
    }
}
