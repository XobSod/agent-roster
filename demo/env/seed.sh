# Run by setup.sh, in the demo's environment: a fresh roster with three projects and
# the sessions of a scene (`seed.sh <scene>`), every agent a stand-in (fake-agent),
# and a few weeks of what they spent. Each session's prompt picks what its stand-in
# does; every name and figure is made up.
set -euo pipefail
scene=${1:-overview}

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
# The agents are the stand-in, on the PATH as claude, pi and codex (setup.sh). The
# warning below a quarter of the machine, so the processes scene's build is marked.
cat >"$XDG_CONFIG_HOME/agent-roster/config.toml" <<TOML
fit_preview = "always"
memory_warning = "1G"
TOML
for p in webapp api docs; do
  mkdir -p "$HOME/$p"
  git -C "$HOME/$p" init -q
done
roster project add webapp ~/webapp --model sonnet >/dev/null
roster project add api ~/api --model opus --effort high >/dev/null
roster project add docs ~/docs --agent pi >/dev/null

# What the agents spent over the last six weeks, in their own files: Claude's
# transcripts, Codex's rollouts, pi's sessions.
python3 - <<'PY'
import datetime, json, os, random, uuid
random.seed(7)
home, now = os.environ["HOME"], datetime.datetime.now(datetime.timezone.utc)
def iso(at):
    return at.strftime("%Y-%m-%dT%H:%M:%S.000Z")
def write(path, lines):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.writelines(json.dumps(l, separators=(",", ":")) + "\n" for l in lines)
for day in range(42):
    for _ in range(random.randint(1, 4)):
        at = now - datetime.timedelta(days=day, hours=random.uniform(1, 10))
        project = random.choice(["webapp", "api", "docs"])
        cwd = f"{home}/{project}"
        calls = random.randint(20, 120)
        agent = random.choices(["claude", "codex", "pi"], [6, 3, 2])[0]
        sid = str(uuid.UUID(int=random.getrandbits(128)))
        if agent == "claude":
            model = random.choices(["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"], [3, 4, 1])[0]
            lines = [{"type": "assistant", "timestamp": iso(at + datetime.timedelta(seconds=20 * i)),
                      "message": {"id": f"msg-{sid[:8]}-{i}", "model": model, "usage": {
                          "input_tokens": random.randint(2, 40),
                          "cache_read_input_tokens": 20_000 + 900 * i,
                          "cache_creation_input_tokens": random.randint(0, 3000),
                          "output_tokens": random.randint(80, 1500)}}} for i in range(calls)]
            write(f"{home}/.claude/projects/{cwd.replace('/', '-')}/{sid}.jsonl", lines)
        elif agent == "codex":
            model = random.choice(["gpt-6.1-sol", "gpt-6-luna"])
            lines = [{"timestamp": iso(at), "type": "session_meta", "payload": {"id": sid, "cwd": cwd}},
                     {"timestamp": iso(at), "type": "turn_context", "payload": {"model": model}}]
            for i in range(calls):
                context = 18_000 + 1_100 * i
                lines.append({"timestamp": iso(at + datetime.timedelta(seconds=25 * i)),
                              "type": "token_usage_record", "payload": {"usage": {
                                  "input_tokens": context, "cached_input_tokens": context - 1_500,
                                  "output_tokens": random.randint(60, 1200),
                                  "total_tokens": context + 600}}})
            write(f"{home}/.codex/sessions/{at.strftime('%Y/%m/%d')}/rollout-"
                  f"{at.strftime('%Y-%m-%dT%H-%M-%S')}-{sid}.jsonl", lines)
        else:
            lines = [{"type": "message", "id": f"a{i}", "timestamp": iso(at + datetime.timedelta(seconds=30 * i)),
                      "message": {"role": "assistant", "provider": "openai", "model": "gpt-6.1-sol",
                                  "usage": {"input": random.randint(500, 3000), "output": random.randint(80, 900),
                                            "cacheRead": 15_000 + 800 * i, "cacheWrite": 0,
                                            "cost": {"total": round(random.uniform(0.005, 0.04), 4)}}}}
                     for i in range(calls)]
            dashed = "--" + cwd.strip("/").replace("/", "-") + "--"
            write(f"{home}/.pi/agent/sessions/{dashed}/{iso(at).replace(':', '-').replace('.', '-')}_{sid}.jsonl", lines)
PY

# Waits until session $1's state is $2 (and, for idle, its turn has ended).
wait_for() {
  for _ in $(seq 150); do
    roster ls --json | jq -e --arg n "$1" --arg s "$2" \
      '.[] | select(.name == $n and .state == $s and ($s != "idle" or .turns > 0))' \
      >/dev/null && return
    sleep 0.2
  done
  echo "seed.sh: $1 never got $2" >&2
  roster ls >&2
  exit 1
}
# new <project> <name> <agent> <prompt>
new() { roster new "$1" --name "$2" --agent "$3" --prompt "$4" >/dev/null; }
# A session that's had its turn, read: idle, nothing waiting.
done_read() {
  new "$@"
  wait_for "$2" idle
  roster read "$1/$2"
}

case $scene in
overview)
  new webapp spike claude "Try a virtualised table for the orders list"
  wait_for spike idle
  roster archive --yes webapp/spike >/dev/null
  done_read docs changelog pi "Write the changelog for v2.3"
  roster stop docs/changelog >/dev/null
  done_read webapp e2e claude "Find out why the checkout e2e test is flaky"
  done_read docs guide pi "Proofread the getting-started guide"
  new webapp parser claude "Fix the failing date parser tests"
  wait_for parser idle
  new api migrate claude "Migrate user IDs to UUIDs"
  wait_for migrate working
  # Last: its question comes about 11 s after its prompt, while the tape is in parser.
  new api pagination codex "Add pagination to the orders endpoint"
  wait_for pagination working
  ;;
notes)
  done_read webapp parser claude "Fix the failing date parser tests"
  done_read webapp e2e claude "Find out why the checkout e2e test is flaky"
  done_read api retries codex "Add retries to the payment webhook"
  ;;
processes)
  # Archived, so the build's is the one tmux session (the server view lists them by
  # id, and keeps a stopped one's last screen).
  done_read webapp e2e claude "Find out why the checkout e2e test is flaky"
  roster archive --yes webapp/e2e >/dev/null
  done_read docs guide pi "Proofread the getting-started guide"
  roster archive --yes docs/guide >/dev/null
  new webapp build claude "Why is the production build so slow?"
  wait_for build working
  ;;
subagents)
  done_read webapp e2e claude "Find out why the checkout e2e test is flaky"
  new api migrate claude "Migrate user IDs to UUIDs"
  wait_for migrate working
  new api audit claude "Audit the admin routes for missing auth checks"
  wait_for audit working
  ;;
usage)
  done_read webapp parser claude "Fix the failing date parser tests"
  done_read api retries codex "Add retries to the payment webhook"
  done_read docs guide pi "Proofread the getting-started guide"
  ;;
finding)
  done_read webapp parser claude "Fix the failing date parser tests"
  done_read webapp e2e claude "Find out why the checkout e2e test is flaky"
  done_read webapp table claude "Try a virtualised table for the orders list"
  done_read api retries codex "Add retries to the payment webhook"
  done_read api changelog claude "Write the API changelog for v2.3"
  done_read docs guide pi "Proofread the getting-started guide"
  ;;
*)
  echo "seed.sh: no scene $scene" >&2
  exit 1
  ;;
esac
