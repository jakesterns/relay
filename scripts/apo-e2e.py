"""End-to-end check of the per-app EQ path on a live machine (S44e).

Profile -> relay-core -> shared memory -> the endpoint APO inside audiodg ->
the endpoint's loopback, measured with `relay-share apo-check observe`.

Needs: the APO installed on the default render endpoint and audiodg loading
it (`relay-share apo-check` passes), no other relay-core running (this script
starts its own, on a throwaway data dir, and stops only that process).

Plays a quiet -30 dBFS three-tone stimulus several times (a measured
automated run; never use this for a listening check).

Phases, each a fresh render stream (so audiodg builds a new APO instance):
  1. applied while nothing plays -> the core's keep-alive activates the chain
     on the stream's new instance;
  2. a second stream -> the held section means the chain is live at once;
  3. restore all -> flat again, section released.

Usage: python scripts/apo-e2e.py [--bin target/debug]
Exit 0 when every phase passes; the JSON summary goes to stdout.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
import uuid

PIPE = r"\\.\pipe\relay-core"
LOW, HIGH = 9.0, -8.0  # the profile's bands at the stimulus' low/high tones
REF = -30.0  # stimulus level per tone, dBFS


def call(method, params=None):
    msg = {"id": 1, "method": method}
    if params is not None:
        msg["params"] = params
    with open(PIPE, "r+b", buffering=0) as p:
        p.write((json.dumps(msg) + "\n").encode())
        r = b""
        while not r.endswith(b"\n"):
            c = p.read(1)
            if not c:
                break
            r += c
    return json.loads(r.decode())


def wait_pipe(secs=15):
    end = time.time() + secs
    while time.time() < end:
        try:
            call("ping")
            return True
        except OSError:
            time.sleep(0.2)
    return False


def observe(bin_dir, secs, settle):
    out = subprocess.run(
        [os.path.join(bin_dir, "relay-share.exe"), "apo-check", "observe", str(secs), str(settle)],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if out.returncode != 0:
        raise RuntimeError(f"observe failed: {out.stderr[-500:]}")
    return json.loads(out.stdout)


def judge(obs, want_eq):
    lv = obs["levels_db"]
    d = [lv[0] - REF, lv[1] - REF, lv[2] - REF]
    if want_eq:
        ok = d[0] >= LOW * 0.6 and d[2] <= HIGH * 0.6 and abs(d[1]) <= 2.0
    else:
        ok = all(abs(x) <= 1.0 for x in d)
    return ok, [round(x, 2) for x in d]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join("target", "debug"))
    a = ap.parse_args()
    try:
        call("ping")
        print("another relay-core is running; stop it first", file=sys.stderr)
        return 3
    except OSError:
        pass
    data = tempfile.mkdtemp(prefix="relay-apo-e2e-")
    core = subprocess.Popen(
        [os.path.join(a.bin, "relay-core.exe"), "--data-dir", data, "run"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    results = []
    try:
        if not wait_pipe():
            print("core did not come up", file=sys.stderr)
            return 3
        pid = str(uuid.uuid4())
        profile = {
            "id": pid,
            "name": "APO e2e",
            "game": {"exe": "relay-apo-e2e-none.exe"},
            "audio": {
                "bands": [
                    {"freq_hz": 100.0, "gain_db": LOW, "q": 0.7},
                    {"freq_hz": 6000.0, "gain_db": HIGH, "q": 0.8},
                ],
                "headset_correction": False,
                "learn_game_eq": False,
            },
            "status": "ready",
        }
        r = call("save_profile", {"profile": profile})
        assert r.get("type") != "error", r
        r = call("apply_profile", {"id": pid})
        assert r.get("type") != "error", r
        # The core's tick runs every second: settle past it on the first run.
        o = observe(a.bin, 4.0, 1.5)
        ok, d = judge(o, True)
        results.append({"phase": "applied before the stream", "pass": ok, "delta_db": d, **o})
        o = observe(a.bin, 3.0, 0.3)
        ok, d = judge(o, True)
        results.append({"phase": "new stream, held section", "pass": ok, "delta_db": d, **o})
        call("restore_all")
        o = observe(a.bin, 3.0, 0.5)
        ok, d = judge(o, False)
        results.append({"phase": "after restore", "pass": ok, "delta_db": d, **o})
        call("delete_profile", {"id": pid})
    finally:
        try:
            call("shutdown")
            core.wait(timeout=10)
        except Exception:
            core.kill()  # our own PID only
    print(json.dumps(results, indent=2))
    return 0 if results and all(r["pass"] for r in results) else 2


if __name__ == "__main__":
    sys.exit(main())
