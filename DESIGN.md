# agent-roster: design

How roster should behave, and why. The code is the truth about how it does it; this file is the truth about what it should do. Where the two disagree, one of them is wrong, and which one is decided case by case: this file changes to match the code, or the code to match this file. It stays at the level of features and the decisions behind them; keys, commands and settings are in the README and `roster --help`, and how a feature is done is in the code. It says nothing about what was before or what may come.

Roster is a small, keyboard-first terminal manager for AI coding-agent sessions, written in Rust. One screen: the sessions on the left with their state, the selected one's screen on the right. Enter takes you into the agent, full screen, where you type to it directly; a back key (Ctrl+Q) brings you back. The agents are the real CLIs (Claude Code, pi, Codex, any other), running on roster's own tmux server.

---

## 1. Requirements

1. **Several agents.** Claude Code, pi and Codex have adapters; any other CLI agent runs as a generic one, tracked by its screen. Tracking can't rely on one agent's hooks.
2. **TUI and CLI parity.** Whatever the TUI does, a command does too, so scripts, and agents themselves, can use roster.
3. **Keyboard-first, no mouse capture**, in the TUI and in roster's tmux.
4. **Keys reach the agent unchanged.** While attached, roster's tmux takes only the back key, in a plain terminal and inside the user's own tmux. So roster's tmux never loads the user's `~/.tmux.conf`: nested inside the user's tmux, its prefix and bindings would take keys meant for the agent.
5. **Read/unread that matches reality**: coming back from a session marks it read, and a turn that ends while you watch counts as seen.
6. **Safe keys.** Nothing destructive, and nothing acting on the agent's behalf, on a single key: actions live in menus, and destructive ones also confirm. Only `y` confirms, so an `Enter` pressed by habit confirms nothing. No quick-approve, no YOLO toggle, no restart with a fresh conversation. Every key of the list and its menus can be rebound or turned off.
7. **The environment is made at every start**, by env files that may run commands, so rotating credentials are fresh each time.
8. **Restarting keeps the conversation**, for agents that can resume, and nothing starts a fresh one silently.
9. **Archiving is recoverable**, and deleting for good is a separate key, confirmed.
10. **Low coupling.** Roster never changes the user's tmux config, nor an agent's own config except to trust a session's folder, when a setting that's off by default asks it to (§3.2). Whatever it adds to an agent, it adds for that run, at launch. Otherwise what it writes lives only in its own XDG directories, and of its own config it writes only what is changed in its settings.
11. **What an agent lacks, roster adds per run, and only that**: pi doesn't report its state, so roster loads an extension of its own into the sessions it starts; Codex gets hooks of its own the same way. Nothing it adds changes how the agent behaves for the user.
12. **Several roster processes at once, without lost work.** The TUI, commands and agents calling roster share one database, and none undoes what another saved meanwhile. One TUI runs at a time: one is enough, and commands run alongside it.
13. **Small scope.** Features come when they're needed.

## 2. Concepts

- **Project**: a name, a directory, defaults for its sessions (agent, model, effort), and a note. Every session belongs to one.
- **Session**: one agent at work in a project: its agent, name, directory, conversation, state, title, model and effort, note.
- **Name**: unique among the project's live sessions, made up as `<agent>-<n>` unless you give one. A given name is often a role (`tester`) reused task after task, so it shows next to the title; a made-up one only stands in for a missing title.
- **Agent**: Claude, pi, Codex, or any other CLI configured by its command. Claude is always offered, pi and Codex once installed, others once configured.
- **Conversation**: what the agent keeps of the exchange, in its own files; roster knows its id and resumes it.
- **State**: working, needs input, idle, stopped; plus unread (§3.3).
- **Archived**: put away with the agent stopped, kept with its conversation and note until restored or deleted for good.
- **Chat**: a session without a project of its own (§3.11).
- **Title**: what the session is about: the agent's, or one of yours, which wins.
- **Note**: free text per session, such as what's left to do in it, and per project, such as what's left to do there.

## 3. Running agents

### 3.1 Processes and tmux
- **One binary**: `roster` alone opens the TUI, its subcommands are the CLI, and agents' hooks call `roster report`. **No daemon**: the agents live on roster's tmux server and their reports go straight into the database, so nothing is lost while no TUI runs.
- **A private tmux server** (tmux 3.2 or later), one tmux session per roster session, so they never show in the user's own tmux. Its config is roster's own: no prefix, no bindings but the back key, mouse off, keys passed on as the agents ask for them. It never loads the user's `~/.tmux.conf`; a `tmux.conf` in roster's config directory adds to it, and a prefix or copy mode go there if wanted. It starts without the variables that tie a process to another tmux or to a Claude Code session, so nothing in it takes itself for being inside either.
- A changed config reaches a running server: whenever a roster process starts, and at once for a change of roster's own settings in a running TUI. A mistake in the user's `tmux.conf` is said once, by the next roster you run, and roster goes on without it. Why once: saying it again would mean running the file again, which adds twice what it adds.
- **Attaching** is a plain `tmux attach`: no pty in between and no key filtering, since the back key is a tmux binding. Nothing flashes between roster's screen and the agent's.
- **A status line while attached** shows the project, name, title, agent, model and effort, and the back key. It's set as you attach, so it leaves out the state, which changes while you're there, and a title the agent picks meanwhile shows from the next attach. Why: attached, nothing said which project or task you were in. It can be turned off.
- **Size**: a session is created at the terminal's size, so its output isn't wrapped at 80×24 before the first attach.
- **Agents drawing full screen are fitted to the preview** while no one is attached, so the preview shows all of their screen. Only those: agents on the normal screen garbled scrolling back through their history after a resize. It can be turned off, for programs that redraw badly and for taste: a fitted preview's layout differs from the attached screen's. A resize's redraw isn't taken for activity.
- **Scrolling back and copying** happen in roster's tmux, which has the agent's history; the outer terminal has only the current screen. Copy mode is the user's to add.
- **Stopped**: the pane outlives the agent, keeping its last screen and exit status. A session whose tmux session is gone (after a reboot) is stopped too.

### 3.2 Launching
- Every start, restart included, runs the whole launch: a POSIX shell (the user's, if it is one; not login, not interactive, so no rc files) sources the global env file, then the project's (`.agent-roster.env` in its directory), then runs the agent. Values are exported, and a failing command stops the start: its error stays on the screen and the session shows stopped. A running agent keeps the environment it started with, so credentials that expire mid-session need a helper the agent calls itself.
- Roster picks the session's id, and for agents that take one the conversation's, up front. Codex takes none: it names its conversation, and reports it from its first turn on. The session's identity and roster's own path reach the agent through its environment, so its hooks can report.
- Arguments: the configured ones, then the session's model and effort, so the session wins, then what the adapter needs.
- **A session's folder can be trusted** in its agent as it starts (off by default), so Claude and Codex don't ask first. They keep trust only in their own config, so roster writes it there, as answering them would: that folder's entry alone, the rest of the file kept as written. It's written once the env files are sourced, so it goes to the config the agent will read, and sessions starting at once keep each other's. A folder Codex was told not to trust stays untrusted, as does one in a repository it was told not to trust; in a trusted repository nothing is written. What fails is said in the pane, and the agent asks.
- **Claude** gets roster's hooks through `--settings` for that run, alongside the user's and the project's, and resumes its conversation once there is one. **pi** gets roster's extension through `-e` for that run. **Codex** gets roster's hooks as `-c` arguments for that run, alongside the user's and the project's, and resumes its conversation by id once it has reported one. It runs without its shared server, so its tools get the session's environment, the env files' included, rather than the server's.

### 3.3 State
| State | Meaning | From |
|---|---|---|
| **working** | in a turn | the agent's reports; else its screen changing |
| **needs input** | waiting for you mid-turn (a permission, a question) | the agent's reports; agents that can't report it show idle |
| **idle** | the turn is over, waiting for a prompt | the agent's reports; else its screen still |
| **stopped** | no agent running, for whatever reason | tmux |

- **Reports first, the screen second.** An agent with an adapter reports a turn's start and end, a question, its conversation and model; its screen only corrects an end that went unreported. A generic agent is tracked by its screen alone: changing means working, still for a few seconds means idle.
- **Esc fires no hook**, so a reporting agent that's working, with no new report and its screen unchanged for 4 s, turns idle, and not unread: you interrupted it yourself. This is reliable for Claude, whose spinner changes the screen as long as it works. Codex's interrupted turn ends the same way, as does one that fails, which fires no hook at all.
- **A compaction isn't a start.** Claude and Codex report a compacted conversation as they report a start, often mid-turn when the context is full; it leaves the state as it was, and counts no turn. One you ask for while it waits for a prompt is work, though no turn: working while it runs, waiting again once done, with nothing new to read.
- **A subagent's reports aren't its session's.** Claude and Codex fire their hooks inside subagents too: no turn starts from them, even after the parent's turn has ended, and they change nothing of the session but this: a subagent's question needs your input until that subagent goes on. A question the session asked itself no subagent clears.
- **Subagents left at work keep the session working.** A turn of Claude's or Codex's that ends while subagents it started run on in the background leaves the session working, not waiting for you and not unread: the agent takes their results up itself as they come. It's idle, and unread, once the last one stops; its screen, still meanwhile, is no sign of the end. A subagent's stop that went unreported is caught from what the agent writes of it; one never heard of again (killed) is taken for gone once unheard of for longer than a tool call may take, its question with it, and all are once the agent exits or starts again. Esc ends Claude's subagents with its turn; Codex's finish theirs, the session working till then. pi's subagents run within a tool call, so its turn waits for them itself.
- **Unread** is set when a session turns idle or needs input, and cleared when you come back from it, when you start or restore it, or by hand. Looking at the preview doesn't clear it.
- Claude's `Notification` hook isn't used for needs input: it comes about 6 s after the permission prompt, maybe after it was answered.
- On its first start in a directory, Claude asks whether to trust it before any hook fires, so the session shows working, then idle once its screen is still. Codex reports nothing before its first turn, so until then it's tracked the same way.
- **The reporter never fails**: it's silent and exits 0 even when the database is locked or broken, since for Claude a hook exiting with 2 means "keep going". An event it can't store is dropped; a missed end of a turn is caught from the screen.

### 3.4 First prompt
- A new session can get a first prompt, typed into the agent once it's ready: for an agent that reports, once it's idle and has reported its conversation; for others, once its screen is still.
- **Typed as a bracketed paste, a pause, then Enter**: typed as keys with Enter straight after, Claude didn't submit a long prompt. **Not on Claude's command line**: a one-word prompt would run a subcommand.
- **Codex gets it on its command line** instead: it reports nothing before its first turn, so it never says it's ready, and what it may show first (a folder to trust, roster's hooks to review, an update, which Enter installs) takes typed keys. On its command line the prompt waits for those to be answered. It can be as long as Linux takes one argument; a longer one is refused as the session is made or started.
- **Until Codex reports its first turn, its prompt is kept**: if it exits first (its trust or update screen declined), the next start, restart or attach asks whether to give it again rather than doing either on its own, since Codex may have taken it unseen (its hooks not trusted), and given twice it would act twice. Either way it's a new conversation, Codex never having said which it's on. The CLI asks too, or is told.
- **Typed once**, by whichever roster process gets to it. One that couldn't be pasted is tried again a few times, then left to another roster process or the next start; one pasted whose Enter failed isn't, since typing it twice is worse. Failures are said. While you're attached nothing refreshes, so a hidden roster process types it.

### 3.5 Start, stop, archive, delete
- **Start** and **restart** run the whole launch; an agent that resumes goes on with its conversation. For agents that can't resume (generic ones), and for a session whose conversation roster doesn't know (Codex names it from its first turn on, through hooks that may not run), roster asks before starting a new conversation.
- **Stop** ends the agent (SIGTERM, then SIGKILL); the pane stays, with its last screen. Stopping or restarting a working session asks first, since both cut its turn short.
- **Archive** stops the agent, closes its tmux session and puts the session on the Archived shelf, with its conversation and note. **Restore** brings it back stopped (if its name was taken meanwhile, you give it another); starting it then resumes. An archived session's note can still be read and edited; nothing else acts on it.
- **Delete for good**, archived or not, confirmed: the agent stops, and the session, its events and its note go. The conversation stays where the agent keeps it, and can be adopted again: why deleting needn't go through archiving.
- **Removing a project** needs it to have no live sessions, and deletes its archived ones for good and its note, so it asks first, saying how many and whether it has a note.
- After a reboot every session is stopped; starting one resumes it.

### 3.6 Model and effort
- Per session. A new session takes its own, else its project's (if the project runs the same agent), else the agent's from the config, else none, leaving it to the agent's own settings. "Default", wherever it's chosen, is the agent's from the config, else the agent's own. Changes apply on the next start. Generic agents take neither.
- **What shows is what the agent runs with**: what it reported, else what its start asked for, so an edit shows from the next start.
- **Roster learns what each alias and "default" turned out to be** (`opus · Opus 5.5`), only from the agent's own start, with what that start asked for: not from a conversation cleared or resumed inside it, which may have switched model meanwhile, and not from the session's model as it is now, which an edit may have changed since.
- **Forms offer the agent's models**: Claude's from roster's own list, since Claude can't list its own; pi's and Codex's from the agent, asked in the background. Any other name can be typed.

### 3.7 Titles
- **The agent's own** (Claude's terminal title, pi's session name, the name Codex gives a conversation), or **one of yours**, which wins; taking yours off follows the agent's again.
- **Generated on request**: the session's own agent, run one-off with no tools able to act and nothing kept, names what its recent prompts are about. It's kept as your title, since Claude would overwrite its own. It's made in the background, and kept only if the session is still as it was when asked: the same conversation, and no title of yours given or taken off meanwhile. Generic agents can't get one, as roster can't read their conversations.
- **Automatic**, for agents that don't name their task (pi): once per conversation, after its first turn, made the same way and kept as the agent's title, so the agent's own naming still replaces it. One that fails isn't tried again by itself. It can be turned off.

### 3.8 Notes
- Free text per session, kept through stops, restarts and archiving, gone only with the session; and per project, gone only with the project. Why both: a session's note ends with its task, a project's outlives every session in it. Sessions and projects with a note are marked wherever they show.
- **The preview shows them side by side** above the agent's screen, in a faint box: the project's pinned lines on the left, the session's on the right, so two notes take the rows of one. Why: stacked, they took twice the rows and read as one list; without a box, they read as part of the agent's screen. Where the preview is too narrow for two, they're stacked. A note that isn't there takes no room, and one the preview already shows whole (a stopped session's) isn't repeated.
- **Edited inside roster**, under the preview's header with the agent's screen still below, in an editor with vim's keys; or in `$EDITOR`. `Esc` doesn't close it, so pressing it once too often is harmless.
- **The pin**: the pinned line and the ones below it show above the agent's screen, so a to-do list shows where you are in it. How many lines show can be set: more push the agent's screen down.
- **Both open at once**: the editor holds the session's note and its project's as vim holds buffers, one shown at a time, each keeping its changes while the other shows; closing refuses while the one not shown has unsaved changes, as vim does with a hidden buffer. Opened on a project, only the project's is there.
- **A note changed meanwhile** (an agent ticking a box through `roster note`, say) is taken as it is while you've changed nothing and aren't in the middle of an edit, and otherwise said as soon as roster sees it. Saving merges it with yours: changes to different lines are both kept; where both changed the same lines, nothing is saved, and both versions go into your text, marked, for you to settle by hand. Why: either way, nothing of yours or the agent's is lost. You can also take the note as it is, leaving yours, or save yours over it. What you wrote in `$EDITOR` that can't be merged is kept in its file.

### 3.9 Statistics
- For every agent, the session's age, the time spent working, and its turns, or its prompts for an agent whose conversation roster reads. Such an agent adds what its conversation holds: context used of its window, output tokens (its subagents' too), cost, lines changed. A fork counts what it spent itself: the copy of the conversation it came from is in its context, and in none of the rest. What's unknown is left out, not guessed. Shown under the preview's header, for archived sessions too.
- The conversation is read incrementally, and the first read must be quick: an 80 MB Claude transcript is normal.

### 3.10 Adopting conversations
- Taking over a Claude, pi or Codex conversation started outside roster (plain `claude`, `pi` or `codex`, an IDE, another session manager), so it goes on as a session here. They're picked from the agents' own files, newest first, with a search: those in your projects' directories, or all of them.
- **Move** (the default): into the project holding its directory, else a new project for that directory; the session runs there and resumes it. **Copy**: a fork, leaving the original untouched.
- **Open elsewhere**: a running Claude leaves a sign, and moving its conversation asks first, since two agents would write one conversation; a copy doesn't. pi and Codex leave none roster can read, so roster can't warn.
- Model and effort: what it last ran with, the defaults, or ones you pick.
- Conversations of live sessions aren't offered; an archived session's is, for restoring. Nor are subagents' threads, which are part of their parent's conversation.

### 3.11 Chats
- A session without a project of its own, to just talk to an agent. A session still needs a directory (it decides where the agent keeps its conversation, which instructions it reads, where its tools act, which folder it asks you to trust), defaults and an address, which is what a project is. So chats belong to a **built-in chats project**, made on first use, which can be renamed and given defaults.
- **One directory for all chats**, in roster's state directory unless configured: one directory means trusting it once, and uninstalling stays "remove both directories".

### 3.12 Usage
- **What the agents' accounts have used of their limits**: for each window (five hours, a week), how much is left and when it starts over. What's left rather than what's used: it's what you work with, running down as you use it, as a battery does. Claude and Codex say; pi and generic agents don't, and don't show.
- **Asked of each agent itself**, so it shows with no session running: Claude, answering what its `/usage` shows; Codex, its own server. Roster never reads their logins: each agent asks its own service with its own. Each in the environment the agent's sessions get, so an env file pointing an agent at another account points its usage there too. An agent with no login to such an account (Claude on an API key) shows nothing; one that can't be asked says why, keeping what it said before.
- **Codex also writes it into its rollouts**, so a running Codex session keeps it current between asks, for nothing. It holds as of when Codex wrote it, so an old figure never undoes a newer answer.
- **When**: at the start, every few minutes while it shows, when the view opens, and on request; in the background, so the TUI never waits on a service. What's old says when it's from, and has no pace.
- **Never, if turned off**: no agent started, nothing sent, by the TUI or `roster usage`, which say how to turn it back on. On by default, being what the view and the sidebar are for.
- **How it's going**, not only how much: each window is used slower than its time goes by (more could be used before it starts over), about as fast, faster (at this rate it runs out first), or it's used up, each with a mark and a colour, the colours told apart with deuteranopia. A window that has started over since, or doesn't say when it does, or was known long ago, has no pace. Marks alone in the sidebar, where words don't fit; words beside them elsewhere, and no forecast.

### 3.13 Spent
- **What the agents spent**: tokens of each kind (input beyond the cache, read from the cache, written to it, output) and what they cost at the API's prices, in the last 24 hours, 7, 30 or 90 days: in all, by agent (with how many conversations, and a chart of when), by model. Everything on the machine counts, started in roster or not: it's for knowing how much was used, not where.
- **Read from the agents' own conversations**, call by call: Claude's transcripts and its subagents', Codex's rollouts, archived ones too, pi's session files with every branch, and what tools and subagents spent where the agent records it. Each call counts in the period it was made in, not its file's: a conversation resumed today holds calls from months ago.
- **Kept in roster's database**, call by call, small enough for millions of calls: the agents delete their conversations (Claude after 30 days unless told otherwise; you, removing them), and what was spent stays. A file unchanged isn't read again, and one that changed is read again in place of what was saved. When reading or prices change, every file still there is read again, so a mended reader or a new price reaches it; what's gone keeps what it was saved with.
- **A call counts once, wherever it's copied**: by its id, so a fork's copy of the conversation it came from (all three agents copy it into the new file, and Codex's subagents are forks of their parent) adds nothing. A file moved (Codex archiving a rollout) stays the same file.
- **Cost from a table of the API's prices** in roster, per model, priced as each agent prices a call (long calls, longer-kept cache writes, Claude's fast mode, US-only runs and web searches). What pi says a call cost counts as said. A subscription's figure is what it would have cost on the API. Models without a known price say so and stay out of the cost, rather than guessed. The config can price a model, one the table lacks or in the table's place; a change reprices every file still there, as a table's does.
- **Read in the background** while it shows, so the TUI shows what's saved at once and never waits; the first read of everything (thousands of files, hundreds of MB) must be quick and light on memory.

## 4. Storage
- **Config** in `$XDG_CONFIG_HOME/agent-roster/`, all optional: `config.toml`, the global env file, the user's `tmux.conf`. An unknown setting is an error, so a setting added later stops an older build instead of being ignored. Changes on disk are used at once.
- **Roster writes `config.toml` only for a change made in its settings**, and only that setting (taking one out takes its comment with it): the rest of the file stays as written, comments included; what was edited outside meanwhile is kept; and a symlinked file stays where it points. The one other write is the template, when you ask to edit a file that isn't there. Why: the settings should configure everything, and a config kept in git shows what changed.
- **State** in `$XDG_STATE_HOME/agent-roster/`: the database (SQLite, also keeping what the agents spent, §3.13), the files roster makes for itself (the generated tmux config, pi's extension, notes being edited, the TUI's lock), and the chats' directory. An older database is migrated when opened; a newer one is refused.
- **Uninstalling**: the binary, both directories, and roster's tmux server.
- **Several processes**: reports are stored as events, which every roster process folds into sessions; roster's own changes fold pending events first, so an older event can't undo them. A save writes only what it changed, so nothing reverts what another process saved meanwhile. Where you and a process may both write the same thing (a note, a generated title), the write happens only if it's still what it was; a note is merged instead (§3.8). A removed project's id is never given to another, so what was opened on it (its note, in an editor) can't be saved onto a project added since.

## 5. TUI
- **Layout**: a narrow sidebar with a summary of what waits ("2 need input · 1 working"), the list, and, once known, a line with what's left of the agents' limits, shortened to fit but never a figure cut short (it can be turned off); the preview on the right; the footer below. No boxes around them, only a line between them; popups, forms and the note editor are boxed.
- **Shelves, by attention** (the default): at the top what waits for you (Needs input: questions, permissions and finished turns you haven't read; Idle; Stopped), at the bottom what's handed off or finished (Working, then Archived, folded). Why: working is "go away, I gave it a task", stopped is idle without a running agent, archived is finished. Grouping by project is the alternative, a setting rather than a key: it's how you keep the list, not something flipped for a look (showing only one project is).
- **Within a shelf, first come, first served**: oldest first, so a card you're reading isn't pushed down by newcomers; in Needs input, questions and permissions come first. Reports that don't change a session's state don't move it. Each shelf's order can be changed, being a matter of taste.
- **The cursor follows its session**, except when you come back from giving it a task: then it keeps its row, landing on what took the session's place, usually the next one waiting. Why: answering an agent sent its card, and the cursor with it, to the bottom; going by whether the card changed shelf would also jump after only reading. Lists wrap around.
- **Cards**: the project, as a coloured badge and its name in that colour, since the badge alone didn't tell projects apart; the title and the state; the agent, name, model and effort. Stopped and archived sessions are one-line rows, each with since when.
- **A card's subagents at work** are counted on it, and the card unfolds, as a shelf does, to a line for each: its model, what it's doing, how long it's been at it. What it's doing is its latest step in a few words, read from its own files, in the manner of the agent's footer but shorter, the card being narrow (a file by its name, a fetch by its host, never a login or a token in a URL); till its first step, what it was given to do. Nothing a terminal would take for more than text reaches the card. Folded by default, as lines coming and going move the list; unfolded always, or the count only, as set.
- **The preview**: a header (project, name, title, agent, model, effort, state) and the statistics, then the live session's screen, read-only. Lines too wide are cut, not wrapped: wrapping breaks agents' box drawing. A stopped or archived session shows a panel of what it was and what starting it goes on with, including the end of its screen after an exit with an error, where an env file's error is. A heading shows its sessions at a glance. No key hints in a session's preview: they only repeated the footer and the menus. Where nothing else says what can be done (nothing to select, a project's details), the preview says it.
- **Filter**: `/` narrows the list fuzzily, fzf-style, keeping the shelves. The summary at the top still counts every session, so nothing waiting is missed.
- **Views**: the projects; the tmux server and database (§7); the settings, where every setting shows with what it does and changes apply at once (prices aside, edited in the file); usage (§3.12), each window a bar of what's left with a mark at how much of its time is left, so a bar ending short of its mark is used faster than the window lasts, and beside it what was spent (§3.13). In a view, `q` goes back, since quitting from a view surprised.
- **The footer** shows the keys that act where you are, with messages beside them rather than over them. Help shows the keys of the list and its menus.
- **Messages and questions say what happens now**: what a question's yes does (what stops, what goes, what stays), what was done. Not how to undo it or what to do next: the keys are in the footer and the menus. Only what refuses says what has to happen first, and only where nothing else would (an empty list's preview) does a message say what can be done.
- **Keys**: vim-style moving at the top level; the list's actions in three menus (session, project, view), where a letter means the same in each (`n` new, `e` edit, `o` note). What's used most gets the plain letter, what's rare its capital, and what can't be undone a capital of its own. The back key must be one an agent rarely needs: with Ctrl or Alt, or an F key, and not one a terminal sends for something else (Ctrl+M is Enter, Ctrl+I Tab).

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
- **Usage over time**: what an account used shows as it is now; keeping it to draw its history isn't worth it. What was spent is kept (§3.13): it's what the conversations held, and they go.
- **Prices from a service**: the table in roster follows the models the agents run, the config prices what it lacks, and a price unknown shows as unknown.
- **macOS and other systems**: roster runs on Linux.
- **Narrow races, accepted**: a refresh writes the state it observed, so in a narrow window another process's fold can lose to it; a first prompt claimed but not yet typed is lost if roster is killed in between; a resize's redraw may count as activity once for another roster process; a start reported after a quick edit of its model is learned under the edited alias; Codex reports its start with its first turn, so a model switched inside it before the first prompt is learned as what its start asked for.

---

## Appendix: what roster relies on

Checked against the real programs (tmux 3.5a, Claude Code 2.1.303 and later, pi 1.4.0, Codex 0.163.0 and 0.164.0). A change in one of these can break roster. What their files hold, field by field, is in the code that reads them.

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
- `SubagentStart` and `SubagentStop` fire as each subagent starts and ends its turn, in the background too, after the session's own `Stop`; its other hooks inside a subagent carry the subagent's id. Each subagent has a transcript of its own, beside which Claude marks it finished as it ends (2.1.311).
- `--session-id` fails once the conversation has messages, `--resume` gets it back, and `--fork-session` copies it, as it was, ids included, written with its first message.
- A one-word prompt on the command line runs a subcommand, `--` notwithstanding; a long prompt typed with Enter straight after isn't submitted, but a bracketed paste, a pause, then Enter is.
- Working, its screen changes at every sample; idle, it stays identical.
- Its terminal title names its task after a glyph (`✳`, or another such as `⠐`) from the first prompt on.
- A running Claude keeps `<claude dir>/sessions/<pid>.json`, removed on exit.
- On its first start in a directory it asks whether to trust the folder, before any hook, and keeps the answer only in its own `.claude.json`, which it rewrites itself under a lock of its own; a folder above it trusted counts too, up to the git repository's root (2.1.311).
- Its classic renderer draws on the normal screen, where a resize garbles scrolling back; the fullscreen one draws in the alternate screen.
- A transcript records each reply's tokens and what changes its price (fast mode, US-only runs, web searches); a subagent's calls are in its own transcript, not the parent's.
- Its login to Claude.ai is kept in its config directory, and its `/usage` asks Claude.ai with it; on an API key there's none, and no plan's limits.
- In its SDK mode it answers a request for what its `/usage` shows, which it calls experimental, calling no model and, told so, starting no MCP server and reading no settings; there, `ANTHROPIC_API_KEY` is used over the login, unasked (2.1.311).

**pi**
- `-e <file>` loads an extension for that run, alongside the user's own; `--session-id` opens a conversation or creates it; `--fork` copies one; `--list-models --offline` lists its models.
- `agent_end` can be followed by retries; `agent_settled` comes once the turn is really over.
- It leaves no sign that a conversation is open.
- Its session file records each reply's tokens and its cost as pi works it out, and model calls outside the conversation as entries of their own; a fork copies the entries as they were, ids included.

**Codex**
- It takes no id for a new conversation; it names it, and its hooks report it from its first turn on, `SessionStart` too, with the model. `resume <id>` gets a conversation back, and says so if it can't find it; `fork <id>` copies one; `debug models` lists its models.
- Esc during a turn fires `Interrupt`, not `Stop`; a turn that fails fires neither. Its subagents run on after Esc, each finishing its turn (0.164).
- At a start it may first ask to trust the folder (in a git repository; elsewhere it doesn't), or offer an update, which Enter installs. Trust counts only from its `config.toml`, not from `-c`: the folder's own entry, else its repository's (for a worktree, the main repository's); its own screen writes the repository's (0.164).
- It names a conversation from its first prompt, within seconds, and keeps the name until it's cleared; its terminal title shows it too, in a form the user can configure.
- `--no-daemon` runs its tools in its own environment.
- Hooks given with `-c` are a layer of their own and fire alongside the user's and the project's.
- `codex exec` always offers the model a couple of tools, whatever is turned off; turning off code mode and a read-only sandbox keep them from acting. The user's own `AGENTS.md` comes whatever is turned off; the project's can be kept out.
- An unknown feature is an error for `--disable`, a warning for `-c features.<name>=false`.
- The trust of hooks given with `-c` is kept per event, for whichever command was trusted last: trusting roster at another path untrusts it at the one before.
- New or changed hooks are reviewed at its start, and a prompt on its command line waits for the answer: trusted, its first turn reports; reviewed one by one or not trusted, it runs without them.
- Its rollout gives the model context as user messages too; prompts are told apart from it, by a mark since 0.157, before by an event of their own. A prompt given on its command line is a prompt like any other.
- `codex app-server` answers JSON-RPC on stdin and stdout until its stdin ends, and gives the account's limits; without a login it says so.
- Its rollout records the account's limits with every response, and, since 0.163, each model call's tokens; before, only running totals, written again without a call between. A resumed conversation goes on in the same rollout, from an older version too.
- A fork's rollout (a subagent's too) begins with a copy of the conversation it came from, every line dated as the copy was made; its turn ids are ordered by time, so the copy's are older than the fork's own (0.137). A rollout goes under the date it was made, so a resumed conversation's, or a subagent's, may be under another day than expected. Archiving moves a rollout, its name unchanged.
- `SubagentStart` and `SubagentStop` fire as each subagent starts and ends its turn, naming its rollout; its other hooks carry the subagent's id. A subagent's rollout says where its own lines start after the copy of its parent's, its task's name and its model; what it was told is encrypted (0.164).
- Its rollout records what each tool did once done, each edit with what it changed by file.
