#!/usr/bin/env bash
# B8 check: how long each end takes to exit once a share is stopped.
# Headless loopback (safe on the PC being captured). Prints, per run, the
# sender's stop->exit time, its exit code, and the receiver's time from the
# sender's stop to its own exit.
#
#   scripts/teardown-check.sh [runs] [share_secs] [extra send flags...]
set -u
RUNS=${1:-5}; SECS=${2:-6}; shift 2 2>/dev/null
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${RELAY_SHARE_BIN:-$ROOT/target/debug/relay-share.exe}"
OUT="$ROOT/target/teardown"
mkdir -p "$OUT"
export RELAY_INSTANCE="teardown$$"
now_ms() { date +%s%3N; }

for i in $(seq 1 "$RUNS"); do
  NAME="td-$$-$i"
  "$BIN" recv --headless --code 515151 --name "$NAME" >"$OUT/recv-$i.ndjson" 2>"$OUT/recv-$i.log" &
  RECV=$!
  sleep 3
  STOPFILE="$OUT/stop-$i"; rm -f "$STOPFILE"
  (sleep "$SECS"; now_ms >"$STOPFILE"; echo stop) | "$BIN" send --peer "$NAME" --code 515151 --fps 30 --bitrate 8 "$@" >"$OUT/send-$i.ndjson" 2>"$OUT/send-$i.log"
  SEND_RC=$?
  SEND_EXIT=$(now_ms)
  # The receiver should notice on its own; give it 10 s before calling it hung.
  for _ in $(seq 1 100); do kill -0 $RECV 2>/dev/null || break; sleep 0.1; done
  RECV_EXIT=$(now_ms)
  HUNG=no
  if kill -0 $RECV 2>/dev/null; then HUNG=yes; kill $RECV 2>/dev/null; fi
  wait $RECV 2>/dev/null
  STOP=$(cat "$STOPFILE")
  STOPPED=$(grep -c '"stopped"' "$OUT/send-$i.ndjson")
  echo "run $i: sender stop->exit $((SEND_EXIT - STOP)) ms rc=$SEND_RC stopped_event=$STOPPED | receiver stop->exit $((RECV_EXIT - STOP)) ms hung=$HUNG"
done
