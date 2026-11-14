#!/bin/sh
# Test plugin that logs each start ("start") and each request method it receives
# in events.log next to the script, and answers every request with "pong".
# "slow" takes one second to answer; "initialize" is refused with an error
# while a file named fail_initialize exists next to the script. Like a Node
# plugin, it rejects a numeric id beyond Number.MAX_SAFE_INTEGER (more than 16
# digits) with an error whose id is null.
dir=$(dirname "$0")
echo start >> "$dir/events.log"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  echo "$method" >> "$dir/events.log"
  [ -z "$id" ] && continue
  if [ ${#id} -gt 16 ]; then
    printf '{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid Request"},"id":null}\n'
    continue
  fi
  [ "$method" = slow ] && sleep 1
  if [ "$method" = initialize ] && [ -f "$dir/fail_initialize" ]; then
    printf '{"jsonrpc":"2.0","error":{"code":-32000,"message":"refused"},"id":%s}\n' "$id"
    continue
  fi
  printf '{"jsonrpc":"2.0","result":"pong","id":%s}\n' "$id"
done
