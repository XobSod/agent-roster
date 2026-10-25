#!/bin/sh
echo "generic agent ready"
while read -r line; do
  sleep 0.5
  echo "reply: $line"
done
