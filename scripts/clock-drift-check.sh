#!/usr/bin/env bash
# B14 check: headless loopback with the receiver's latency clock skewed, so
# drift exists on one PC. Prints the newest-frame capture_to_arrival latency
# at the start and end of the run. Headless only: nothing is rendered and
# nothing plays, so it is safe on the PC being captured.
#
#   scripts/clock-drift-check.sh <secs> <skew_ppm> [once]
#
# "once" sets RELAY_CLOCK_SYNC_ONCE on the sender: the pre-S33 behaviour.
set -u
SECS=${1:-90}; PPM=${2:-500}; MODE=${3:-resync}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/relay-share.exe"
OUT="$ROOT/target/clock-drift-$MODE"
mkdir -p "$OUT"
CODE=424242; NAME="drift-$$"
export RELAY_INSTANCE="drift$$"

RELAY_CLOCK_SKEW_PPM=$PPM "$BIN" recv --headless --code $CODE --name "$NAME" >"$OUT/recv.ndjson" 2>"$OUT/recv.log" &
RECV=$!
sleep 3
if [ "$MODE" = once ]; then export RELAY_CLOCK_SYNC_ONCE=1; fi
(sleep "$SECS"; echo stop) | "$BIN" send --peer "$NAME" --code $CODE --fps 30 --bitrate 8 --no-audio >"$OUT/send.ndjson" 2>"$OUT/send.log"
sleep 1
kill $RECV 2>/dev/null
wait $RECV 2>/dev/null
python "$ROOT/scripts/clock-drift-summary.py" "$OUT/recv.ndjson"
echo "mode=$MODE skew_ppm=$PPM secs=$SECS"
