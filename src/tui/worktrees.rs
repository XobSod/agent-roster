//! How the worktrees sessions work in stand, read in the background: when
//! a turn ends, now and then while it works, and when it's selected, so
//! the list never waits on git.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::engine::Standing;
use crate::model::{Session, State};

/// What a read brings: how each session's worktree stands, by its id
/// (None: it couldn't be read).
type Read = Vec<(String, Option<Standing>)>;

/// How often a working session's worktree is read again.
const WHILE_WORKING: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Reads {
    /// How each session's worktree stood when last read, by its id.
    pub standing: HashMap<String, Standing>,
    /// When each was last asked for, and in what state its session was.
    asked: HashMap<String, (Instant, State)>,
    /// The session selected when they were last asked for.
    selected: Option<String>,
    /// What's being read now.
    reading: Option<Receiver<Read>>,
}

impl Reads {
    /// Takes in what was read, and asks for what's due: a worktree never
    /// read, one whose session's state changed since (a turn ended), a
    /// working one's every `WHILE_WORKING`, the selected one as it's
    /// selected. One read at a time.
    pub fn tick(&mut self, sessions: &[Session], selected: Option<&str>, now: Instant) {
        if let Some(reading) = &self.reading {
            match reading.try_recv() {
                Ok(read) => {
                    for (id, standing) in read {
                        match standing {
                            Some(standing) => self.standing.insert(id, standing),
                            None => self.standing.remove(&id),
                        };
                    }
                    self.reading = None;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => self.reading = None,
            }
        }
        let newly = selected.filter(|id| self.selected.as_deref() != Some(id));
        let due: Vec<(String, crate::model::Worktree)> = (sessions.iter())
            .filter_map(|s| Some((s, s.worktree.as_ref()?)))
            .filter(|(s, _)| match self.asked.get(&s.id) {
                None => true,
                Some((at, state)) => {
                    *state != s.state
                        || (s.state == State::Working && now.duration_since(*at) >= WHILE_WORKING)
                        || newly == Some(s.id.as_str())
                }
            })
            .map(|(s, w)| (s.id.clone(), w.clone()))
            .collect();
        self.selected = selected.map(str::to_string);
        // Those gone from the list aren't kept.
        self.asked
            .retain(|id, _| sessions.iter().any(|s| s.id == *id));
        self.standing
            .retain(|id, _| sessions.iter().any(|s| s.id == *id));
        if due.is_empty() {
            return;
        }
        for (id, _) in &due {
            if let Some(s) = sessions.iter().find(|s| s.id == *id) {
                self.asked.insert(id.clone(), (now, s.state));
            }
        }
        let (send, read) = channel();
        std::thread::spawn(move || {
            let read: Read = (due.into_iter())
                .map(|(id, w)| (id, crate::engine::standing(&w)))
                .collect();
            send.send(read).ok();
        });
        self.reading = Some(read);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::session;

    #[test]
    fn a_worktree_is_read_when_due_and_one_read_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        crate::git::tests::repo(&repo);
        let mut s = session("s");
        s.worktree = Some(crate::model::Worktree {
            root: repo.clone(),
            repo: repo.clone(),
            branch: "main".into(),
            base: Some("main".into()),
            made: false,
            set_up: true,
            trusted_in: None,
        });
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        let mut reads = Reads::default();
        let start = Instant::now();
        let sessions = vec![s.clone()];
        reads.tick(&sessions, None, start);
        let deadline = Instant::now() + Duration::from_secs(10);
        while reads.reading.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            reads.tick(&sessions, None, start);
        }
        assert_eq!(reads.standing["s"].changed, 1);
        // Nothing due: none asked.
        reads.tick(&sessions, None, start + Duration::from_secs(1));
        assert!(reads.reading.is_none());
        // Working on: again after a while; a turn's end at once.
        reads.tick(&sessions, None, start + WHILE_WORKING);
        assert!(reads.reading.is_some());
        let mut idle = s.clone();
        idle.state = State::Idle;
        reads.reading = None;
        reads.tick(std::slice::from_ref(&idle), None, start + WHILE_WORKING);
        assert!(reads.reading.is_some());
        // Gone from the list, let go.
        reads.reading = None;
        reads.tick(&[], None, start + WHILE_WORKING);
        assert!(reads.standing.is_empty());
    }
}
