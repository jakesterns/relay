#!/usr/bin/env bash
# B17 / S35 check on one PC: does a remembered fingerprint actually match on
# the next share? Headless loopback (safe on the PC being captured). Both
# ends share this PC's identity and peers.json, so the entry the code pairing
# writes is this PC remembering itself -- which is exactly what a second PC
# would hold, and enough to prove the fingerprint is stable across engine
# runs and the trusted path completes end to end.
#
#   scripts/trusted-check.sh
#
# Touches the live data dir (identity upgrade, a self-entry in peers.json);
# peers.json is put back as it was.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${RELAY_SHARE_BIN:-$ROOT/target/debug/relay-share.exe}"
OUT="$ROOT/target/trusted"
mkdir -p "$OUT"
export RELAY_INSTANCE="trusted$$"
DATA="$LOCALAPPDATA/Relay/data"
PEERS="$DATA/peers.json"
HAD_PEERS=no
if [ -f "$PEERS" ]; then HAD_PEERS=yes; cp "$PEERS" "$OUT/peers.backup.json"; fi
NAME="tc-$$"

share() { # <n> <recv code> <send flags...>
  local n=$1 code=$2; shift 2
  "$BIN" recv --headless --code "$code" --name "$NAME" >"$OUT/recv-$n.ndjson" 2>"$OUT/recv-$n.log" &
  local RECV=$!
  sleep 3
  (sleep 5; echo stop) | "$BIN" send --peer "$NAME" --fps 30 --bitrate 8 --no-audio "$@" >"$OUT/send-$n.ndjson" 2>"$OUT/send-$n.log"
  local rc=$?
  for _ in $(seq 1 100); do kill -0 $RECV 2>/dev/null || break; sleep 0.1; done
  kill $RECV 2>/dev/null; wait $RECV 2>/dev/null
  return $rc
}

echo "identity before: $(ls "$DATA" | grep -E '^identity' | tr '\n' ' ')"

echo "--- run 1: code pairing"
share 1 424242 --code 424242; echo "sender rc=$?"
grep -o '"event":"paired"[^}]*' "$OUT/recv-1.ndjson" || echo "NO paired event"
grep -ohE 'identity (upgraded|wrapped|generated)[^"]*' "$OUT/recv-1.log" "$OUT/send-1.log" | sort -u
FP=$(python -c "import json;d=json.load(open(r'$PEERS'));print(d['peers'][0]['fingerprint'])")
echo "remembered fingerprint: ${FP:0:40}..."

echo "--- run 2: no code, trusted by fingerprint"
share 2 999999 --trusted "$FP"; echo "sender rc=$?"
grep -o '"event":"paired"[^}]*' "$OUT/recv-2.ndjson" || echo "NO paired event"
grep -o '"event":"connected"[^}]*' "$OUT/send-2.ndjson" || echo "NO connected event"
grep -ohE 'identity (upgraded|wrapped|generated)[^"]*' "$OUT/recv-2.log" "$OUT/send-2.log" | sort -u

echo "--- run 3: a fingerprint nobody remembers must be refused"
share 3 999999 --trusted "sha-256 00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF"; echo "sender rc=$?"
grep -o '"event":"error"[^}]*' "$OUT/send-3.ndjson" || tail -2 "$OUT/send-3.log"
grep -c '"event":"paired"' "$OUT/recv-3.ndjson" | sed 's/^/receiver paired events: /'

echo "identity after: $(ls "$DATA" | grep -E '^identity' | tr '\n' ' ')"
if [ "$HAD_PEERS" = yes ]; then cp "$OUT/peers.backup.json" "$PEERS"; else rm -f "$PEERS"; fi
echo "peers.json restored (had one before: $HAD_PEERS)"
