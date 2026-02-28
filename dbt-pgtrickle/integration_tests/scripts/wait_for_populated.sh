#!/usr/bin/env bash
# Wait for a stream table to be populated (is_populated = true).
# Usage: ./wait_for_populated.sh <stream_table_name> [timeout_seconds]
set -euo pipefail

NAME="${1:?Usage: wait_for_populated.sh <name> [timeout]}"
TIMEOUT="${2:-30}"
ELAPSED=0

while [ "$ELAPSED" -lt "$TIMEOUT" ]; do
  POPULATED=$(psql -tAc \
    "SELECT is_populated FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '$NAME'")
  if [ "$POPULATED" = "t" ]; then
    echo "Stream table '$NAME' is populated after ${ELAPSED}s"
    exit 0
  fi
  sleep 1
  ELAPSED=$((ELAPSED + 1))
done

echo "ERROR: Stream table '$NAME' not populated after ${TIMEOUT}s" >&2
exit 1
