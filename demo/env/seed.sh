# Run by setup.sh, in the demo's environment: a fresh roster with three projects and a
# session on every shelf. Each session's prompt picks what fake-claude does.
set -euo pipefail
env_dir=$(cd "$(dirname "$0")" && pwd)

# What follows deletes $HOME and kills a tmux server: only the demo's own, as
# setup.sh makes them, never ones inherited from wherever it was run.
if [ "$HOME" != /tmp/roster-demo ] || [ "${ROSTER_TMUX_SOCKET:-}" != roster-demo ] ||
  [ "${XDG_CONFIG_HOME:-}" != /tmp/roster-demo/.config ] ||
  [ "${XDG_STATE_HOME:-}" != /tmp/roster-demo/.local/state ]; then
  echo "seed.sh: run it through setup.sh (it resets HOME=/tmp/roster-demo); $HOME stays" >&2
  exit 1
fi
tmux -L "$ROSTER_TMUX_SOCKET" kill-server 2>/dev/null || true
rm -rf "$HOME"
mkdir -p "$XDG_CONFIG_HOME/agent-roster"
cat >"$XDG_CONFIG_HOME/agent-roster/config.toml" <<TOML
[agents.claude]
command = "$env_dir/fake-claude"
TOML
for p in webapp api docs; do
  mkdir -p "$HOME/$p"
  git -C "$HOME/$p" init -q
done
roster project add webapp ~/webapp --model sonnet >/dev/null
roster project add api ~/api --model opus --effort high >/dev/null
roster project add docs ~/docs --model haiku >/dev/null

# Waits until session $1's state is $2 (and, for idle, its turn has ended).
wait_for() {
  for _ in $(seq 150); do
    roster ls --json | jq -e --arg n "$1" --arg s "$2" \
      '.[] | select(.name == $n and .state == $s)' >/dev/null && return
    sleep 0.2
  done
  echo "seed.sh: $1 never got $2" >&2
  roster ls >&2
  exit 1
}
new() { roster new "$1" --name "$2" --prompt "$3" >/dev/null; }

new webapp spike "Try a virtualised table for the orders list"
wait_for spike idle
roster archive --yes webapp/spike >/dev/null
new docs changelog "Write the changelog for v2.3"
wait_for changelog idle
roster read docs/changelog
roster stop docs/changelog >/dev/null
new webapp e2e "Find out why the checkout e2e test is flaky"
wait_for e2e idle
roster read webapp/e2e
roster chat --prompt "How do I use tmux copy mode?" >/dev/null
wait_for claude-1 idle
roster read chats/claude-1
new webapp parser "Fix the failing date parser tests"
wait_for parser idle
new api migrate "Migrate user IDs to UUIDs"
wait_for migrate working
# Last: its question comes about 11 s after its prompt, while the tape is in parser.
new api limits "Add rate limiting to the login endpoint"
wait_for limits working
