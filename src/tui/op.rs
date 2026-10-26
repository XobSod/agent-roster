//! What the TUI asks of the run loop.

/// Work for the run loop, which owns the terminal and the roster.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A setting changed to `input` (as typed), or back to its default.
    ChangeSetting {
        setting: String,
        input: Option<String>,
    },
    /// config.toml opened in `$EDITOR`.
    EditConfig,
    /// A new agent that runs this command line.
    AddAgent(String),
    Attach(String),
    Start(String),
    Stop(String),
    Restart(String),
    Archive(String),
    /// An archived session, for good.
    Delete(String),
    Restore {
        id: String,
        name: Option<String>,
    },
    SetUnread(String, bool),
    NewSession {
        project: String,
        agent: String,
        name: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        prompt: Option<String>,
    },
    /// Only what was changed in the form: None stays as it is.
    EditSession {
        id: String,
        name: Option<String>,
        /// Some(None) follows the agent's title.
        title: Option<Option<String>>,
        /// Some(None): the default.
        model: Option<Option<String>>,
        effort: Option<Option<String>>,
    },
    RetitleSession(String),
    /// Notes as written, in turn, then the editor closed if all were saved
    /// and `close`, else left open.
    SaveNotes {
        saves: Vec<NoteSave>,
        close: bool,
    },
    /// A note in `$EDITOR`: the open editor's text, or the saved note.
    EditNote(crate::note::Whose),
    /// Make sure the chats project exists, then open the new-chat form.
    NewChat,
    /// Look for conversations to adopt, then pick one.
    FindConversations,
    Adopt {
        conversation: String,
        project: String,
        name: Option<String>,
        run_on: crate::adopt::RunOn,
        /// Picked here (Some(None): the defaults).
        model: Option<Option<String>>,
        effort: Option<Option<String>>,
        fork: bool,
        /// Even though it's open in its agent.
        force: bool,
    },
    AddProject {
        name: String,
        path: String,
        agent: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    EditProject {
        id: i64,
        name: String,
        path: String,
        agent: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    RemoveProject(String),
    KillOrphans,
    Quit,
}

/// A note as written in the editor (all blank: none).
#[derive(Clone, Debug, PartialEq)]
pub struct NoteSave {
    pub whose: crate::note::Whose,
    /// "api/fix", "project api", to say what was saved.
    pub label: String,
    pub text: String,
    pub pin: Option<usize>,
    /// The note the editor was opened on, which this replaces (unless
    /// `force`d, not another saved meanwhile).
    pub base: Option<crate::note::Note>,
    pub force: bool,
}
