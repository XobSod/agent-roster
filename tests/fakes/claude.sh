#!/bin/sh
if [ -e .fullscreen ]; then
  printf '\033[?1049h'
  draw() { printf '\033[2J\033[Hfull screen %s\n' "$(stty size)"; }
  trap draw WINCH
  draw
  while :; do sleep 0.1; done
fi
echo "fake agent: $*"
echo "TOKEN=${TOKEN-unset} GLOBAL=${GLOBAL-unset} SESSION=${ROSTER_SESSION-unset}"
exec cat
