#!/bin/sh
# Claude's SDK mode, as roster asks it its limits: each control request
# answered, get_usage with usage.json in Claude's directory if there's one
# (no plan's limits if not), what it was given kept beside it, and that its
# input ended (it exits then, as Claude does, tidying up).
case " $* " in *" --input-format stream-json "*)
  dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
  printf '%s\n' "$@" > "$dir/sdk-args" 2>/dev/null
  echo "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY-unset}" > "$dir/sdk-env" 2>/dev/null
  rm -f "$dir/sdk-ended" "$dir/sdk-input"
  while read -r line; do
    printf '%s\n' "$line" >> "$dir/sdk-input"
    case "$line" in
      *'"subtype":"initialize"'*) id=r1 body='{}' ;;
      *'"subtype":"get_usage"'*)
        id=r2
        body=$(cat "$dir/usage.json" 2>/dev/null || echo '{"rate_limits_available":false,"rate_limits":null}') ;;
      *) continue ;;
    esac
    printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":%s}}\n' "$id" "$body"
  done
  : > "$dir/sdk-ended"
  exit 0 ;;
esac
if [ -e .fullscreen ]; then
  printf '\033[?1049h'
  draw() {
    size=$(stty size)
    printf '\033[2J\033[Hfull screen %s\n' "$size"
    echo "$size" >> .sizes
  }
  trap draw WINCH
  draw
  # A trapped signal ends a wait at once, so each size is read as soon as
  # it's told, one told for a moment too.
  while :; do sleep 1 & wait $!; done
fi
echo "fake agent: $*"
echo "TOKEN=${TOKEN-unset} GLOBAL=${GLOBAL-unset} SESSION=${ROSTER_SESSION-unset}"
exec cat
