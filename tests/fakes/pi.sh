#!/bin/sh
id=; extension=; prev=
for a in "$@"; do
  case "$prev" in --session-id) id=$a ;; -e) extension=$a ;; esac
  prev=$a
done
echo "fake pi: $*"
[ -f "$extension" ] && echo "with the extension"
file="$PWD/.pi-$id.jsonl"
report() { printf '%s' "$2" | "$ROSTER_EXE" report "$1"; }
report session "{\"conversation_id\":\"$id\",\"transcript_path\":\"$file\",\"model\":\"fake-model-1\",\"effort\":\"high\",\"title\":null,\"context_window\":200000,\"started\":true}"
while read -r line; do
  report prompt '{}'
  echo "got: $line"
  printf '{"type":"message","message":{"role":"user","content":[{"type":"text","text":"%s"}]}}\n' "$line" >> "$file"
  printf '{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":5,"cacheRead":100,"cost":{"total":0.5}}}}\n' >> "$file"
  case "$line" in "name "*) report info "{\"title\":\"${line#name }\"}" ;; esac
  report idle '{}'
done
