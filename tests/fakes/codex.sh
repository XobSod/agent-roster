#!/bin/sh
# Its server, as asked for the account's limits: it answers each request,
# and serves until its stdin ends.
if [ "$1" = app-server ]; then
  in=$(( $(date +%s) + 3600 ))
  while read -r line; do
    case "$line" in
      *'"method":"initialize"'*) echo '{"id":1,"result":{"userAgent":"fake"}}' ;;
      *'"method":"account/rateLimits/read"'*)
        echo '{"method":"remoteControl/status/changed","params":{"status":"disabled"}}'
        printf '{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":%s},"secondary":{"usedPercent":13,"windowDurationMins":10080,"resetsAt":%s},"planType":"plus"}}}\n' "$in" "$in" ;;
    esac
  done
  exit 0
fi
id=; sub=; prev=; model=; effort=; prompt=; rest=no; n=0
for a in "$@"; do
  if [ "$rest" = yes ]; then prompt=$a; continue; fi
  case "$a" in
    --) rest=yes ;;
    model=*) model=$(printf '%s' "${a#model=}" | tr -d '"') ;;
    model_reasoning_effort=*) effort=$(printf '%s' "${a#model_reasoning_effort=}" | tr -d '"') ;;
    resume|fork) sub=$a ;;
  esac
  case "$prev" in resume|fork) [ -z "$id" ] && id=$a ;; esac
  prev=$a
done
new_id() { n=$((n + 1)); printf '%08x-0000-4000-8000-%012x' "$$" "$n"; }
source=startup
case "$sub" in
  resume) source=resume ;;
  fork) source=fork; id=$(new_id) ;;
  *) id=$(new_id) ;;
esac
started=no
echo "fake codex: $*"
report() { printf '%s' "$2" | "$ROSTER_EXE" report "$1"; }
turn() {
  file="$PWD/.codex-$id.jsonl"
  pay="{\"session_id\":\"$id\",\"transcript_path\":\"$file\",\"cwd\":\"$PWD\",\"model\":\"${model:-fake-model-1}\",\"source\":\"$source\"}"
  if [ "$started" = no ]; then report session "$pay"; started=yes; fi
  echo "turn: $1"
  # Codex names a conversation from its first prompt, in its index.
  mkdir -p "$CODEX_HOME"
  if ! grep -qs "\"$id\"" "$CODEX_HOME/session_index.jsonl"; then
    printf '{"id":"%s","thread_name":"named: %s","updated_at":"t"}\n' "$id" "$1" >> "$CODEX_HOME/session_index.jsonl"
  fi
  report prompt "$pay"
  printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$PWD" >> "$file"
  printf '{"type":"turn_context","payload":{"model":"%s","effort":"%s"}}\n' "${model:-fake-model-1}" "${effort:-medium}" >> "$file"
  printf '{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"%s"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}\n' "$1" >> "$file"
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}\n' >> "$file"
  printf '{"type":"token_usage_record","payload":{"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":115}}}\n' >> "$file"
  printf '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"output_tokens":5,"total_tokens":115},"last_token_usage":{"total_tokens":115},"model_context_window":400000}}}\n' >> "$file"
  report working "$pay"
  report idle "$pay"
}
# As at a screen it shows first, declined: gone before its first turn.
if [ -e .exit-early ]; then echo "declined"; exit 1; fi
[ -n "$prompt" ] && turn "$prompt"
while read -r line; do
  case "$line" in
    clear) id=$(new_id); source=clear; started=no; echo "cleared"; continue ;;
  esac
  echo "got: $line"
  turn "$line"
done
