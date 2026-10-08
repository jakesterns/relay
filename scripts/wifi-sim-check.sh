#!/usr/bin/env bash
# S49: a real loopback share (capture -> hardware encode -> webrtc -> headless
# receiver) through the Wi-Fi link model, on one PC. Safe on the PC being used:
# the receiver is headless, and the sender captures a synthetic noise pattern
# (RELAY_TEST_SOURCE), not the screen, so the encoder runs at its target.
#
#   scripts/wifi-sim-check.sh PROFILE SECS SIZE FPS MBPS [extra send flags...]
#   scripts/wifi-sim-check.sh wifi-busy 90 3840x2160 60 60
#   scripts/wifi-sim-check.sh wired 45 1920x1080 60 25
#
# PROFILE is any RELAY_TEST_NET spec (transport/netsim.rs), or "none" for no
# model at all. Prints one summary line (scripts/wifi-sim-summary.py); the raw
# NDJSON and logs are in target/wifi-sim/.
set -u
PROFILE=${1:?profile}; SECS=${2:-60}; SIZE=${3:-1920x1080}; FPS=${4:-60}; MBPS=${5:-25}
shift 5 2>/dev/null
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${RELAY_SHARE_BIN:-$ROOT/target/debug/relay-share.exe}"
OUT="$ROOT/target/wifi-sim"
mkdir -p "$OUT"
TAG="${TAG_PREFIX:-}${PROFILE%%,*}-${SIZE}-${FPS}-${MBPS}"
export RELAY_INSTANCE="wifisim$$"
if [ "$PROFILE" != "none" ]; then export RELAY_TEST_NET="$PROFILE"; fi
export RELAY_TEST_SOURCE="noise:$SIZE"
NAME="wifisim-$$"
CODE=$(printf '%06d' $(( $$ % 1000000 )))

"$BIN" recv --headless --code "$CODE" --name "$NAME" >"$OUT/$TAG.recv.ndjson" 2>"$OUT/$TAG.recv.log" &
RECV=$!
sleep 3
(sleep "$SECS"; echo stop) | "$BIN" send --peer "$NAME" --code "$CODE" --fps "$FPS" \
  --bitrate "$MBPS" --no-audio "$@" >"$OUT/$TAG.send.ndjson" 2>"$OUT/$TAG.send.log"
SEND_RC=$?
for _ in $(seq 1 100); do kill -0 $RECV 2>/dev/null || break; sleep 0.1; done
if kill -0 $RECV 2>/dev/null; then kill $RECV 2>/dev/null; fi
wait $RECV 2>/dev/null
python "$ROOT/scripts/wifi-sim-summary.py" "$TAG" "$OUT/$TAG.send.ndjson" "$OUT/$TAG.recv.ndjson" "$SEND_RC"
