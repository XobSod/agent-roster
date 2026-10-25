# agent-roster: design

How roster should behave, and why. The code is the truth about how it does it; this file is the truth about what it should do. Where the two disagree, one of them is wrong, and which one is decided case by case: this file changes to match the code, or the code to match this file. It stays at the level of features and the decisions behind them; keys, commands and settings are in the README and `roster --help`. It says nothing about what was before or what may come.

Roster is a small, keyboard-first terminal manager for AI coding-agent sessions, written in Rust. One screen: the sessions on the left with their state, the selected one's screen on the right. Enter takes you into the agent, full screen, where you type to it directly; a back key (Ctrl+Q) brings you back. The agents are the real CLIs (Claude Code, pi, any other), running on roster's own tmux server.

---

## 1. Requirements

1. **Several agents.** Claude Code and pi have adapters; any other CLI agent runs as a generic one, tracked by its screen. Tracking can't rely on one agent's hooks.
2. **TUI and CLI parity.** Whatever the TUI does, a command does too, so scripts, and agents themselves, can use roster.
3. **Keyboard-first, no mouse capture**, in the TUI and in roster's tmux.
4. **Keys reach the agent unchanged.** While attached, roster's tmux takes only the back key, in a plain terminal and inside the user's own tmux. So roster's tmux never loads the user's `~/.tmux.conf`: nested inside the user's tmux, its prefix and bindings would take keys meant for the agent.
5. **Read/unread that matches reality**: coming back from a session marks it read, and a turn that ends while you watch counts as seen.
6. **Safe keys.** Nothing destructive, and nothing acting on the agent's behalf, on a single key: actions live in menus, and destructive ones also confirm. Only `y` confirms, so an `Enter` pressed by habit confirms nothing. No quick-approve, no YOLO toggle, no restart with a fresh conversation. Every key of the list and its menus can be rebound or turned off.
7. **The environment is made at every start**, by env files that may run commands, so rotating credentials are fresh each time.
8. **Restarting keeps the conversation**, for agents that can resume, and nothing starts a fresh one silently.
9. **Archiving is recoverable**, and deleting for good is a separate, confirmed step.
10. **Low coupling.** Roster never changes an agent's own config or the user's tmux config: whatever it adds to an agent, it adds for that run, at launch. What it writes lives only in its own XDG directories, and of its own config it writes only what is changed in its settings.
11. **What an agent lacks, roster adds per run, and only that**: pi doesn't report its state, so roster loads an extension of its own into the sessions it starts. Nothing it adds changes how the agent behaves for the user.
12. **Several roster processes at once, without lost work.** The TUI, commands and agents calling roster share one database, and none undoes what another saved meanwhile. One TUI runs at a time: one is enough, and commands run alongside it.
13. **Small scope.** Features come when they're needed.

## 2. Concepts

- **Project**: a name, a directory, and defaults for its sessions (agent, model, effort). Every session belongs to one.
- **Session**: one agent at work in a project: its agent, name, directory, conversation, state, title, model and effort, note.
- **Name**: unique among the project's live sessions, made up as `<agent>-<n>` unless you give one. A given name is often a role (`tester`) reused task after task, so it shows next to the title; a made-up one only stands in for a missing title.
- **Agent**: Claude, pi, or any other CLI configured by its command. Claude is always offered, pi once it's installed, others once configured.
- **Conversation**: what the agent keeps of the exchange, in its own files; roster knows its id and resumes it.
- **State**: working, needs input, idle, stopped; plus unread (§3.3).
- **Archived**: put away with the agent stopped, kept with its conversation and note until restored or deleted for good.
- **Chat**: a session without a project of its own (§3.11).
- **Title**: what the session is about: the agent's, or one of yours, which wins.
- **Note**: free text per session, such as what's left to do in it.

## 3. Running agents

### 3.1 Processes and tmux
- **One binary**: `roster` alone opens the TUI, its subcommands are the CLI, and agents' hooks call `roster report`. **No daemon**: the agents live on roster's tmux server and their reports go straight into the database, so nothing is lost while no TUI runs.
- **A private tmux server** (tmux 3.2 or later), one tmux session per roster session, so they never show in the user's own tmux. Its config is roster's own: no prefix, no bindings but the back key, mouse off, keys passed on as the agents ask for them. It never loads the user's `~/.tmux.conf`; a `tmux.conf` in roster's config directory adds to it, and a prefix or copy mode go there if wanted. It starts without the variables that tie a process to another tmux or to a Claude Code session, so nothing in it takes itself for being inside either.
- A changed config reaches a running server: whenever a roster process starts, and at once for a change of roster's own settings in a running TUI. A mistake in the user's `tmux.conf` is said once, whether a running server reloads the file or a server starts with it, by the next roster you run (roster's own helpers in the background leave it to that one), and roster goes on without it. Saying it again would mean running the file again, which adds twice what it adds; the file is read again once it changes, or when roster's back key or status line changes, which takes reading the whole config again.
- **Attaching** is a plain `tmux attach`: no pty in between and no key filtering, since the back key is a tmux binding. Nothing flashes between roster's screen and the agent's.
- **A status line while attached** shows the project, name, title, agent, model and effort, and the back key. It's set as you attach, so it leaves out the state, which changes while you're there, and a title the agent picks meanwhile shows from the next attach. Why: attached, nothing said which project or task you were in. It can be turned off.
- **Size**: a session is created at the terminal's size, so its output isn't wrapped at 80×24 before the first attach.
- **Agents drawing full screen are fitted to the preview** while no one is attached, so the preview shows all of their screen. Only those: agents on the normal screen garbled scrolling back through their history after a resize. It can be turned off, for programs that redraw badly and for taste: a fitted preview's layout differs from the attached screen's. A resize's redraw isn't taken for activity.
- **Scrolling back and copying** happen in roster's tmux, which has the agent's history; the outer terminal has only the current screen. Copy mode is the user's to add, and the examples show how, including forwarding the user's own tmux's copy-mode key into roster's.
- **Stopped**: the pane outlives the agent, keeping its last screen and exit status. A session whose tmux session is gone (after a reboot) is stopped too.

### 3.2 Launching
- Every start, restart included, runs the whole launch: a POSIX shell (the user's, if it is one; not login, not interactive, so no rc files) sources the global env file, then the project's (`.agent-roster.env` in its directory), then runs the agent. Values are exported, and a failing command stops the start (unless an `export` in front hides its failure): its error stays on the screen and the session shows stopped. A running agent keeps the environment it started with, so credentials that expire mid-session need a helper the agent calls itself.
- Roster picks the session's id, and for agents that take one the conversation's, up front. The session's identity and roster's own path reach the agent through its environment, so its hooks can report.
- Arguments: the configured ones, then the session's model and effort, so the session wins, then what the adapter needs.
- **Claude** gets roster's hooks through `--settings` for that run, alongside the user's and the project's, and resumes its conversation once there is one. **pi** gets roster's extension through `-e` for that run.

### 3.3 State
| State | Meaning | From |
|---|---|---|
| **working** | in a turn | the agent's reports; else its screen changing |
| **needs input** | waiting for you mid-turn (a permission, a question) | the agent's reports; agents that can't report it show idle |
| **idle** | the turn is over, waiting for a prompt | the agent's reports; else its screen still |
| **stopped** | no agent running, for whatever reason | tmux |

- **Reports first, the screen second.** An agent with an adapter reports a turn's start and end, a question, its conversation and model; its screen only corrects an end that went unreported. A generic agent is tracked by its screen alone: changing means working, still for a few seconds means idle.
- **Esc fires no hook**, so a reporting agent that's working, with no new report and its screen unchanged for 4 s, turns idle, and not unread: you interrupted it yourself. This is reliable for Claude, whose spinner changes the screen as long as it works.
- **Unread** is set when a session turns idle or needs input, and cleared when you come back from it, when you start or restore it, or by hand. Looking at the preview doesn't clear it.
- Claude's `Notification` hook isn't used for needs input: it comes about 6 s after the permission prompt, maybe after it was answered.
- On its first start in a directory, Claude asks whether to trust it before any hook fires, so the session shows working, then idle once its screen is still.
- **The reporter never fails**: it's silent and exits 0 even when the database is locked or broken, since for Claude a hook exiting with 2 means "keep going". An event it can't store is dropped; a missed end of a turn is caught from the screen.

### 3.4 First prompt
- A new session can get a first prompt, typed into the agent once it's ready: for an agent that reports, once it's idle and has reported its conversation; for others, once its screen is still.
- **Typed as a bracketed paste, a pause, then Enter**: typed as keys with Enter straight after, Claude didn't submit a long prompt. **Not on Claude's command line**: a one-word prompt would run a subcommand.
- **Typed once**, by whichever roster process gets to it. One that couldn't be pasted is tried again a few times, then left to another roster process or the next start; one pasted whose Enter failed isn't, since typing it twice is worse. Failures are said. While you're attached nothing refreshes, so a hidden roster process types it.

### 3.5 Start, stop, archive, delete
- **Start** and **restart** run the whole launch; an agent that resumes goes on with its conversation. For agents that can't resume (generic ones), and for a session whose conversation roster doesn't know (Codex names it from its first turn on, through hooks that may not run), roster asks before starting a new conversation.
- **Stop** ends the agent (SIGTERM, then SIGKILL); the pane stays, with its last screen. Stopping or restarting a working session asks first, since both cut its turn short.
- **Archive** stops the agent, closes its tmux session and puts the session on the Archived shelf, with its conversation and note. **Restore** brings it back stopped (if its name was taken meanwhile, you give it another); starting it then resumes. An archived session's note can still be read and edited; nothing else acts on it.
- **Delete for good**, for archived sessions only, confirmed: the session, its events and its note go. The conversation stays where the agent keeps it, and can be adopted again.
- **Removing a project** needs it to have no live sessions, and deletes its archived ones for good, so it asks first, saying how many.
- After a reboot every session is stopped; starting one resumes it.

### 3.6 Model and effort
- Per session. A new session takes its own, else its project's (if the project runs the same agent), else the agent's from the config, else none, leaving it to the agent's own settings. "Default", wherever it's chosen, is the agent's from the config, else the agent's own. Changes apply on the next start. Generic agents take neither.
- **What shows is what the agent runs with**: what it reported, else what its start asked for, so an edit shows from the next start.
- **Roster learns what each alias and "default" turned out to be** (`opus · Opus 5.5`), only from the agent's own start, with what that start asked for: not from a conversation cleared or resumed inside it, which may have switched model meanwhile, and not from the session's model as it is now, which an edit may have changed since.
- **Forms offer the agent's models**: Claude's from roster's own list, since Claude can't list its own; pi's from pi, asked in the background. Any other name can be typed.

### 3.7 Titles
- **The agent's own** (Claude's terminal title, pi's session name), or **one of yours**, which wins; taking yours off follows the agent's again.
- **Generated on request**: the session's own agent, run one-off with no tools (or the fewest it allows, none of them able to act) and nothing kept, names what its recent prompts are about. It's kept as your title, since Claude would overwrite its own. It's made in the background, and kept only if the session is still as it was when asked: the same conversation, and no title of yours given or taken off meanwhile. Generic agents can't get one, as roster can't read their conversations.
- **Automatic**, for agents that don't name their task (pi): once per conversation, after its first turn, made the same way and kept as the agent's title, so the agent's own naming still replaces it. One that fails isn't tried again by itself. It can be turned off.

### 3.8 Notes
- Free text per session, kept through stops, restarts and archiving, gone only with the session. Cards mark sessions with one.
- **Edited inside roster**, under the preview's header with the agent's screen still below, in an editor with vim's keys; or in `$EDITOR`. `Esc` doesn't close it, so pressing it once too often is harmless.
- **The pin**: the pinned line and the one below it show above the agent's screen, so a to-do list shows where you are in it.
- **A note changed meanwhile isn't saved over** (an agent ticking a box through `roster note`, say): the save is refused unless you force it, and what you wrote in `$EDITOR` is kept in its file.

### 3.9 Statistics
- For every agent, the session's age, the time spent working, and its turns, or its prompts for an agent whose conversation roster reads. Such an agent adds what its conversation holds: context used of its window, output tokens, cost, lines changed. What's unknown is left out, not guessed. Shown under the preview's header, for archived sessions too.
- The conversation is read incrementally, and the first read must be quick: an 80 MB Claude transcript is normal.

### 3.10 Adopting conversations
- Taking over a Claude or pi conversation started outside roster (plain `claude` or `pi`, an IDE, another session manager), so it goes on as a session here. They're picked from the agents' own files, newest first, with a search: those in your projects' directories, or all of them.
- **Move** (the default): into the project holding its directory, else a new project for that directory; the session runs there and resumes it. **Copy**: a fork, leaving the original untouched.
- **Open elsewhere**: a running Claude leaves a sign, and moving its conversation asks first, since two agents would write one conversation; a copy doesn't. pi leaves none, so roster can't warn.
- Model and effort: what it last ran with, the defaults, or ones you pick.
- Conversations of live sessions aren't offered; an archived session's is, for restoring.

### 3.11 Chats
- A session without a project of its own, to just talk to an agent. A session still needs a directory (it decides where the agent keeps its conversation, which instructions it reads, where its tools act, which folder it asks you to trust), defaults and an address, which is what a project is. So chats belong to a **built-in chats project**, made on first use, which can be renamed and given defaults.
- **One directory for all chats**, in roster's state directory unless configured: one directory means trusting it once, and uninstalling stays "remove both directories".

## 4. Storage
- **Config** in `$XDG_CONFIG_HOME/agent-roster/`, all optional: `config.toml`, the global env file, the user's `tmux.conf`. An unknown setting is an error, so a setting added later stops an older build instead of being ignored. Changes on disk are used at once.
- **Roster writes `config.toml` only for a change made in its settings**, and only that setting (taking one out takes its comment with it): the rest of the file stays as written, comments included; what was edited outside meanwhile is kept; and a symlinked file stays where it points. The one other write is the template, when you ask to edit a file that isn't there. Why: the settings should configure everything, and a config kept in git shows what changed.
- **State** in `$XDG_STATE_HOME/agent-roster/`: the database (SQLite), the files roster makes for itself (the generated tmux config, pi's extension, notes being edited, the TUI's lock), and the chats' directory. An older database is migrated when opened; a newer one is refused.
- **Uninstalling**: the binary, both directories, and roster's tmux server.
- **Several processes**: reports are stored as events, which every roster process folds into sessions; roster's own changes fold pending events first, so an older event can't undo them. A save writes only what it changed, so nothing reverts what another process saved meanwhile. Where you and a process may both write the same thing (a note, a generated title), the write happens only if it's still what it was.

## 5. TUI
- **Layout**: a narrow sidebar with a summary of what waits ("2 need input · 1 working") and the list; the preview on the right; the footer below. No boxes around them, only a line between them; popups, forms and the note editor are boxed.
- **Shelves, by attention** (the default): at the top what waits for you (Needs input: questions, permissions and finished turns you haven't read; Idle; Stopped), at the bottom what's handed off or finished (Working, then Archived, folded). Why: working is "go away, I gave it a task", stopped is idle without a running agent, archived is finished. Grouping by project is the alternative.
- **Within a shelf, first come, first served**: oldest first, so a card you're reading isn't pushed down by newcomers; in Needs input, questions and permissions come first. Reports that don't change a session's state don't move it. Each shelf's order can be changed, being a matter of taste.
- **The cursor follows its session**, except when you come back from giving it a task: then it keeps its row, landing on what took the session's place, usually the next one waiting. Why: answering an agent sent its card, and the cursor with it, to the bottom; going by whether the card changed shelf would also jump after only reading. Lists wrap around.
- **Cards**: the project, as a coloured badge and its name in that colour, since the badge alone didn't tell projects apart; the title and the state; the agent, name, model and effort. Stopped and archived sessions are one-line rows.
- **The preview**: a header (project, name, title, agent, model, effort, state) and the statistics, then the live session's screen, read-only. Lines too wide are cut, not wrapped: wrapping breaks agents' box drawing. A stopped or archived session shows a panel of what it was and what starting it goes on with, including the end of its screen after an exit with an error, where an env file's error is. A heading shows its sessions at a glance. No key hints in a session's preview: they only repeated the footer and the menus. Where nothing else says what can be done (nothing to select, a project's details), the preview says it.
- **Filter**: `/` narrows the list fuzzily, fzf-style, keeping the shelves. The summary at the top still counts every session, so nothing waiting is missed.
- **Views**: the projects; the tmux server and database (§7); the settings, where every setting shows with what it does and changes apply at once. In a view, `q` goes back, since quitting from a view surprised.
- **The footer** shows the keys that act where you are, with messages beside them rather than over them. Help shows the keys of the list and its menus.
- **Keys**: vim-style moving at the top level; the list's actions in three menus (session, project, view), where a letter means the same in each (`n` new, `e` edit, `d` archive or remove). The back key must be one an agent rarely needs: with Ctrl or Alt, or an F key, and not one a terminal sends for something else (Ctrl+M is Enter, Ctrl+I Tab).

## 6. CLI
- Every TUI action has a command (§1.2), and `roster ls --json` is for scripts.
- The CLI asks where the TUI asks; `--yes` answers for scripts and agents, and without a terminal it refuses rather than guess. Where the TUI offers something else instead (another name for a restore, restoring an archived session rather than adopting its conversation), the CLI says what to run.
- A session is addressed by its id, `project/name`, its name, or an id prefix; an address that fits several is an error.
- A few commands are roster's own: the hooks' reports, typing a first prompt while you're attached, automatic titles. A log of reports is for debugging.

## 7. Diagnostics
- `roster doctor` and the server view check roster's tmux server and the database against each other: whether the server runs roster's config and was started clean (no other tmux's or Claude Code's variables), each tmux session against its roster session, and the database's state. The problems are a server without roster's config, a server not started clean, and orphans (tmux sessions no live roster session owns), which can be killed after asking. `roster doctor` exits with 1 while problems remain.

## 8. Left out on purpose
- **Telling an answered permission prompt.** Claude has no hook for an approval, so a session stays in Needs input until the approved tool finishes. Most people run agents in auto mode, and recognising Claude's dialog on its screen isn't worth it.
- **Jumping to the next waiting session while attached.** The list in between is the loop: attached, you don't see what's next.
- **A live side panel** (the agent's pane beside the TUI): `join-pane` works only within one tmux server, and the TUI isn't on roster's. Fitting agents to the preview does it instead.
- **Ignoring parts of a generic agent's screen**: all of it counts, so an agent that redraws while idle (a clock) flips between working and idle.
- **tmux control mode or file watching**: polling once a second is enough.
- **macOS and other systems**: roster runs on Linux.
- **Narrow races, accepted**: a refresh writes the state it observed, so in a narrow window another process's fold can lose to it; a first prompt claimed but not yet typed is lost if roster is killed in between; a resize's redraw may count as activity once for another roster process; a start reported after a quick edit of its model is learned under the edited alias; Codex reports its start with its first turn, so a model switched inside it before the first prompt is learned as what its start asked for.

---

## Appendix: what roster relies on

Checked against the real programs (tmux 3.5a, Claude Code 2.1.303 and later, pi 1.4.0, Codex 0.163.0). A change in one of these can break roster.

**tmux**
- Extended keys and `new-session -e` exist from 3.2.
- `resize-window`, then unsetting `window-size`, keeps the new size until a client attaches, which then sizes the window as usual.
- An attaching client draws through the terminal it opens itself.
- A server that started with mistakes in its config keeps them, and the next `source-file` prints them, once.
- A key the outer tmux binds without a prefix is taken there before it reaches roster, as in any pane.

**Claude Code**
- Hooks given with `--settings` fire alongside the user's and the project's own.
- Every hook payload names the conversation and its transcript; `SessionStart` says whether it started, resumed, cleared or compacted one, and the model only for a new conversation.
- Esc during a turn fires no hook; an approved permission fires none until the tool has run; `Notification` comes about 6 s after a permission prompt.
- `--session-id` fails once the conversation has messages, `--resume` gets it back, and `--fork-session` copies it, written with its first message.
- A one-word prompt on the command line runs a subcommand, `--` notwithstanding; a long prompt typed with Enter straight after isn't submitted, but a bracketed paste, a pause, then Enter is.
- Working, its screen changes at every sample; idle, it stays identical.
- Its terminal title names its task after a glyph (`✳`, or another such as `⠐`) from the first prompt on.
- A running Claude keeps `<claude dir>/sessions/<pid>.json`, removed on exit.
- On its first start in a directory it asks whether to trust the folder, before any hook.
- Its classic renderer draws on the normal screen, where a resize garbles scrolling back; the fullscreen one draws in the alternate screen.

**pi**
- `-e <file>` loads an extension for that run, alongside the user's own; `--session-id` opens a conversation or creates it; `--fork` copies one; `--list-models --offline` lists its models.
- `agent_end` can be followed by retries; `agent_settled` comes once the turn is really over.
- It leaves no sign that a conversation is open.

**Codex**
- Hooks given with `-c hooks.<Event>=[…]` are a layer of their own and fire alongside the user's and the project's, whether those are in `config.toml` or `hooks.json`, though the merged config shows only the `-c` ones.
- `codex exec` always offers the model `exec` and `request_user_input`, whatever is turned off; with `code_mode_host` off, `exec` fails at once, and in a read-only sandbox `apply_patch` is refused. The user's own `AGENTS.md` comes whatever is turned off; `project_doc_max_bytes=0` keeps out the project's.
- An unknown feature is an error for `--disable`, a warning for `-c features.<name>=false`.
- New or changed hooks are reviewed at its start, and a prompt on its command line waits for the answer: trusted, its first turn reports; reviewed one by one or not trusted, it runs without them. Every hook payload names the conversation, so the next turn that reports says it.
- Its rollout gives the model context as user messages too. Since 0.157 it marks each message's parts with their kinds (`user.text` for a prompt); before, it marked none and wrote each prompt again as a `user_message` event, which 0.124 also wrote beside marked messages. A prompt given on its command line is a prompt like any other.
