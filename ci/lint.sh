#!/usr/bin/env bash
# What each push has to pass besides the tests: formatting, and clippy with
# warnings as errors. Not for a release: a lint new in that day's stable
# would stop a release of a commit that passed.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
