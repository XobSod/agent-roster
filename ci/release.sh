#!/usr/bin/env bash
# Makes the draft release of the tag being built (GITHUB_REF_NAME, or the
# first argument), with what dist.sh packed and the tag's message as its
# notes. It checks first that the tag is annotated, is `v` and Cargo.toml's
# version, is the commit built, and is what the binary says it is: built on
# the tag and unchanged. Publishing the draft is left to a person.
set -euo pipefail
cd "$(dirname "$0")/.."

tag=${1:-${GITHUB_REF_NAME:?the tag to release}}
target=x86_64-unknown-linux-musl
fail() {
  echo "not releasing $tag: $*" >&2
  exit 1
}

# The checkout may have the tag as a bare name for its commit; the message is
# in the tag's own object, on GitHub.
if [ "${GITHUB_ACTIONS:-}" = true ]; then
  git fetch --quiet --force --no-tags origin "refs/tags/$tag:refs/tags/$tag"
fi
[ "$(git cat-file -t "refs/tags/$tag" 2>/dev/null)" = tag ] ||
  fail "it isn't an annotated tag, whose message would be the notes"
[ "$(git rev-parse "refs/tags/$tag^{commit}")" = "$(git rev-parse HEAD)" ] ||
  fail "it isn't the commit checked out"

version=$(cargo pkgid --quiet --package agent-roster | sed 's/.*[#@]//')
[ "$tag" = "v$version" ] || fail "Cargo.toml's version is $version"
built=$("target/$target/release/roster" --version)
[ "$built" = "roster $version" ] || fail "the binary says it's $built"

name=agent-roster-$tag-linux-x86_64
archive=target/dist/$name.tar.gz
for file in "$archive" "$archive.sha256"; do
  [ -f "$file" ] || fail "dist.sh hasn't made $file"
done

gh release create "$tag" --draft --verify-tag --notes-from-tag \
  --title "roster $version" "$archive" "$archive.sha256"
