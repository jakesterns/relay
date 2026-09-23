#!/usr/bin/env bash
# S19 check on one PC: the receiver sends a process's audio back and the
# sender hears it. Headless loopback for video (safe on the PC being
# captured). A PowerShell SoundPlayer plays Relay's demo clip (speech/music,
# never a tone) as the "call app"; the receiver returns that process's
# output; the sender's stats must show return packets and a non-zero peak.
# The sender plays the return on this PC's default endpoint for ~10 s.
#
#   scripts/return-check.sh
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${RELAY_SHARE_BIN:-$ROOT/target/debug/relay-share.exe}"
OUT="$ROOT/target/return"
mkdir -p "$OUT"
export RELAY_INSTANCE="return$$"
WAV="$LOCALAPPDATA/Relay/previews/original.wav"
[ -f "$WAV" ] || { echo "no demo clip at $WAV"; exit 1; }
NAME="rc-$$"
PIDFILE="$OUT/player.pid"; rm -f "$PIDFILE"

# The "call app": plays the clip on a loop and writes its own Windows PID.
WAVWIN=$(cygpath -w "$WAV"); PIDWIN=$(cygpath -w "$PIDFILE")
powershell -NoProfile -Command "\$pid | Out-File -Encoding ascii '$PIDWIN'; \$p = New-Object System.Media.SoundPlayer '$WAVWIN'; for (\$i = 0; \$i -lt 4; \$i++) { \$p.PlaySync() }" >/dev/null 2>&1 &
PLAYER=$!
for _ in $(seq 1 50); do [ -s "$PIDFILE" ] && break; sleep 0.1; done
PLAYPID=$(tr -d '[:space:]' < "$PIDFILE")
echo "call app (SoundPlayer) pid=$PLAYPID"

"$BIN" recv --headless --code 313131 --name "$NAME" --return-pid "$PLAYPID" >"$OUT/recv.ndjson" 2>"$OUT/recv.log" &
RECV=$!
sleep 3
(sleep 10; echo stop) | "$BIN" send --peer "$NAME" --code 313131 --fps 30 --bitrate 8 --no-audio >"$OUT/send.ndjson" 2>"$OUT/send.log"
echo "sender rc=$?"
for _ in $(seq 1 100); do kill -0 $RECV 2>/dev/null || break; sleep 0.1; done
kill $RECV 2>/dev/null; wait $RECV 2>/dev/null
kill $PLAYER 2>/dev/null; taskkill //PID "$PLAYPID" //F >/dev/null 2>&1

echo "--- receiver: return pipeline"
grep -ohE "return audio pipeline up[^\"]*|predates the return route|returning the call app" "$OUT/recv.log" | sort -u
echo "--- receiver: last stats"
grep '"event":"stats"' "$OUT/recv.ndjson" | tail -1 | grep -oE '"return_packets":[0-9]+,"return_peak":[0-9.]+'
echo "--- sender: return track"
grep -ohE "return audio track arrived[^\"]*|audio playback up[^\"]*" "$OUT/send.log" | sort -u
echo "--- sender: stats (return packets / peak over time)"
grep '"event":"stats"' "$OUT/send.ndjson" | grep -oE '"return_packets":[0-9]+,"return_peak":[0-9.]+' | awk 'NR%4==1' | tr '\n' ' '; echo
