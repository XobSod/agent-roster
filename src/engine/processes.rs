//! What sessions' processes use, and killing them.

use super::*;

/// What killing a session came to: how many of its processes were killed,
/// and whether its agent outlived the wait for it to end (a process stuck
/// on a disk can't be killed until it's back).
#[derive(Debug, PartialEq)]
pub struct Killed {
    pub killed: usize,
    pub lingering: bool,
}

impl Killed {
    /// What it came to, for `what` (a session's label, a process of it).
    pub fn said(&self, what: &str) -> String {
        let killed = match self.killed {
            0 => return format!("{what}: nothing running"),
            1 => format!("Killed {what}: 1 process"),
            n => format!("Killed {what}: {n} processes"),
        };
        match self.lingering {
            true => format!("{killed}, though its agent hasn't ended yet"),
            false => killed,
        }
    }
}

impl Roster {
    /// What killing session `id` would end now: its agent and all it
    /// started, those that left it or outlived it too (a deleted session's,
    /// by its id); with `process` (a pid and when it started), that one and
    /// those under it alone. Never this roster itself (run from inside the
    /// session, by its agent, say), which would end before the rest.
    pub fn kill_targets(
        &mut self,
        id: &str,
        process: Option<(i32, u64)>,
    ) -> Result<Vec<procs::Process>> {
        let panes = self.tmux.panes()?;
        let agent = |s: &Session| panes.get(&s.tmux_name()).filter(|p| !p.dead)?.pid;
        let sessions = self.db.sessions(true)?;
        let agents: Vec<i32> = sessions.iter().filter_map(agent).collect();
        let root = sessions.iter().find(|s| s.id == id).and_then(agent);
        let snapshot = self.processes(&agents)?;
        let tree = snapshot.of_session(id, root);
        let chosen = match process {
            None => tree.iter().map(|(p, _)| *p).collect(),
            Some(process) => match procs::subtree(&tree, process) {
                Some(chosen) => chosen,
                None => bail!("process {} has ended", process.0),
            },
        };
        let me = std::process::id() as i32;
        Ok(chosen
            .into_iter()
            .filter(|p| p.pid != me)
            .cloned()
            .collect())
    }

    /// Kills session `id`'s processes at once (SIGKILL), asking nothing of
    /// them, or with `process` that one and those under it
    /// (`kill_targets`). Each is stopped first, and they're read again till
    /// no new one shows: a process forking (a build's next job, a watcher
    /// starting again) would leave a child to outlive the kill.
    pub fn kill(&mut self, id: &str, process: Option<(i32, u64)>) -> Result<Killed> {
        // Its agent's pane and pid, while it runs.
        let s = self.db.sessions(true)?.into_iter().find(|s| s.id == id);
        let agent = match &s {
            Some(s) => (self.live_pid(&s.tmux_name())?).map(|pid| (s.tmux_name(), pid)),
            None => None,
        };
        // Each held on to from its stop to its kill, so the kill reaches it.
        let mut stopped: Vec<procs::Held> = Vec::new();
        for round in 0..5 {
            let found = match self.kill_targets(id, process) {
                Ok(found) => found,
                Err(e) if round == 0 => return Err(e),
                // Those stopped are killed whatever comes of reading again.
                Err(_) => break,
            };
            let mut new = 0;
            for p in found {
                let known = (stopped.iter())
                    .any(|h| h.process().pid == p.pid && h.process().started == p.started);
                if known {
                    continue;
                }
                if let Some(held) = procs::Held::new(&p) {
                    held.signal(libc::SIGSTOP);
                    stopped.push(held);
                    new += 1;
                }
            }
            if new == 0 {
                break;
            }
        }
        // Those under first: a group losing its leader while stopped is woken
        // by the kernel.
        let killed = (stopped.iter().rev())
            .filter(|h| h.signal(libc::SIGKILL))
            .count();
        // Its agent killed: stopped once its pane says so, its exit no error
        // (asked for, as a stop's).
        let mut lingering = false;
        if let Some((name, root)) =
            agent.filter(|(_, root)| stopped.iter().any(|h| h.process().pid == *root))
        {
            let deadline = Instant::now() + STOP_TIMEOUT;
            while self.live_pid(&name)? == Some(root) {
                if Instant::now() > deadline {
                    lingering = true;
                    break;
                }
                sleep(Duration::from_millis(50));
            }
            self.refresh()?;
            let mut after = self.db.session(id)?;
            if !lingering && after.state == State::Stopped && after.exit_status.is_some() {
                let before = after.clone();
                after.exit_status = None;
                self.db.save_changes(&before, &after)?;
            }
        }
        Ok(Killed { killed, lingering })
    }

    /// The processes running now, with the CPU each took since this process
    /// last read them; `agents`, the agents' pids, are where each one's
    /// session is known from without reading their environment.
    pub fn processes(&mut self, agents: &[i32]) -> Result<procs::Snapshot> {
        let all = procs::processes(agents, &self.paths.db(), &mut self.sessions_of)?;
        Ok(self.sampler.read(all, now_ms()))
    }

    /// What each session's processes use now, by session id: its agent and
    /// all it started, those that left it or outlived it too (an archived
    /// session's, still running).
    pub fn resources(&mut self) -> Result<HashMap<String, procs::Totals>> {
        let panes = self.tmux.panes()?;
        let agent = |s: &Session| panes.get(&s.tmux_name()).filter(|p| !p.dead)?.pid;
        let sessions = self.db.sessions(true)?;
        let agents: Vec<i32> = sessions.iter().filter_map(agent).collect();
        let snapshot = self.processes(&agents)?;
        Ok((sessions.iter())
            .filter_map(|s| {
                let tree = snapshot.of_session(&s.id, agent(s));
                Some((s.id.clone(), snapshot.totals(&tree)?))
            })
            .collect())
    }
}

pub(super) fn signal(pid: i32, sig: libc::c_int) {
    // SAFETY: kill(2) has no memory effects; a stale pid just fails.
    unsafe {
        libc::kill(pid, sig);
    }
}
