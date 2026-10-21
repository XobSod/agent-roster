#!/usr/bin/env bash
# Records the GIFs of demo/gifs/ from the tapes of demo/tapes/ with VHS in Docker:
# `demo/render.sh` all of them, `demo/render.sh hero` some. Roster is built here first
# (release); the agents are demo/env/fake-claude, a scripted stand-in for Claude Code.
set -euo pipefail
cd "$(dirname "$0")/.."
image=roster-vhs
cargo build --release --quiet
docker build -q -t "$image" - <demo/env/Dockerfile >/dev/null
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for tape in demo/tapes/*.tape; do
    name=$(basename "$tape" .tape)
    [ "$name" = settings ] || names+=("$name")
  done
fi
for name in "${names[@]}"; do
  tape=demo/tapes/$name.tape
  gif=$(sed -n 's/^Output \(.*\.gif\)$/\1/p' "$tape")
  if [ -z "$gif" ] || [ "$(wc -l <<<"$gif")" -ne 1 ]; then
    echo "$tape: needs exactly one 'Output <file>.gif'" >&2
    exit 1
  fi
  echo "== $name"
  # then smaller: gifsicle -O3 is lossless, --lossy=30 leaves faint traces in large empty areas
  docker run --rm -v "$PWD:/work" -w /work --user "$(id -u):$(id -g)" -e HOME=/tmp \
    --entrypoint sh "$image" -c 'vhs "$1" && gifsicle -O3 --lossy=30 --batch "$2"' sh "$tape" "$gif"
done
