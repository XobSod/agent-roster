# Sourced by the tapes (hidden): `source demo/env/setup.sh` gives the demo a home of
# its own (HOME=/tmp/roster-demo) with projects and sessions in every state, all run by
# the stand-in agent (fake-claude, seed.sh), and sets up the shell.

env_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export HOME=/tmp/roster-demo
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state
export XDG_DATA_HOME=$HOME/.local/share XDG_CACHE_HOME=$HOME/.cache
export CLAUDE_CONFIG_DIR=$HOME/.claude ROSTER_TMUX_SOCKET=roster-demo
PATH=$(cd "$env_dir/../.." && pwd)/target/release:$PATH

# (a process of its own: a sourced file's set -e is off in `source … && clear`)
bash "$env_dir/seed.sh" || {
  echo "demo/env/setup.sh: seeding the demo failed" >&2
  return 1
}
cd ~ || return
PS1='\[\e[1;34m\]\W\[\e[0m\] \$ '
