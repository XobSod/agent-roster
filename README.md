# agent-roster

A keyboard-first terminal manager for AI coding agent sessions (Claude Code for now).
Agents run in roster's own private tmux server; roster tracks whether each one is
working, needs input, is idle or stopped, and whether you've seen its latest turn.

Status: MVP complete (M0–M3). Claude Code is tracked through its hooks; any other CLI agent can be added as a generic agent, tracked by its screen.

## Install

Requires tmux ≥ 3.2 and Claude Code on `PATH`.

```sh
cargo install --path .     # installs `roster` into ~/.cargo/bin
```

To scroll back through an agent's history while attached, also set up the two
example files described under [Configure](#configure): without them, copy mode in
your own tmux shows that pane's older output, not the agent's.

## Use

Run `roster` for the TUI: sessions on the left, the selected agent's screen on the right.

Navigate with `h j k l` (or arrows); `Enter` attaches (or folds a heading); `q` quits
and `?` shows every key. Everything else is in three menus, which pop up their keys:

| Menu | Keys |
|---|---|
| `s` session | `n` new · `e` edit · `s` start · `x` stop · `r` restart · `u` read/unread · `d` delete |
| `p` project | `n` new · `e` edit · `f` show only this project · `d` remove |
| `v` view | `p` group by project · `d` deleted sessions (`Enter` restores) · `t` tmux server and database |

Sessions sit on shelves by attention: *Needs you* (questions, finished turns you
haven't seen), *Idle*, *Working* (dimmed) and *Stopped*. In an agent, Ctrl+Q comes
back and marks it read.

New and edit open a form (`Tab`/`↑↓` move, `←→` change, `Enter` submits). Projects
carry a default agent, model and effort for their sessions; a new session can also
get a first prompt, typed into the agent once it's ready.

Everything is also available from the CLI:

```sh
roster project add myproj ~/src/myproj --model opus --effort high
roster new myproj --name api --attach   # start Claude and go into it; Ctrl+Q comes back
roster new myproj --prompt "fix the failing tests"   # typed in once Claude is ready
roster edit api --model sonnet          # applies on the next start
roster project edit myproj --effort default
roster ls                               # ● marks sessions with a turn you haven't seen
roster ls --project myproj --sort attention   # like the TUI's shelves
roster show api                         # print its current screen
roster attach api                       # going in and out with Ctrl+Q marks it read
roster stop api && roster start api     # start resumes the conversation
roster rm api && roster restore api     # deleted sessions keep their conversation
roster events --follow                  # what the agents' hooks report
roster doctor                           # roster's tmux server and database, checked; --fix removes orphans
```

Sessions are addressed as `project/name`, `name`, or an id prefix. `roster --help`
lists every command.

## Configure

All files are optional and live in `~/.config/agent-roster/`:

- `config.toml`: `shell`, `[keys]` (every TUI key, plus `detach` for Ctrl+Q),
  `[agents.claude] command`/`args`. Any other agent
  goes in its own table and is then offered for new sessions:

  ```toml
  [agents.aider]          # runs `aider`; or set command = "..."
  args = ["--no-auto-commits"]
  idle_after = 4          # seconds of a still screen that count as idle
  ```

  Such agents are tracked by their screen and can't resume, so roster asks before
  starting one again.
- `env`: sourced before every agent start, so `TOKEN="$(token-cmd)"` gets a fresh
  value each time. A `.agent-roster.env` in a project's directory is sourced after it.
- `tmux.conf`: additions to roster's tmux. By default every key except Ctrl+Q goes to
  the agent. For scrolling back with `C-a [`, start from
  [examples/tmux.conf](examples/tmux.conf); if you run roster inside your own tmux,
  also see [examples/outer-tmux.conf](examples/outer-tmux.conf).

State lives in `~/.local/state/agent-roster/`. Uninstall: remove the binary and both
directories, and `tmux -L agent-roster kill-server`.

## Develop

```sh
cargo test     # unit tests, plus end-to-end tests that need tmux
```

`ROSTER_TMUX_SOCKET=<name>` (and `XDG_CONFIG_HOME`/`XDG_STATE_HOME`) keep a
development build apart from your real sessions.
