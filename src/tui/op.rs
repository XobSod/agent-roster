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
    EditSession {
        id: String,
        name: String,
        /// None follows the agent's title.
        title: Option<String>,
        /// None: as it was; Some(None): the default.
        model: Option<Option<String>>,
        effort: Option<Option<String>>,
    },
    RetitleSession(String),
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
