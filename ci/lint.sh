#!/usr/bin/env bash
# What each push has to pass besides the tests: formatting, and clippy with
# warnings as errors, for the release's target too where it's installed (as
# on CI): some warnings are musl's alone. Not for a release: a lint new in
# that day's stable would stop a release of a commit that passed.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
musl=x86_64-unknown-linux-musl
if rustup target list --installed 2>/dev/null | grep -x "$musl" >/dev/null; then
  cargo clippy --all-targets --locked --target "$musl" -- -D warnings
fi
