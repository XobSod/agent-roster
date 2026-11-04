#!/usr/bin/env bash
# Builds roster for release and packs it in target/dist/: a static x86_64
# Linux binary (musl, so it runs whatever the distribution's glibc), as
# agent-roster-v<version>-linux-x86_64.tar.gz with the licences and the
# README, and that archive's SHA-256 beside it, for `sha256sum -c`.
set -euo pipefail
cd "$(dirname "$0")/.."

target=x86_64-unknown-linux-musl
# Marked a release's, which alone updates itself: any other build is for what
# made it to update.
ROSTER_RELEASE_BUILD=1 cargo build --release --locked --target "$target"
bin=target/$target/release/roster
if readelf --program-headers "$bin" | grep -q INTERP; then
  echo "$bin isn't static: it asks for a program interpreter" >&2
  exit 1
fi
version=$("$bin" --version | sed -n 's/^roster //p')
if [ -z "$version" ]; then
  echo "$bin didn't say its version" >&2
  exit 1
fi

name=agent-roster-v$version-linux-x86_64
dist=target/dist
rm -rf "$dist"
mkdir -p "$dist/$name"
cp "$bin" LICENSE README.md "$dist/$name/"
cp vendor/edtui/LICENSE.md "$dist/$name/LICENSE-edtui.md"
tar -C "$dist" -czf "$dist/$name.tar.gz" \
  --sort=name --owner=0 --group=0 --numeric-owner "$name"
rm -r "${dist:?}/$name"
(cd "$dist" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
echo "$dist/$name.tar.gz"
