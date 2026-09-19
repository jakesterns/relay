"""Summarise latency drift from a receiver's NDJSON stats (B14).

Works on a headless run (`capture_to_arrival_last_ms`) and on a windowed one
(`capture_to_present_ms`), so the same script reads a loopback check and a
two-PC run.
"""
import json
import sys

vals = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    if '"stats"' not in line:
        continue
    try:
        ev = json.loads(line)
    except ValueError:
        continue
    if not ev.get("aus"):
        continue
    v = ev.get("capture_to_arrival_last_ms", ev.get("capture_to_present_ms"))
    if v is not None:
        vals.append(v)

vals = vals[4:]  # the first two seconds include connect and the first IDR
if len(vals) < 20:
    sys.exit("too few stats lines: %d" % len(vals))
n = max(1, len(vals) // 10)
med = lambda a: sorted(a)[len(a) // 2]
first, last = med(vals[:n]), med(vals[-n:])
minutes = len(vals) * 0.5 / 60
print("samples %d (%.1f min)" % (len(vals), minutes))
print("first-10%% median %.3f ms   last-10%% median %.3f ms" % (first, last))
print("drift %.3f ms/min   min %.3f   max %.3f   negative %d" % (
    (last - first) / minutes, min(vals), max(vals), sum(v < 0 for v in vals)))
