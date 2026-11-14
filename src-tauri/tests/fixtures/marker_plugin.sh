#!/bin/sh
# Test plugin that records each start in a file next to the script and answers
# every request with "pong", so a test can tell whether and how often the host
# started it.
dir=$(dirname "$0")
echo started >> "$dir/started.marker"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -n "$id" ] && printf '{"jsonrpc":"2.0","result":"pong","id":%s}\n' "$id"
done
