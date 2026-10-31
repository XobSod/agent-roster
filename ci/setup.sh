#!/usr/bin/env bash
# Readies a fresh Ubuntu machine, GitHub's runner or a container like it, for
# the other scripts: tmux and git for the tests, musl for the static build,
# and Rust's stable toolchain with clippy, rustfmt and the musl target.
set -euo pipefail

sudo=
[ "$(id -u)" = 0 ] || sudo=sudo
$sudo apt-get update -qq
$sudo apt-get install -y -qq --no-install-recommends \
  build-essential ca-certificates curl git musl-tools tmux >/dev/null

if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
  export PATH="$HOME/.cargo/bin:$PATH"
fi
rustup toolchain install stable --profile minimal \
  --component clippy,rustfmt --target x86_64-unknown-linux-musl
rustup default stable
tmux -V
rustc --version
