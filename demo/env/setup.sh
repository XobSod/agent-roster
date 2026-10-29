# Sourced by the tapes (hidden): `source demo/env/setup.sh <scene>` gives the demo a
# home of its own (HOME=/tmp/roster-demo) with the scene's projects and sessions, all
# run by the stand-in agents (fake-agent, seed.sh), and sets up the shell.

env_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# Nothing of a roster or tmux it was run from (inside a session, say), whose
# database the demo's commands would reach, nor of the agents' own files or a
# shell's startup files (which could put a real agent first on the PATH).
unset ROSTER_SESSION ROSTER_DB ROSTER_EXE ROSTER_LIST_VIEW TMUX TMUX_PANE
unset PI_CODING_AGENT_SESSION_DIR ZDOTDIR BASH_ENV ENV
export SHELL=/bin/bash
export HOME=/tmp/roster-demo
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state
export XDG_DATA_HOME=$HOME/.local/share XDG_CACHE_HOME=$HOME/.cache
export CLAUDE_CONFIG_DIR=$HOME/.claude PI_CODING_AGENT_DIR=$HOME/.pi/agent
export CODEX_HOME=$HOME/.codex ROSTER_TMUX_SOCKET=roster-demo
# The stand-in as claude, pi and codex, before any real one.
PATH=$env_dir/bin:$(cd "$env_dir/../.." && pwd)/target/release:$PATH

# (a process of its own: a sourced file's set -e is off in `source … && clear`)
bash "$env_dir/seed.sh" "${1:-overview}" || {
  echo "demo/env/setup.sh: seeding the demo failed" >&2
  return 1
}
cd ~ || return
PS1='\[\e[1;34m\]\W\[\e[0m\] \$ '
