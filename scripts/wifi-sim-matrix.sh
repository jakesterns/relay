#!/usr/bin/env bash
# S49: the loopback matrix -- wired regression rows at the S32 settings, then
# each Wi-Fi profile -- one summary line per run into target/wifi-sim/matrix.jsonl.
#
#   scripts/wifi-sim-matrix.sh [secs]
set -u
SECS=${1:-60}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/target/wifi-sim/matrix.jsonl"
mkdir -p "$(dirname "$OUT")"
: >"$OUT"
run() { bash "$ROOT/scripts/wifi-sim-check.sh" "$@" | tail -n 1 >>"$OUT"; }
# Wired: no link model, the S32 matrix settings. With BASE_BIN (a build of
# main with only the test source added) each row runs on both builds, A/B.
wired() {
  for row in "1920x1080 60 25" "2560x1440 60 40" "3840x2160 60 60" "3840x2160 30 40"; do
    # shellcheck disable=SC2086
    run none 30 $row
    if [ -n "${BASE_BIN:-}" ]; then
      # shellcheck disable=SC2086
      RELAY_SHARE_BIN="$BASE_BIN" TAG_PREFIX=base- run none 30 $row
    fi
  done
}
wired
# Wi-Fi.
run wifi-good "$SECS" 3840x2160 60 60
run wifi-good "$SECS" 2560x1440 60 40
run wifi-busy "$SECS" 3840x2160 60 60
run wifi-busy "$SECS" 2560x1440 60 40
run wifi-bad "$SECS" 2560x1440 60 40
run wifi-bad "$SECS" 1920x1080 60 25
run capacity-drop "$SECS" 3840x2160 60 60
cat "$OUT"
