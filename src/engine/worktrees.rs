//! Worktrees: where a new session works, a worktree made for it or one
//! there was; set up, and trusted, as it starts.

use super::*;
use crate::git;
use crate::model::Worktree;

/// The script a project's new worktrees are set up by, once each, beside
/// its env file.
pub const WORKTREE_SETUP: &str = ".agent-roster.worktree";

impl Roster {
    /// Removes the worktree `w` made a moment ago, or as much of it as git made
    /// before it failed, for a session that couldn't be: what's in it too (a
    /// hook of the repository's made it, say), being no one's yet; its branch
    /// too, if it was made with it, having nothing of its own; and what roster
    /// keeps of it. What git still has for a worktree that it can't remove
    /// stays.
    pub fn undo_worktree(&self, w: &Worktree) {
        let removed = git::remove_worktree(&w.repo, &w.root, true).is_ok();
        let listed = || {
            (git::worktrees(&w.repo).unwrap_or_default().iter())
                .any(|l| place(&l.path) == place(&w.root))
        };
        // Made by git, not yet a worktree to it.
        if !removed && w.root.exists() && !listed() {
            fs::remove_dir_all(&w.root).ok();
        }
        if !w.root.exists() && !listed() {
            if w.branch_made && git::branch_exists(&w.repo, &w.branch) {
                git::delete_branch(&w.repo, &w.branch, true).ok();
            }
            // Its project's folder, once it's left empty.
            if let Some(parent) = w.root.parent() {
                fs::remove_dir(parent).ok();
            }
            fs::remove_dir_all(self.worktree_state(&w.root)).ok();
        }
    }
}

/// Where a new session works.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WorksIn {
    /// Its project's checkout.
    #[default]
    Checkout,
    /// A worktree made for it, on `branch` (the session's name if None)
    /// from `base` (the checkout's HEAD if None); a branch that's there is
    /// checked out as it is.
    New {
        branch: Option<String>,
        base: Option<String>,
    },
    /// A worktree there is: a directory in it, or a session working in it.
    Existing(String),
}

/// Held while a new session's worktree is made, from choosing its name to
/// saving it (`Roster::create_session`), by one roster process at a time: a
/// lock on a file in roster's state directory, let go as it's dropped. Why:
/// two making the same (an agent starting workers named alike) would both
/// find it free, and the one git refused would undo the other's.
pub(super) struct Making {
    /// Its lock, held while it's open.
    _held: fs::File,
}

impl Making {
    pub(super) fn take(state: &Path) -> Result<Making> {
        let _held = state_lock(state, "worktrees.lock")?;
        Ok(Making { _held })
    }
}

/// A lock on the file `name` in roster's state directory, waited for: held
/// while the file it gives is open, let go as it's dropped or the process
/// ends, however it ends.
pub(super) fn state_lock(state: &Path, name: &str) -> Result<fs::File> {
    use std::os::fd::AsRawFd;
    fs::create_dir_all(state).with_context(|| format!("can't make {}", state.display()))?;
    let path = state.join(name);
    let file = (fs::File::options().create(true).append(true).open(&path))
        .with_context(|| format!("can't open {}", path.display()))?;
    // SAFETY: flock on a descriptor this File owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("can't lock {}", path.display());
    }
    Ok(file)
}

impl Roster {
    /// What roster keeps of the worktree at `root`, in a folder of its own
    /// in roster's state, gone as roster removes it: that roster made it
    /// (`made`), so it's roster's to remove though its sessions went; that
    /// it's set up (`set_up_record`); the lock its setup runs under.
    fn worktree_state(&self, root: &Path) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        let name = util::fnv1a(root.as_os_str().as_bytes());
        self.paths.state_dir.join("worktrees-state").join(name)
    }

    /// Notes that roster made the worktree at `root`, now: not set up yet,
    /// whatever set up one there before. Noted with which directory it is,
    /// so one put in its place since (removed, and made again by hand) isn't
    /// taken for it.
    pub(super) fn made_now(&self, root: &Path) -> Result<()> {
        let state = self.worktree_state(root);
        fs::create_dir_all(&state).with_context(|| format!("can't make {}", state.display()))?;
        fs::remove_file(self.set_up_record(root)).ok();
        let which = directory_identity(root).context("can't read the worktree made")?;
        fs::write(state.join("made"), which)
            .with_context(|| format!("can't write in {}", state.display()))
    }

    /// Whether roster made the worktree at `root`, the directory there now
    /// (`made_now`).
    pub(super) fn made_here(&self, root: &Path) -> bool {
        let made = fs::read_to_string(self.worktree_state(root).join("made"));
        made.is_ok_and(|which| directory_identity(root).is_some_and(|now| now == which))
    }

    /// Where the record that the worktree at `root` is set up is kept: there
    /// once its setup script ran to its end there, whichever session ran
    /// it, and taken away as roster makes it (again) or removes it, so one
    /// made anew is set up anew.
    pub(super) fn set_up_record(&self, root: &Path) -> PathBuf {
        self.worktree_state(root).join("set-up")
    }

    /// Where new worktrees go: `[worktrees] directory`, else roster's state
    /// directory; a folder per project in it.
    pub fn worktrees_dir(&self) -> PathBuf {
        match self.config.worktrees.directory.as_deref() {
            Some(dir) => PathBuf::from(util::expand_tilde(dir)),
            None => self.paths.state_dir.join("worktrees"),
        }
    }

    /// Where a session named `name` in `project` works, as `works_in` says:
    /// None for the checkout, else its directory and its worktree, made now
    /// if it's to be new. What's to know of a new one is said (`notices`):
    /// a branch there already, checked out as it is, and the checkout's
    /// changes not committed, which it doesn't have.
    pub(super) fn place(
        &mut self,
        project: &Project,
        name: &str,
        works_in: &WorksIn,
    ) -> Result<Option<(PathBuf, Worktree)>> {
        if *works_in == WorksIn::Checkout {
            return Ok(None);
        }
        if self.chat_project_id()? == Some(project.id) {
            bail!("chats work in their own directory, not in a worktree");
        }
        let checkout = &project.origin;
        let Some(top) = git::toplevel(checkout) else {
            bail!(
                "project {:?} isn't in a git repository, so its sessions can't work in worktrees",
                project.name
            );
        };
        // Where the project is in its repository: there in the worktree too.
        let within = (checkout.canonicalize().ok())
            .and_then(|c| c.strip_prefix(&top).ok().map(Path::to_path_buf))
            .unwrap_or_default();
        let worktree = match works_in {
            WorksIn::Checkout => unreachable!(),
            WorksIn::New { branch, base } => {
                let branch = branch.as_deref().unwrap_or(name);
                self.made_worktree(project, &top, branch, base.as_deref())?
            }
            WorksIn::Existing(at) => match self.existing_worktree(project, &top, at)? {
                Some(worktree) => worktree,
                // The checkout itself.
                None => return Ok(None),
            },
        };
        // `join("")` would end it in a `/`, another directory to Claude.
        let dir = match within.as_os_str().is_empty() {
            true => worktree.root.clone(),
            false => worktree.root.join(&within),
        };
        if !dir.is_dir() {
            // One made now, not one there was (another session's, say).
            if matches!(works_in, WorksIn::New { .. }) {
                self.undo_worktree(&worktree);
            }
            bail!(
                "{} isn't in the worktree at {}, so the session can't work there",
                within.display(),
                worktree.root.display()
            );
        }
        Ok(Some((dir, worktree)))
    }

    /// A worktree made for `project` on `branch`, from `base` if the branch
    /// is new.
    fn made_worktree(
        &mut self,
        project: &Project,
        top: &Path,
        branch: &str,
        base: Option<&str>,
    ) -> Result<Worktree> {
        let checkout = &project.origin;
        if !git::branch_name_valid(top, branch) {
            bail!("{branch:?} isn't a name git takes for a branch");
        }
        let exists = git::branch_exists(top, branch);
        if exists && let Some(at) = git::checked_out(top, branch)? {
            bail!(
                "branch {branch} is checked out already, in {}: a session can work in that worktree",
                at.display()
            );
        }
        if exists && base.is_some_and(|b| b != "HEAD") {
            bail!(
                "branch {branch} is there already, so it isn't made from anything: leave out --base to check it out as it is"
            );
        }
        // What it's counted against: the branch the checkout is on, as it
        // moves, rather than the commit it was on; for a branch that was
        // there, the repository's default one, unless it's that one.
        let (from, recorded) = match (exists, base) {
            (true, _) => {
                self.notices.push(format!(
                    "Branch {branch} is there already: checked out as it is"
                ));
                (None, git::default_branch(top).filter(|d| d != branch))
            }
            (false, None | Some("HEAD")) => {
                let head = git::commit(checkout, "HEAD")?;
                let on = git::current_branch(checkout).unwrap_or_else(|| head.clone());
                (Some(head), Some(on))
            }
            // A branch or a tag by its name (`@{-1}` the branch it names),
            // counted against as it moves, made from as git makes a branch
            // from it; anything else (`HEAD~1`, a commit) by the commit it
            // names now, as in the worktree it would name another.
            (false, Some(base)) => {
                let commit = git::commit(checkout, base)?;
                let named = git::ref_name(checkout, base).unwrap_or(commit);
                (Some(named.clone()), Some(named))
            }
        };
        let dir = self.worktrees_dir();
        if !dir.is_absolute() {
            bail!(
                "[worktrees] directory is {}: it has to be a whole path (or start with ~)",
                dir.display()
            );
        }
        let root = dir.join(&project.name).join(git::directory_name(branch));
        if root.exists() {
            bail!(
                "{} is there already: roster makes no worktree in a directory it didn't make",
                root.display()
            );
        }
        // Not in the repository's own working tree, the project's checkout
        // say, which `[worktrees] directory` could put it in.
        let above = root.ancestors().skip(1).find(|a| a.exists());
        if let Some(inside) = above
            .and_then(git::toplevel)
            .filter(|t| git::same_repository(t, top))
        {
            bail!(
                "{} would be in {}, a working tree of the repository itself: [worktrees] directory has to be outside it",
                root.display(),
                inside.display()
            );
        }
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("can't make {}", parent.display()))?;
        }
        let made = |root: PathBuf| Worktree {
            root,
            repo: top.to_path_buf(),
            branch: branch.to_string(),
            base: recorded.clone(),
            made: true,
            branch_made: !exists,
            set_up: !checkout.join(WORKTREE_SETUP).is_file(),
            trusted_in: None,
            left_trusted_in: Vec::new(),
        };
        // Git may have made some of it before failing (a hook of the
        // repository's failing, say): that goes, a branch that was there
        // stays.
        if let Err(e) = git::add_worktree(checkout, &root, branch, from.as_deref()) {
            self.undo_worktree(&made(root));
            return Err(e);
        }
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(e) => {
                self.undo_worktree(&made(root.clone()));
                bail!("can't find the worktree made at {}: {e}", root.display());
            }
        };
        if let Err(e) = self.made_now(&root) {
            self.undo_worktree(&made(root));
            return Err(e);
        }
        if let Ok(n @ 1..) = git::changed_files(checkout) {
            let files = if n == 1 { "file" } else { "files" };
            self.notices.push(format!(
                "The checkout of {} has {n} {files} changed, not committed: the worktree doesn't have them",
                project.name
            ));
        }
        Ok(made(root))
    }

    /// The worktree `at` names, of `project`'s repository: a session's,
    /// else the one a directory is in; None for the checkout itself.
    fn existing_worktree(
        &self,
        project: &Project,
        top: &Path,
        at: &str,
    ) -> Result<Option<Worktree>> {
        let sessions = self.db.sessions(true)?;
        let named = match self.find(at, false)? {
            Some(s) => Some(s),
            None if !Path::new(at).exists() => self.find(at, true)?,
            None => None,
        };
        let root = match named {
            Some(s) => match &s.worktree {
                Some(w) => w.root.clone(),
                None => bail!(
                    "{} works in its project's checkout, not in a worktree",
                    s.label()
                ),
            },
            None => {
                let dir = PathBuf::from(util::expand_tilde(at));
                match git::toplevel(&dir) {
                    Some(root) => root,
                    None => bail!("{at} is neither a session nor a directory in a git worktree"),
                }
            }
        };
        if !git::same_repository(&root, top) {
            bail!(
                "{} isn't a worktree of project {:?}'s repository",
                root.display(),
                project.name
            );
        }
        if root == top {
            return Ok(None);
        }
        let Some(branch) = git::current_branch(&root) else {
            bail!("the worktree at {} is on no branch", root.display());
        };
        // As the sessions working there have it: what it was made from, and
        // whether roster made it.
        let theirs: Vec<&Worktree> = (sessions.iter())
            .filter_map(|s| s.worktree.as_ref())
            .filter(|w| w.root == root)
            .collect();
        // One roster made, its sessions gone (it stayed for its changes,
        // say), is roster's still, counted against the default branch as one
        // made again is.
        let ours = theirs.is_empty() && self.made_here(&root);
        let base = match ours {
            true => git::default_branch(top).filter(|d| *d != branch),
            false => theirs.iter().find_map(|w| w.base.clone()),
        };
        // As roster's record says of the directory there now, not as the
        // sessions there before had it: replaced since, it isn't roster's.
        let made = self.made_here(&root);
        Ok(Some(Worktree {
            root,
            repo: top.to_path_buf(),
            branch,
            base,
            made,
            branch_made: false,
            // Its record says, as a session in it starts (`set_up_record`).
            set_up: !(made && project.origin.join(WORKTREE_SETUP).is_file()),
            trusted_in: None,
            left_trusted_in: Vec::new(),
        }))
    }
}

impl Roster {
    /// The project a conversation that ran in `dir`, in no project's
    /// directory, belongs to by its worktree: the project of the repository
    /// a worktree there is of, whose place in it holds where the
    /// conversation ran, the deepest; or the project one of roster's that's
    /// gone was made for. What's worked out is kept in `found`, for the next.
    pub(crate) fn project_by_worktree(
        &self,
        dir: &Path,
        projects: &[Project],
        found: &mut ByWorktree,
    ) -> Option<Project> {
        if let Some(known) = found.by_dir.get(dir) {
            return known.clone();
        }
        let project = self.worktree_project(dir, projects, found);
        found.by_dir.insert(dir.to_path_buf(), project.clone());
        project
    }

    fn worktree_project(
        &self,
        dir: &Path,
        projects: &[Project],
        found: &mut ByWorktree,
    ) -> Option<Project> {
        if !dir.exists() {
            let name = self.made_for(dir)?.0;
            return projects.iter().find(|p| p.name == name).cloned();
        }
        // A worktree's root has a `.git` file, not a directory: only then
        // is git asked.
        if !dir.ancestors().any(|a| a.join(".git").is_file()) {
            return None;
        }
        let top = git::toplevel(dir)?;
        let repo = git::repository(&top)?;
        let dir = dir.canonicalize().ok()?;
        let within = dir.strip_prefix(&top).ok()?;
        let places = found.places.get_or_insert_with(|| {
            (projects.iter())
                .filter_map(|p| {
                    let top = git::toplevel(&p.origin)?;
                    let origin = p.origin.canonicalize().ok()?;
                    let within = origin.strip_prefix(&top).ok()?.to_path_buf();
                    Some((p.clone(), git::repository(&top)?, within))
                })
                .collect()
        });
        (places.iter())
            .filter(|(_, r, place)| *r == repo && within.starts_with(place))
            .max_by_key(|(_, _, place)| place.components().count())
            .map(|(p, ..)| p.clone())
    }

    /// For a directory in one of roster's worktrees: the project it was made
    /// for and the worktree's directory's name, by its path.
    fn made_for(&self, dir: &Path) -> Option<(String, String)> {
        let ours = self.worktrees_dir();
        let canonical = ours.canonicalize().ok();
        let rest = dir
            .strip_prefix(&ours)
            .ok()
            .or_else(|| dir.strip_prefix(canonical.as_ref()?).ok())?;
        let mut parts = rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        Some((parts.next()?, parts.next()?))
    }

    /// The worktree a conversation adopted into `project` ran in, at `dir`:
    /// one there is, of its repository, on a branch; or one of roster's
    /// that's gone, made again now from its branch, if that's there and
    /// free. None for the checkout, a detached worktree (adopted there, as
    /// any directory), or a directory that's no worktree.
    pub(crate) fn adopted_worktree(
        &mut self,
        project: &Project,
        dir: &Path,
    ) -> Result<Option<Worktree>> {
        let Some(top) = git::toplevel(&project.origin) else {
            return Ok(None);
        };
        if dir.exists() {
            if !git::same_repository(dir, &top) || git::current_branch(dir).is_none() {
                return Ok(None);
            }
            return self.existing_worktree(project, &top, &dir.to_string_lossy());
        }
        let Some((made_for, name)) = (self.made_for(dir))
            .filter(|(p, _)| *p == project.name)
            .filter(|_| self.worktrees_dir().is_absolute())
        else {
            return Ok(None);
        };
        let root = self.worktrees_dir().join(made_for).join(&name);
        let branches: Vec<String> = (git::branches(&top).into_iter())
            .filter(|b| git::directory_name(b) == name)
            .collect();
        let branch = match &branches[..] {
            [branch] => branch.clone(),
            [] => bail!(
                "it ran in a worktree, {}, that's gone, and so is its branch: it can't be adopted there",
                root.display()
            ),
            _ => bail!(
                "it ran in a worktree, {}, that's gone, whose branch could be any of {}",
                root.display(),
                branches.join(", ")
            ),
        };
        if let Some(at) = git::checked_out(&top, &branch)? {
            bail!(
                "it ran in a worktree, {}, that's gone, and its branch {branch} is checked out in {} now",
                root.display(),
                at.display()
            );
        }
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("can't make {}", parent.display()))?;
        }
        {
            let _making = Making::take(&self.paths.state_dir)?;
            git::add_worktree_again(&project.origin, &root, &branch)?;
        }
        let root = root.canonicalize()?;
        self.made_now(&root)?;
        self.notices.push(format!(
            "Made its worktree, {}, again from branch {branch}",
            util::short_path(&root)
        ));
        Ok(Some(Worktree {
            root,
            repo: top.clone(),
            base: git::default_branch(&top).filter(|d| *d != branch),
            branch,
            made: true,
            branch_made: false,
            set_up: !project.origin.join(WORKTREE_SETUP).is_file(),
            trusted_in: None,
            left_trusted_in: Vec::new(),
        }))
    }
}

/// What adopting has worked out of worktrees, for each conversation after
/// the first: the projects' places in their repositories, and the project
/// each directory belongs to.
#[derive(Default)]
pub struct ByWorktree {
    places: Option<Vec<(Project, PathBuf, PathBuf)>>,
    by_dir: std::collections::HashMap<PathBuf, Option<Project>>,
}

/// A worktree sessions deleted for good work in, as it is now: whether it
/// goes with them, what's in it not committed, and its branch.
#[derive(Clone, Debug, PartialEq)]
pub struct Leaving {
    pub root: PathBuf,
    pub repo: PathBuf,
    /// Where the agents of those sessions were told to trust it, by agent.
    pub trusted_in: Vec<(String, PathBuf)>,
    /// Why it stays; None: it goes with them.
    pub stays: Option<String>,
    /// Files changed and not committed in one that goes: they go only if
    /// you say so.
    pub changed: usize,
    /// Its branch, if it's there to delete.
    pub branch: Option<Branch>,
}

/// A worktree's branch, and how it stands against its base.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub name: String,
    pub base: Option<String>,
    /// Commits it has that its base hasn't: 0, merged; None, not known.
    pub unmerged: Option<usize>,
}

/// What's decided of the worktrees deleting sessions for good leaves: those
/// with changes not committed removed too, their branches deleted too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deleting {
    pub discard_changes: bool,
    pub delete_branches: bool,
}

impl Roster {
    /// What deleting the sessions `ids` for good leaves of the worktrees
    /// they work in, a worktree once however many of them work in it.
    pub fn leaving(&self, ids: &[String]) -> Result<Vec<Leaving>> {
        let all = self.db.sessions(true)?;
        let projects = self.db.projects()?;
        let (going, staying): (Vec<&Session>, Vec<&Session>) =
            all.iter().partition(|s| ids.contains(&s.id));
        let mut leaving: Vec<Leaving> = Vec::new();
        for s in &going {
            let Some(w) = &s.worktree else { continue };
            if let Some(l) = leaving.iter_mut().find(|l| l.root == w.root) {
                l.trusted_in.extend(trusted_in(s));
                continue;
            }
            // Working in it as its own, or only there (adopted where it was
            // detached, say).
            let other = (staying.iter()).find(|o| {
                o.worktree.as_ref().is_some_and(|ow| ow.root == w.root)
                    || works_under(&o.workdir, &w.root)
            });
            let project = (projects.iter()).find(|p| p.origin.starts_with(&w.root));
            let mut stays = match (other, project) {
                (Some(o), _) => Some(format!("{} works in it", o.label())),
                (None, Some(p)) => Some(format!("project {} is there", p.name)),
                // Roster's, as its record says of the directory there now:
                // not one put in its place since, though its session's still
                // here; one gone has nothing to say of.
                (None, None) if !w.made || (w.root.exists() && !self.made_here(&w.root)) => {
                    Some("roster didn't make it".to_string())
                }
                (None, None) => None,
            };
            // Moved off any branch, to commits no branch has: removing it
            // would lose them.
            if stays.is_none() && w.root.exists() && git::current_branch(&w.root).is_none() {
                match git::head_on_a_ref(&w.root) {
                    Ok(true) => {}
                    Ok(false) => stays = Some("it's at a commit no branch or tag has".to_string()),
                    Err(e) => stays = Some(format!("git can't tell what's in it ({e:#})")),
                }
            }
            // What git can't read isn't taken for clean.
            let changed = match stays.is_none() && w.root.exists() {
                true => git::changed_files(&w.root).unwrap_or_else(|e| {
                    stays = Some(format!("git can't tell what's in it ({e:#})"));
                    0
                }),
                false => 0,
            };
            let goes = stays.is_none();
            // The one it's on now, renamed or another since; not the branch
            // its others are counted against, nor the default.
            let name = branch_now(w).unwrap_or_else(|| w.branch.clone());
            let shared = w.base.as_ref() == Some(&name)
                || git::default_branch(&w.repo).as_ref() == Some(&name);
            let deletable = goes && !shared && git::branch_exists(&w.repo, &name);
            let branch = deletable.then(|| Branch {
                unmerged: (w.base.as_deref()).and_then(|b| git::unmerged(&w.repo, &name, b).ok()),
                name,
                base: w.base.clone(),
            });
            leaving.push(Leaving {
                root: w.root.clone(),
                repo: w.repo.clone(),
                trusted_in: trusted_in(s),
                stays,
                changed,
                branch,
            });
        }
        Ok(leaving)
    }

    /// Where `s`'s agent had its worktree trusted, and the sessions deleted
    /// from it before had: left to another session working in it, if one
    /// is, not one of those `going` with it, as `s` is deleted, so it's
    /// taken back however the worktree is left.
    pub(super) fn hand_over_trust(&self, s: &Session, going: &[String]) -> Result<()> {
        let Some(w) = &s.worktree else {
            return Ok(());
        };
        let records = trusted_in(s);
        if records.is_empty() {
            return Ok(());
        }
        let others = self.db.sessions(true)?;
        let Some(heir) = (others.into_iter()).find(|o| {
            !going.contains(&o.id) && o.worktree.as_ref().is_some_and(|ow| ow.root == w.root)
        }) else {
            return Ok(());
        };
        let mut after = heir.clone();
        if let Some(hw) = &mut after.worktree {
            for record in records {
                if !hw.left_trusted_in.contains(&record) {
                    hw.left_trusted_in.push(record);
                }
            }
        }
        self.db.save_changes(&heir, &after)
    }

    /// Removes the worktrees deleting sessions left, as `how` says, with
    /// their branches: what was done, and what stays and why, to say.
    pub(super) fn let_go(&mut self, leaving: Vec<Leaving>, how: Deleting) -> Vec<String> {
        let mut said = Vec::new();
        for l in leaving {
            let at = util::short_path(&l.root);
            if let Some(why) = &l.stays {
                said.push(format!("Its worktree, {at}, stays: {why}"));
                continue;
            }
            if l.changed > 0 && !how.discard_changes {
                let n = l.changed;
                let files = if n == 1 { "file" } else { "files" };
                said.push(format!(
                    "Its worktree, {at}, stays, with its {n} {files} changed, not committed"
                ));
                continue;
            }
            // Gone already: git forgets it, and it alone, if it hadn't.
            let removed = match l.root.exists() {
                true => git::remove_worktree(&l.repo, &l.root, how.discard_changes),
                false => {
                    git::remove_worktree(&l.repo, &l.root, true).ok();
                    Ok(())
                }
            };
            if let Err(e) = removed {
                said.push(format!("Its worktree, {at}, stays: {e:#}"));
                continue;
            }
            // Where its sessions' agents were told to trust it, and where
            // they keep their trust by default: a session that shared it and
            // was deleted before took its record with it.
            let mut told = l.trusted_in.clone();
            for name in agent::available(&self.config) {
                if let Some(file) = agent::adapter(&name).trust_config() {
                    told.push((name, file));
                }
            }
            told.sort();
            told.dedup();
            for (agent, file) in &told {
                agent::adapter(agent)
                    .untrust_folders_under(file, &l.root)
                    .ok();
            }
            fs::remove_dir_all(self.worktree_state(&l.root)).ok();
            // Its project's folder, once it's left empty.
            if let Some(parent) = l.root.parent() {
                fs::remove_dir(parent).ok();
            }
            said.push(format!("Removed its worktree, {at}"));
            let Some(branch) = l.branch else { continue };
            if !how.delete_branches {
                continue;
            }
            match git::delete_branch(&l.repo, &branch.name, true) {
                Ok(()) => said.push(format!("Deleted its branch {}", branch.name)),
                Err(e) => said.push(format!("Its branch {} stays: {e:#}", branch.name)),
            }
        }
        said
    }
}

/// Where the agent of `s` had its worktree trusted, and the agents of
/// sessions deleted from it before had, by agent.
fn trusted_in(s: &Session) -> Vec<(String, PathBuf)> {
    let Some(w) = &s.worktree else {
        return Vec::new();
    };
    let own = (w.trusted_in.iter()).map(|file| (s.agent.clone(), file.clone()));
    own.chain(w.left_trusted_in.iter().cloned()).collect()
}

/// The branch the worktree `w` is on now, which can be another than it was
/// made on (renamed, or switched to): as its HEAD says, or once its
/// directory is gone, as git still has it; None if detached. Where git has
/// forgotten it, the one it was made on.
pub(crate) fn branch_now(w: &Worktree) -> Option<String> {
    if w.root.exists() {
        return git::current_branch(&w.root);
    }
    let listed = git::worktrees(&w.repo).unwrap_or_default();
    match listed
        .into_iter()
        .find(|l| place(&l.path) == place(&w.root))
    {
        Some(l) => l.branch,
        None => Some(w.branch.clone()),
    }
}

/// Which directory `dir` is, as long as it's there: its inode, and when it
/// was made, where the filesystem says, as an inode can be given again to
/// one made after it's gone. Not its device's number, which some
/// filesystems give anew as they're mounted (btrfs subvolumes, overlays).
fn directory_identity(dir: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(dir).ok()?;
    let born = (meta.created().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some(format!("{}:{born}", meta.ino()))
}

/// Where `path` is, its parent's links resolved, though it may be gone: as
/// git says where a worktree is.
fn place(path: &Path) -> PathBuf {
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// Whether the directory `dir` is at or under `root`, as found or with its
/// links resolved.
fn works_under(dir: &Path, root: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    dir.starts_with(root) || canonical(dir).starts_with(canonical(root))
}

/// What deleting for good says of the worktrees that stay (after the
/// question itself): one, a session's; else each.
pub fn staying_said(leaving: &[Leaving]) -> String {
    let stays: Vec<(String, &str)> = (leaving.iter())
        .filter_map(|l| Some((util::short_path(&l.root), l.stays.as_deref()?)))
        .collect();
    match &stays[..] {
        [] => String::new(),
        [(at, why)] if leaving.len() == 1 => format!(" Its worktree, {at}, stays: {why}."),
        _ => {
            let each: Vec<String> = stays
                .iter()
                .map(|(at, why)| format!("{at} ({why})"))
                .collect();
            format!(" Worktrees that stay: {}.", each.join(", "))
        }
    }
}

/// Whether to remove the worktrees with changes not committed that deleting
/// would remove; None if there are none.
pub fn changes_question(leaving: &[Leaving]) -> Option<String> {
    let files = |n: usize| match n {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    let dirty: Vec<&Leaving> = (leaving.iter())
        .filter(|l| l.stays.is_none() && l.changed > 0)
        .collect();
    match &dirty[..] {
        [] => None,
        [l] if leaving.len() == 1 => Some(format!(
            "Its worktree, {}, has {} changed, not committed: remove them with it?",
            util::short_path(&l.root),
            files(l.changed)
        )),
        _ => {
            let each: Vec<String> = (dirty.iter())
                .map(|l| format!("{} ({})", util::short_path(&l.root), files(l.changed)))
                .collect();
            Some(format!(
                "Worktrees with changes not committed: {}. Remove them with their changes?",
                each.join(", ")
            ))
        }
    }
}

/// Whether to delete the branches of the worktrees deleting removes (with
/// those that have changes, if `discard`); None if there are none.
pub fn branches_question(leaving: &[Leaving], discard: bool) -> Option<String> {
    let branches: Vec<&Branch> = (leaving.iter())
        .filter(|l| l.stays.is_none() && (discard || l.changed == 0))
        .filter_map(|l| l.branch.as_ref())
        .collect();
    let base = |b: &Branch| base_said(b.base.as_deref().unwrap_or("its base")).to_string();
    let commits = |n: usize| match n {
        1 => "1 commit".to_string(),
        n => format!("{n} commits"),
    };
    match &branches[..] {
        [] => None,
        [b] if leaving.len() == 1 => Some(match b.unmerged {
            Some(0) => format!(
                "Its branch {} is merged into {}: delete it too?",
                b.name,
                base(b)
            ),
            None if b.base.is_some() => format!(
                "Its branch {} can't be told merged or not, {} being gone: delete it too?",
                b.name,
                base(b)
            ),
            Some(n) => format!(
                "Its branch {} has {} {} doesn't, which deleting it loses: delete it too?",
                b.name,
                commits(n),
                base(b)
            ),
            None => format!("Delete its branch {} too?", b.name),
        }),
        _ => {
            let each: Vec<String> = (branches.iter())
                .map(|b| match b.unmerged {
                    Some(0) => format!("{} (merged into {})", b.name, base(b)),
                    Some(n) => format!("{} ({} {} doesn't have)", b.name, commits(n), base(b)),
                    None => b.name.clone(),
                })
                .collect();
            Some(format!(
                "Their branches: {}. Delete them too?",
                each.join(", ")
            ))
        }
    }
}

/// How a worktree stands against its base, as read now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// Commits its branch has that its base hasn't, and the other way;
    /// None if its base can't be found.
    pub ahead_behind: Option<(usize, usize)>,
    /// Files changed and not committed, untracked ones included.
    pub changed: usize,
    /// What changed against its base, committed or not: files, lines added
    /// and removed; None without a base to tell.
    pub diff: Option<(usize, usize, usize)>,
}

/// How the worktree `w` stands now; None if it can't be read (gone, say).
pub fn standing(w: &Worktree) -> Option<Standing> {
    if !w.root.is_dir() {
        return None;
    }
    let changed = git::changed_files(&w.root).ok()?;
    let base = w.base.as_deref();
    let ahead_behind = base.and_then(|b| git::ahead_behind(&w.root, b).ok());
    let diff = base.and_then(|b| git::diff_stat(&w.root, b).ok());
    Some(Standing {
        ahead_behind,
        changed,
        diff,
    })
}

/// A base as said: a commit it was made from, short.
pub fn base_said(base: &str) -> &str {
    match base.len() == 40 && base.chars().all(|c| c.is_ascii_hexdigit()) {
        true => &base[..7],
        false => base,
    }
}

/// How `standing` is said briefly, on a card: what there's something to
/// say of, each.
pub fn standing_brief(w: &Worktree, standing: &Standing) -> Vec<String> {
    let (ahead, behind) = standing.ahead_behind.unwrap_or_default();
    let mut brief: Vec<String> = [
        (ahead, "ahead"),
        (behind, "behind"),
        (standing.changed, "changed"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    if let Some(base) = w
        .base
        .as_deref()
        .filter(|_| standing.ahead_behind.is_none())
    {
        brief.insert(0, format!("{} gone", base_said(base)));
    }
    brief
}

/// How the worktree `w` stands, said short, for a line beside others: its
/// branch and base, what there's something to say of, and the lines it
/// changed against its base.
pub fn standing_short(w: &Worktree, standing: Option<&Standing>) -> String {
    let mut parts = vec![match &w.base {
        Some(base) => format!("{} ← {}", w.branch, base_said(base)),
        None => w.branch.clone(),
    }];
    if let Some(st) = standing {
        parts.extend(standing_brief(w, st));
        if let Some((_, added, removed)) = st.diff.filter(|d| d.0 > 0) {
            parts.push(format!("+{added} −{removed}"));
        }
    }
    parts.join(" · ")
}

/// How the worktree `w` stands, said whole: its branch and base, how far
/// ahead and behind, what's changed not committed, and what changed against
/// its base.
pub fn standing_said(w: &Worktree, standing: Option<&Standing>) -> String {
    let mut parts = vec![match &w.base {
        Some(base) => format!("{} ← {}", w.branch, base_said(base)),
        None => w.branch.clone(),
    }];
    if let Some(st) = standing {
        if let Some(base) = &w.base {
            parts.push(match st.ahead_behind {
                None => format!("{} can't be found", base_said(base)),
                Some((0, 0)) => "even with it".into(),
                Some((a, 0)) => format!("{a} ahead"),
                Some((0, b)) => format!("{b} behind"),
                Some((a, b)) => format!("{a} ahead, {b} behind"),
            });
        }
        if st.changed > 0 {
            let files = if st.changed == 1 { "file" } else { "files" };
            parts.push(format!("{} {files} changed, not committed", st.changed));
        }
        if let Some((files, added, removed)) = st.diff.filter(|d| d.0 > 0) {
            let in_files = if files == 1 {
                "1 file".into()
            } else {
                format!("{files} files")
            };
            parts.push(format!("+{added} −{removed} in {in_files}"));
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaving(
        root: &str,
        stays: Option<&str>,
        changed: usize,
        unmerged: Option<usize>,
    ) -> Leaving {
        Leaving {
            root: root.into(),
            repo: "/r".into(),
            trusted_in: Vec::new(),
            stays: stays.map(str::to_string),
            changed,
            branch: stays.is_none().then(|| Branch {
                name: root.rsplit('/').next().unwrap().into(),
                base: Some("main".into()),
                unmerged,
            }),
        }
    }

    #[test]
    fn deleting_for_good_says_what_it_leaves_of_worktrees() {
        // One session's: of it.
        let one = [leaving("/w/fix", None, 1, Some(0))];
        assert_eq!(
            changes_question(&one).unwrap(),
            "Its worktree, /w/fix, has 1 file changed, not committed: remove them with it?"
        );
        // A branch with changes kept isn't asked of: its worktree stays.
        assert_eq!(branches_question(&one, false), None);
        assert_eq!(
            branches_question(&one, true).unwrap(),
            "Its branch fix is merged into main: delete it too?"
        );
        // Its base gone, or none known: said so.
        let unknown = [leaving("/w/fix", None, 0, None)];
        assert_eq!(
            branches_question(&unknown, false).unwrap(),
            "Its branch fix can't be told merged or not, main being gone: delete it too?"
        );
        let mut none = unknown.clone();
        if let Some(b) = &mut none[0].branch {
            b.base = None;
        }
        assert_eq!(
            branches_question(&none, false).unwrap(),
            "Delete its branch fix too?"
        );
        // A project's: each.
        let many = [
            leaving("/w/a", None, 2, Some(1)),
            leaving("/w/b", None, 0, Some(0)),
            leaving("/w/c", Some("roster didn't make it"), 0, None),
        ];
        assert_eq!(
            changes_question(&many).unwrap(),
            "Worktrees with changes not committed: /w/a (2 files). Remove them with their changes?"
        );
        assert_eq!(
            branches_question(&many, true).unwrap(),
            "Their branches: a (1 commit main doesn't have), b (merged into main). Delete them too?"
        );
        assert_eq!(
            branches_question(&many, false).unwrap(),
            "Their branches: b (merged into main). Delete them too?"
        );
        assert_eq!(
            staying_said(&many),
            " Worktrees that stay: /w/c (roster didn't make it)."
        );
        assert_eq!(staying_said(&one), "");

        // How one stands: a base that can't be found isn't taken for even.
        let w = Worktree {
            root: "/w/fix".into(),
            repo: "/r".into(),
            branch: "fix".into(),
            base: Some("gone".into()),
            made: true,
            branch_made: true,
            set_up: true,
            trusted_in: None,
            left_trusted_in: Vec::new(),
        };
        let lost = Standing {
            ahead_behind: None,
            changed: 1,
            diff: None,
        };
        assert_eq!(
            standing_said(&w, Some(&lost)),
            "fix ← gone · gone can't be found · 1 file changed, not committed"
        );
        let even = Standing {
            ahead_behind: Some((0, 0)),
            changed: 0,
            diff: Some((2, 5, 1)),
        };
        assert_eq!(
            standing_said(&w, Some(&even)),
            "fix ← gone · even with it · +5 −1 in 2 files"
        );
        assert_eq!(standing_short(&w, Some(&even)), "fix ← gone · +5 −1");
        // Briefly too, on a card.
        assert_eq!(standing_brief(&w, &lost), ["gone gone", "1 changed"]);
    }
}
