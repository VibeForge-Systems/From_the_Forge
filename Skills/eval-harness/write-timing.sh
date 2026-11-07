#!/usr/bin/env bash
# usage: write-timing.sh AGENT_ID TOTAL_TOKENS DURATION_MS
S="$(dirname "$0")"; d="$(python3 -I -c "import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]])" "$S/agent-map.json" "$1")"
printf '{\n  "total_tokens": %s,\n  "duration_ms": %s,\n  "total_duration_seconds": %s\n}\n' "$2" "$3" "$(python3 -I -c "print(round($3/1000,1))")" > "$S/$d/timing.json"
echo "$d/timing.json"
