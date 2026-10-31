#!/usr/bin/env bash
# The tests, roster's (the end-to-end ones need tmux) and its vendored edtui's.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo test --locked
cargo test --locked -p edtui
