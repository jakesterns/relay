"""Summarise one S49 loopback run: the sender's stats lines and the headless
receiver's summary. Usage: wifi-sim-summary.py TAG SEND.ndjson RECV.ndjson RC
"""
import json
import sys


def lines(path):
    out = []
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for l in f:
                l = l.strip()
                if l.startswith("{"):
                    try:
                        out.append(json.loads(l))
                    except ValueError:
                        pass
    except OSError:
        pass
    return out


tag, send_path, recv_path, rc = sys.argv[1:5]
send = lines(send_path)
recv = lines(recv_path)
stats = [s for s in send if s.get("event") == "stats"]
rates = [s.get("bitrate_mbps", 0) for s in stats]
targets = [s.get("adapt", {}).get("target_mbps") for s in stats if s.get("adapt")]
rungs = []
for s in send:
    if s.get("event") == "rung":
        rungs.append(f'{s["height"]}p{s["fps"]}({s.get("rebuild_ms", 0)}ms)')
summ = next((r for r in recv if r.get("event") == "summary"), {})
p = summ.get("playout", {})
lat = summ.get("capture_to_arrival_ms", {})
shown = p.get("capture_to_shown_ms", {})
last_send = stats[-1] if stats else {}
link = last_send.get("link", {}).get("label", "?")
res = {
    "run": tag,
    "rc": int(rc),
    "secs": len(stats) / 2,
    "sent_mbps_mean": round(sum(rates) / len(rates), 1) if rates else 0,
    "target_mbps_min": round(min(t for t in targets if t is not None), 1) if targets else None,
    "target_mbps_end": targets[-1] if targets else None,
    "rungs": rungs,
    "frames_shown": p.get("frames"),
    "stalls_over_100ms": p.get("stalls"),
    "max_gap_ms": round(p.get("max_gap_ms", 0), 1),
    "judder_ms": round(p.get("judder_ms", 0), 2),
    "playout_ms_end": round(p.get("target_ms", 0), 1),
    "arrival_ms_p50_p99": [round(lat.get("p50", 0), 1), round(lat.get("p99", 0), 1)],
    "shown_ms_p50_p99": [round(shown.get("p50", 0), 1), round(shown.get("p99", 0), 1)],
    "lost": p.get("rtp_lost"),
    "gaps": p.get("rtp_gaps"),
    "recovered": p.get("rtp_recovered"),
    "keyframe_requests": p.get("keyframe_requests"),
    "withheld": p.get("frames_withheld"),
    "reorder_hold_ms": p.get("reorder_hold_ms"),
    "keyframes_sent": last_send.get("keyframes"),
    "link": link,
}
print(json.dumps(res))
