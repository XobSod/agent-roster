//! What starting a session asks first, or why it can't: decided here once,
//! for the CLI and the TUI to say.

use super::*;

/// How a session is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Starting {
    /// Started, if it's stopped.
    Start,
    /// Attached to, started first if it's stopped.
    Attach,
    /// Stopped if it runs, then started.
    Restart,
}

/// What starting a session asks first, or why it can't.
#[derive(Debug, PartialEq)]
pub enum StartCheck {
    /// Nothing: it goes ahead.
    Go,
    /// Whether to go ahead, asked so.
    Confirm(String),
    /// Whether to give its agent again the first prompt it hasn't said it
    /// took (`Session::given_prompt`), asked so, never on its own, as given
    /// twice it would be acted on twice; either way it starts a new
    /// conversation. `unasked`: what to confirm before going ahead on an
    /// answer given without the question (on the command line), which says
    /// a working one is cut short.
    PromptAgain {
        question: String,
        unasked: Option<String>,
    },
    /// It can't, for this reason.
    Refused(String),
}

/// What starting `s` (`how`) asks first, or why it can't.
pub fn start_check(s: &Session, how: Starting) -> StartCheck {
    let stopped = s.state == State::Stopped;
    match how {
        Starting::Start if !stopped => {
            return StartCheck::Refused(format!("{} is already running", s.label()));
        }
        Starting::Attach if !stopped => return StartCheck::Go,
        _ => {}
    }
    let check = agents_check(s);
    // Its worktree gone (removed by hand, or with roster's directories):
    // made again from its branch where it was, so its conversation, kept
    // by its directory, resumes; asked, as something removed it.
    let Some(w) = s.worktree.as_ref().filter(|w| !w.root.exists()) else {
        return check;
    };
    let gone = format!(
        "The worktree of {}, {}, is gone",
        s.label(),
        w.root.display()
    );
    if !crate::git::branch_exists(&w.repo, &w.branch) {
        return StartCheck::Refused(format!(
            "{gone}, and so is its branch {}: it can't start",
            w.branch
        ));
    }
    let again = format!(
        "{gone}: it's made again from branch {}, where it was.",
        w.branch
    );
    match check {
        StartCheck::Go => StartCheck::Confirm(format!(
            "{gone}. Make it again from branch {}, where it was, and start?",
            w.branch
        )),
        StartCheck::Confirm(question) => StartCheck::Confirm(format!("{again} {question}")),
        StartCheck::PromptAgain { question, unasked } => StartCheck::PromptAgain {
            question: format!("{again} {question}"),
            unasked,
        },
        refused @ StartCheck::Refused(_) => refused,
    }
}

/// What starting `s` asks first for its agent's sake: its first prompt
/// again, a conversation it can't go on with, a turn it cuts short.
fn agents_check(s: &Session) -> StartCheck {
    // Its agent exited first, or is still at a screen it showed first: a new
    // conversation either way, as its agent never said which.
    if s.given_prompt.is_some() {
        return StartCheck::PromptAgain {
            question: agent::prompt_again_question(s),
            unasked: (s.state == State::Working).then(|| working(s)),
        };
    }
    if !agent::resumes(s) {
        return StartCheck::Confirm(agent::new_conversation_question(s));
    }
    // Restarting a working one cuts its turn short.
    if s.state == State::Working {
        return StartCheck::Confirm(working(s));
    }
    StartCheck::Go
}

fn working(s: &Session) -> String {
    format!("{} is working. Restart it?", s.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::session;

    #[test]
    fn starting_asks_one_thing_first_or_refuses() {
        #[derive(Clone, Copy, Debug, PartialEq)]
        enum Asks {
            Go,
            Confirm,
            PromptAgain { unasked: bool },
            Refused,
        }
        let asks = |state, prompt: bool, resumes: bool, how| {
            let mut s = session("s");
            s.state = state;
            s.conversation_id = resumes.then(|| "c-1".to_string());
            s.given_prompt = prompt.then(|| "fix it".to_string());
            match start_check(&s, how) {
                StartCheck::Go => Asks::Go,
                StartCheck::Confirm(_) => Asks::Confirm,
                StartCheck::PromptAgain { unasked, .. } => Asks::PromptAgain {
                    unasked: unasked.is_some(),
                },
                StartCheck::Refused(_) => Asks::Refused,
            }
        };
        use {Starting::*, State::*};
        // Whether to give its first prompt again; with what a working one
        // asks too, answered without it.
        let (again, cut) = (
            Asks::PromptAgain { unasked: false },
            Asks::PromptAgain { unasked: true },
        );
        let cases = [
            // Stopped: what it'd lose asked about, the first prompt first.
            (Stopped, false, true, Start, Asks::Go),
            (Stopped, false, false, Start, Asks::Confirm),
            (Stopped, true, true, Start, again),
            (Stopped, true, false, Start, again),
            (Stopped, false, true, Attach, Asks::Go),
            (Stopped, false, false, Attach, Asks::Confirm),
            (Stopped, true, true, Attach, again),
            (Stopped, false, true, Restart, Asks::Go),
            (Stopped, false, false, Restart, Asks::Confirm),
            (Stopped, true, true, Restart, again),
            // Running: not started again; attached to as it is.
            (Idle, false, false, Start, Asks::Refused),
            (Working, true, true, Start, Asks::Refused),
            (Working, true, false, Attach, Asks::Go),
            // A restart cuts a turn short: asked, once.
            (Idle, false, true, Restart, Asks::Go),
            (NeedsInput, false, true, Restart, Asks::Go),
            (Working, false, true, Restart, Asks::Confirm),
            (Working, false, false, Restart, Asks::Confirm),
            (Idle, true, true, Restart, again),
            (NeedsInput, true, true, Restart, again),
            (Working, true, true, Restart, cut),
        ];
        for (state, prompt, resumes, how, expected) in cases {
            assert_eq!(
                asks(state, prompt, resumes, how),
                expected,
                "{state:?}, prompt {prompt}, resumes {resumes}, {how:?}"
            );
        }
        // Said as each question has it; a working one's restart that would
        // lose its conversation asks about that.
        let mut s = session("s");
        s.conversation_id = Some("c-1".into());
        let StartCheck::Confirm(question) = start_check(&s, Restart) else {
            panic!()
        };
        assert!(question.ends_with("is working. Restart it?"), "{question}");
        s.conversation_id = None;
        let new_one = StartCheck::Confirm(agent::new_conversation_question(&s));
        assert_eq!(start_check(&s, Restart), new_one);
        s.state = Stopped;
        let new_one = StartCheck::Confirm(agent::new_conversation_question(&s));
        assert_eq!(start_check(&s, Start), new_one);
    }

    #[test]
    fn a_worktree_gone_is_asked_about_or_refused_with_its_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        crate::git::tests::repo(&repo);
        let mut s = session("s");
        s.state = State::Stopped;
        s.conversation_id = Some("c-1".into());
        s.worktree = Some(crate::model::Worktree {
            root: tmp.path().join("gone"),
            repo: repo.clone(),
            branch: "main".into(),
            base: None,
            made: true,
            set_up: true,
            trusted_in: None,
        });
        let StartCheck::Confirm(question) = start_check(&s, Starting::Start) else {
            panic!()
        };
        assert!(
            question.ends_with("is gone. Make it again from branch main, where it was, and start?")
        );
        // With what else it asks, in one question.
        s.conversation_id = None;
        let StartCheck::Confirm(question) = start_check(&s, Starting::Start) else {
            panic!()
        };
        assert!(
            question.contains("made again from branch main, where it was. "),
            "{question}"
        );
        assert!(question.ends_with("Start a new one?"), "{question}");
        // Attached to while it runs, nothing.
        s.state = State::Idle;
        assert_eq!(start_check(&s, Starting::Attach), StartCheck::Go);
        // Its branch gone too.
        s.state = State::Stopped;
        if let Some(w) = &mut s.worktree {
            w.branch = "nope".into();
        }
        let StartCheck::Refused(why) = start_check(&s, Starting::Start) else {
            panic!()
        };
        assert!(
            why.ends_with("and so is its branch nope: it can't start"),
            "{why}"
        );
    }
}
