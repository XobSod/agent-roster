#!/usr/bin/env bash
# Runs CI's steps (setup, lint, test, dist) as GitHub's runner does: in an
# Ubuntu 24.04 container, as a user that isn't root but has sudo, with no
# terminal and $CPUS processors (4 unless set, as the runner has), on a fresh
# clone of the commit checked out: what isn't committed isn't in it. The
# release's archive comes out in target/docker-dist/. Needs Docker.
set -euo pipefail
cd "$(dirname "$0")/.."

out=target/docker-dist
rm -rf "$out"
mkdir -p "$out"
# A clone here first: a worktree's .git points into its repository, outside
# what the container would see.
src=target/docker-src
rm -rf "$src"
trap 'rm -rf "$src"' EXIT
git clone -q . "$src"
docker run --rm -i --cpus "${CPUS:-4}" -v "$PWD/$src:/src:ro" -v "$PWD/$out:/out" \
  ubuntu:24.04 bash -euo pipefail <<'CONTAINER'
apt-get update -qq
apt-get install -y -qq --no-install-recommends sudo git ca-certificates >/dev/null
useradd -m runner
echo 'runner ALL=(ALL) NOPASSWD:ALL' >/etc/sudoers.d/runner
sudo -iu runner bash -euo pipefail <<'RUNNER'
git config --global --add safe.directory '*'
git clone -q /src work
cd work
ci/setup.sh
export PATH="$HOME/.cargo/bin:$PATH"
ci/lint.sh
ci/test.sh
ci/dist.sh
RUNNER
cp /home/runner/work/target/dist/* /out/
chown -R "$(stat -c %u:%g /out)" /out
CONTAINER
ls "$out"
