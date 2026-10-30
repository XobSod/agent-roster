# Codex: what to check on a machine with Codex

What changed for Codex since the checks with Codex 0.165.0 (2026-10-29) was
written from Codex's source alone, without a real Codex. This branch holds
only this file on top of master. Tick each item once you've seen it with a
real Codex, note what differs, and send me the output asked for.

Setup, from the repo on this branch (master's code): `cargo build`, then

```sh
r=./target/debug/roster
mkdir -p /tmp/cx && git -C /tmp/cx init -q && echo hello > /tmp/cx/a.txt
$r project add --agent codex cx /tmp/cx
codex --version
```

If your config.toml sets `trust_project_hooks`, rename it `trust_roster_hooks`:
roster refuses the old name.

## Roster's hooks trusted for each run, your config untouched

Roster no longer writes `hooks.state` into `~/.codex/config.toml`. It gives
the hashes to each run as `-c hooks.state={…}`, which Codex's source reads
over the config's.

- [ ] First `cp ~/.codex/config.toml /tmp/codex-config-before.toml`.
- [ ] `n=trust; $r new cx -n $n -a`: no review of roster's hooks should come
  first, and a prompt should show working, then idle and unread.
- [ ] `diff /tmp/codex-config-before.toml ~/.codex/config.toml` prints nothing.
- [ ] With that session still running, start one from a release build
  (`cargo build --release; ./target/release/roster new cx -n trust2`): no
  review either.
- [ ] Back in `trust`, do something that makes Codex write its config
  (`/approvals`, say), then give it a prompt: its card still goes working,
  then idle.
- [ ] If a review shows or a hook stops reporting, send me
  `$r events -s cx/$n | tail -20` and what Codex's screen said.

## Esc, reported through Codex's Interrupt hook

Roster now listens to `Interrupt`, and reads what an interrupted turn left
running, as at a turn's end.

- [ ] (a) Esc while it works: idle at once (not after a few still seconds),
  not unread.
- [ ] (b) In Plan mode (Shift+Tab), ask something that makes it put a question
  to you, then press Esc on its form: idle, no longer on Needs input.
- [ ] (c) Ask it to run a command that needs your approval, then press Esc at
  that question: idle.
- [ ] (d) Have it start `sleep 60; echo done` in the background, then press Esc
  mid-turn: the card stays working with `$1`, not unread; after the minute
  it's idle and unread.
- [ ] If any of these stays working or needing input, send me
  `$r events -s cx/$n | tail -20`.

## Background terminals ending, however Codex records it

A terminal counts as over once its end is recorded anywhere in its turn,
whichever output said it ran: its own call's, or a poll's, since Codex writes
a call's output only once the whole response is over.

- [ ] Ask: "Start `sleep 20; echo done` in the background, then keep checking
  on it until it's finished, and tell me what it printed". When the turn ends,
  the card has no `$1` left, and the session is idle and unread.
- [ ] Ask for two commands in one go, a slow one and a quick one, both in the
  background ("Run `sleep 40` and `sleep 3` in the background, both at once,
  then end your turn"). `$2`, then `$1` once the quick one ends, then idle
  and unread. A `$` that never goes away is the bug: send me the rollout's
  lines (`grep -n 'session ID\|item_completed' <its rollout>`).

## `/review` still the session's

The check for a review's own thread changed: a thread's rollout is now told
by the thread id in its name.

- [ ] Run a `/review` in a session that has had a prompt. Its "Last prompts"
  and `$r info cx/$n` stay the session's (no review instructions as a prompt);
  when it ends, idle and unread.

## An edited prompt

- [ ] After two prompts in a session, edit the first one (Esc Esc, pick it,
  change it, Enter). By Codex's source, Codex then writes the conversation
  over into a rollout of its own, `rollout-…-<thread>_<rollout>.jsonl`, beside
  the old one. Send me:
  - `ls ~/.codex/sessions/$(date +%Y/%m/%d) | tail -4`;
  - `$r ls --json | jq -r '.[] | select(.name=="'$n'") | .transcript_path'`;
  - `wc -l` of both files.
- [ ] Roster should follow the new file: its preview's "Last prompts" shows
  the edited prompt, and its card's state is right afterwards.
- [ ] Also check `$r info cx/$n` (is the context counted twice?) and the adopt
  list (`$r adopt --list`, or the TUI's): which of the two files does it show
  for that conversation?

## A conversation an older Codex began

By Codex's source, Codex records a command's end only in rollouts with
paginated history. A conversation begun before keeps the old kind, resumed
or forked, so roster counts no terminals there.

- [ ] If you have one (`head -1 <its rollout> | jq .payload.history_mode` says
  `"legacy"` or nothing), adopt it and leave a command running, as above: no
  `$1` on its card, and the session idle and unread when the turn ends.

## A failed turn (if one comes your way)

- [ ] A turn that ends in an error (a network drop mid-turn, say) ends idle,
  not unread, as an interrupted one does.
- [ ] `grep task_complete <its rollout> | tail -1` should show an `error` in it.
