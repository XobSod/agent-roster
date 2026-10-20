# agent-roster

A keyboard-first terminal manager for AI coding agent sessions (Claude Code for now).
Agents run in roster's own private tmux server; roster tracks whether each one is
working, needs input, is idle or stopped, and whether you've seen its latest turn.

Status: M1, the engine and CLI. The TUI (list + preview) arrives in M2.

## Install

Requires tmux ≥ 3.2 and Claude Code on `PATH`.

```sh
cargo install --path .     # installs `roster` into ~/.cargo/bin
```

## Use

```sh
roster project add myproj ~/src/myproj
roster new myproj --name api --attach   # start Claude and go into it; Ctrl+Q comes back
roster ls                               # ● marks sessions with a turn you haven't seen
roster show api                         # print its current screen
roster attach api                       # going in and out with Ctrl+Q marks it read
roster stop api && roster start api     # start resumes the conversation
roster rm api && roster restore api     # deleted sessions keep their conversation
roster events --follow                  # what the agents' hooks report
```

Sessions are addressed as `project/name`, `name`, or an id prefix. `roster --help`
lists every command.

## Configure

All files are optional and live in `~/.config/agent-roster/`:

- `config.toml`: `shell`, `[keys] detach`, `[agents.claude] command`/`args`.
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
