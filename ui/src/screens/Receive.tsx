import { useEffect, useRef, useState } from "react";
import { Card, ErrorNote, Kv, Live } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { ago } from "../lib/ago";
import { MixerCard, type MixerRow } from "../components/Mixer";
import type { ProcessInfo } from "../lib/ipc";
import { HealthTracker, healthText, type HealthDelta, type HealthState } from "../lib/health";
import {
  api, codecLabel, onCoreEvents, type FirewallStatus, type Peer, type ShareCapabilities,
  type ShareStats, type StreamStatus, type VdeviceStatus, type VideoArea, type VideoCodec,
} from "../lib/ipc";

/** Warn before the user tries, not after it fails.
 *
 *  A share runs on HEVC or H.264, negotiated per share (S27). H.264 decode
 *  ships with every Windows install, so "cannot receive" now means no decoder
 *  for *either* codec — an N edition without the Media Feature Pack — and a PC
 *  without HEVC decode gets a plain note, not an error: its shares work, at a
 *  higher bitrate for the same picture. Relay is free, so nothing here sends
 *  anyone to buy a codec. */
export function CodecBanner({ need }: { need: "share" | "receive" }) {
  const { offline } = useCore();
  const [caps, setCaps] = useState<ShareCapabilities | null>(null);

  // Re-probe when the window regains focus: a Media Feature Pack or codec
  // install happens in another window, and the user comes back expecting
  // Relay to have noticed.
  useEffect(() => {
    let live = true;
    const probe = () => {
      api.shareCapabilities()
        .then((c) => { if (live) setCaps(c); })
        .catch(() => { if (live) setCaps(null); });
    };
    probe();
    window.addEventListener("focus", probe);
    return () => { live = false; window.removeEventListener("focus", probe); };
  }, [offline]);

  if (!caps) return null;
  if (need === "receive" && !caps.can_receive) {
    // `.offline` is a flex row, so every text node would become a column;
    // `.msg` keeps it a single inline flow.
    return (
      <div className="offline"><i />
        <span className="msg">
          This PC has no H.264 or HEVC video decoder, so Relay cannot show a shared screen.
          H.264 decoding is part of Windows; on an N edition of Windows, install Microsoft's free
          Media Feature Pack, then come back — this banner clears itself.
        </span>
      </div>
    );
  }
  if (need === "receive" && caps.receive_codecs && !caps.receive_codecs.includes("hevc")) {
    return (
      <p className="note" data-testid="codec-note">
        Shares to this PC use H.264, because it has no HEVC decoder. Everything works; HEVC gives
        the same picture at a lower bitrate, and Relay uses it automatically wherever both PCs
        have it.
      </p>
    );
  }
  if (need === "share" && !caps.can_share) {
    // Name the GPU. "This GPU" is useless on the machine this matters on most:
    // a laptop where the display hangs off the iGPU and the encoder is on the
    // dGPU, so the user has somewhere specific to go.
    const gpu = caps.adapters.length ? caps.adapters.join(" and ") : "this PC's GPU";
    return (
      <div className="offline"><i />
        <span className="msg">
          No hardware HEVC or H.264 encoder on {gpu}. Relay encodes in hardware only (NVENC / Quick
          Sync / AMF) — there is no software encode path, so this PC can receive a share but not
          send one. If the GPU is recent, update its graphics driver: Windows only lists the encoder
          once the vendor driver is installed.
        </span>
      </div>
    );
  }
  return null;
}

/** Say "Windows Firewall is blocking this", not "the other PC never
 *  connected".
 *
 *  Windows prompts the first time a given path listens, and dismissing that
 *  prompt writes a Block rule that is permanent, invisible and never
 *  mentioned again. Every symptom after that points at the network: the
 *  pairing code is accepted, mDNS finds nothing, the share sits waiting. So
 *  the same warn-before-you-fail shape as `CodecBanner` — read the state when
 *  the screen opens, and if it is going to fail, name the real reason and
 *  offer the one action that fixes it.
 *
 *  The fix goes through the elevated helper, which means a UAC prompt the
 *  user can read and decline. Declining is a supported answer: the banner
 *  stays, the rest of the app keeps working, and on a network that is not
 *  dropping inbound traffic the share works anyway (`permissive`). */
export function FirewallBanner() {
  const { offline } = useCore();
  const [fw, setFw] = useState<FirewallStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  const probe = () => {
    api.firewallStatus().then(setFw).catch(() => setFw(null));
  };
  useEffect(() => {
    let live = true;
    api.firewallStatus()
      .then((s) => { if (live) setFw(s); })
      .catch(() => { if (live) setFw(null); });
    return () => { live = false; };
  }, [offline]);

  // A probe that failed says nothing rather than guessing, and a healthy
  // machine gets no banner at all.
  if (!fw || fw.unknown) return null;
  if (fw.state === "allowed" || fw.state === "firewall_off" || fw.state === "permissive") return null;

  const allow = async () => {
    setBusy(true);
    setNote(null);
    try {
      const r = await api.runElevated("allow_firewall");
      // `declined` is a normal answer, not an error: nothing was changed and
      // saying so plainly is the whole point.
      setNote(r.declined
        ? "You declined the Windows permission prompt, so nothing was changed. Relay still works everywhere it can; only incoming shares to this PC stay blocked."
        : r.lines.join(" "));
      probe();
    } catch (e) {
      setNote(String((e as { message?: string })?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  const body =
    fw.state === "blocked" ? (
      <>
        <b>Windows Firewall is blocking Relay.</b> This is a firewall rule, not a network
        fault — {fw.blocking_rules === 1 ? "a rule was" : `${fw.blocking_rules} rules were`} created
        when somebody dismissed Windows' "allow this app?" prompt, and Windows applies a block
        before any allow. Incoming shares to this PC will not connect until it is removed.
      </>
    ) : fw.state === "public_network" ? (
      <>
        <b>This network is set to Public.</b> Relay only opens the firewall on private and domain
        networks, so a share cannot reach this PC here. Set the network to Private in Windows
        Settings → Network &amp; internet, or connect to your home or work network.
      </>
    ) : (
      <>
        <b>Windows will ask whether to allow Relay</b> the first time you share, and declining that
        prompt blocks Relay permanently with no visible cause. You can settle it now instead.
      </>
    );

  return (
    <div className="offline">
      <i />
      <div>
        {body}
        {fw.state !== "public_network" && (
          <div style={{ marginTop: 8 }}>
            <button className="btn q" disabled={busy} onClick={() => void allow()}>
              {busy ? "Waiting for Windows…" : "Allow Relay through Windows Firewall…"}
            </button>
          </div>
        )}
        <p className="note" style={{ marginTop: 8 }}>
          {note ?? (
            <>
              Relay asks Windows for permission — it never takes it silently. The rule covers
              private and domain networks only, never public, and the uninstaller removes it.
            </>
          )}
        </p>
      </div>
    </div>
  );
}

/** Whether a call on this PC will actually see the incoming share.
 *
 *  This is the question the Receive screen exists to answer and previously
 *  did not: the virtual camera only appears in Discord/Zoom/Meet if it was
 *  consented to *and* registered, and both are decided elsewhere. Saying so
 *  here saves a support round-trip with someone staring at a camera picker. */
function VirtualDeviceCard() {
  const { offline, mock } = useCore();
  const [vd, setVd] = useState<VdeviceStatus | null>(null);

  useEffect(() => {
    let live = true;
    api.vdeviceStatus()
      .then((s) => { if (live) setVd(s); })
      .catch(() => { if (live) setVd(null); });
    return () => { live = false; };
  }, [offline]);

  if (!vd) {
    return (
      <Card title="In calls">
        <p className="note">{offline && !mock ? "Relay is not running — status unknown." : "Reading…"}</p>
      </Card>
    );
  }

  const camera = !vd.camera_supported
    ? `Needs Windows 11 22H2+ (this PC: build ${vd.windows_build ?? "?"})`
    : vd.camera_registered
      ? '"Relay Camera" — pick it in Discord, Zoom or Meet'
      : vd.consent?.camera
        ? "Consented, not installed yet — finish in Settings"
        : "Not enabled — turn it on in Settings";

  const mic = vd.mic_targets.length > 0
    ? vd.mic_targets[0].name
    : "No route yet — the signed driver ships later; VB-Cable works meanwhile";

  return (
    <Card title="In calls">
      <Kv k="Camera" v={camera} />
      <Kv k="Microphone" v={mic} />
      {!vd.camera_registered && vd.camera_supported && (
        <p className="note">Without the virtual camera the share still plays here in Relay — it just
          cannot be picked as a webcam.</p>
      )}
      {vd.obs_virtualcam && !vd.camera_registered && (
        <p className="note">OBS VirtualCam is installed on this PC, but Relay does not feed it.</p>
      )}
    </Card>
  );
}

/** The video area's place on screen, for the shell to put the stream window
 *  over it (S29). The stream is a native window, not an element: it sits on
 *  top of this box and follows it. Reported on mount, on every size change
 *  and on every layout-affecting event; `null` on unmount so nothing is left
 *  over a screen that no longer shows a video area. */
function useVideoArea(deps: unknown[]) {
  const ref = useRef<HTMLDivElement | null>(null);
  const report = () => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    // Clip to the viewport: the window must never cover chrome outside
    // the client area, and an off-screen slice has nothing to show.
    const x = Math.max(r.left, 0), y = Math.max(r.top, 0);
    const w = Math.min(r.right, window.innerWidth) - x;
    const h = Math.min(r.bottom, window.innerHeight) - y;
    const area: VideoArea | null = w > 0 && h > 0 ? { x, y, w, h } : null;
    void api.setVideoArea(area).catch(() => {});
  };
  // Observers live for the screen's lifetime; `null` goes out only when it
  // unmounts. Re-running this on every render dependency used to send a
  // `null` and then the rect a millisecond apart, which the shell dutifully
  // turned into a hide and a show of the stream window (seen in ui.log on
  // the second PC).
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    report();
    // jsdom has no ResizeObserver; the mount report above still runs there.
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(report) : null;
    ro?.observe(el);
    if (el.parentElement) ro?.observe(el.parentElement);
    window.addEventListener("resize", report);
    return () => {
      ro?.disconnect();
      window.removeEventListener("resize", report);
      void api.setVideoArea(null).catch(() => {});
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Anything around the video area that can move it: re-measure only.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(report, deps);
  return ref;
}

/**
 * Receive mode. Advertises this PC over mDNS and shows an incoming share
 * right here, in the video area: the share engine renders into a native
 * window that the shell keeps over that box, and a pop-out control turns it
 * into a window of its own and back (S29). The pairing code shown here is
 * what the sender types on its Share screen.
 */
/** The numbers that used to exist only in `share.log`.
 *
 *  Every night spent debugging the second PC's picture was spent reading a log
 *  file over a chat session, because the app itself showed nothing about the
 *  stream it was playing. This is that information, in the instrument-strip
 *  idiom: a readout, not an alert.
 *
 *  `rtp_recovered` is shown next to `rtp_lost` deliberately. Repaired-but-not-
 *  lost is the signature of a lossy link that Relay is handling, and someone
 *  looking at a perfect picture with a large repair count should be able to
 *  see that rather than conclude the counters are broken. */
function StreamHealthCard(
  { on, s, state, d }:
  { on: boolean; s: ShareStats | null; state: HealthState; d: HealthDelta },
) {
  if (!on || !s) return null;
  const n = (v: number | undefined) => (typeof v === "number" && Number.isFinite(v) ? v : null);
  const fps = n(s.fps);
  const mbps = n(s.bitrate_mbps);
  const latency = n(s.capture_to_present_ms);
  const audioMs = n(s.audio?.buffered_ms);
  const lost = n(s.rtp_lost) ?? 0;
  const recovered = n(s.rtp_recovered) ?? 0;

  return (
    <Card title="Stream health">
      {fps !== null && <Kv k="Frame rate" v={`${fps.toFixed(1)} fps`} mono />}
      {mbps !== null && <Kv k="Bitrate" v={`${mbps.toFixed(1)} Mb/s`} mono />}
      {/* Latency is safe to show since B14: the clock is re-estimated during
          the share, so this no longer drifts and no longer goes negative. */}
      {latency !== null && <Kv k="Latency" v={`${latency.toFixed(1)} ms`} mono />}
      {audioMs !== null && <Kv k="Audio buffer" v={`${audioMs.toFixed(0)} ms`} mono />}
      <Kv k="Repaired" v={recovered.toLocaleString()} mono />
      <Kv k="Lost" v={lost.toLocaleString()} mono />
      {state === "coping" && d.recovered > 0 && (
        <p className="note" data-testid="health-coping">
          {/* Not "dropping": on a clean wired LAN a keyframe burst arrives slightly out of
              order and counts here too (r36, row 6). */}
          Some video packets are arriving late or out of order, and Relay is putting them back
          in time. The picture is not affected.
        </p>
      )}
    </Card>
  );
}

/** PCs that may send to this one without a code (S35).
 *
 *  They still need this screen to be on Start receiving. Remembering removed
 *  the code, not the consent -- Jake's call in the trust model (§5): nothing
 *  can put a picture on this screen unasked. Forget is real, not a hidden
 *  row: the PC needs a code again, like a stranger. */
function TrustedSendersCard({ tick }: { tick: number }) {
  const { offline } = useCore();
  const [peers, setPeers] = useState<Peer[]>([]);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api.listPeers()
      .then((p) => { if (live) setPeers(p); })
      .catch(() => { if (live) setPeers([]); });
    return () => { live = false; };
  }, [offline, tick]);

  // Stays for the note after the last Forget: a card that vanishes the moment
  // you act on it looks like the click did nothing.
  if (peers.length === 0 && !note) return null;

  const forget = async (p: Peer) => {
    try {
      await api.forgetPeer(p.id);
      setPeers((was) => was.filter((x) => x.id !== p.id));
      setNote(`${p.name} will need a code next time.`);
    } catch (e) { setNote(errText(e)); }
  };
  const direction = (p: Peer) =>
    p.last_direction === "received_from" ? "shared to this PC" : "received a share from this PC";

  return (
    <Card title="Remembered PCs">
      {peers.map((p) => (
        <div key={p.id} className="dev" data-testid="trusted-row">
          <div>
            <b>{p.favourite ? "★ " : ""}{p.name}</b>
            <span>Last {direction(p)} · {ago(p.last_seen_unix)}</span>
          </div>
          <button className="btn q" style={{ marginLeft: "auto" }} onClick={() => void forget(p)}>
            Forget
          </button>
        </div>
      ))}
      <p className="note">
        {note ?? "These PCs connect without a code — but only while Start receiving is on here. Nothing can put a picture on this screen unasked."}
      </p>
    </Card>
  );
}

/** Where the call app's choice is kept between runs (S19). The exe name,
 *  not the PID: a PID is new every launch, the program is not. */
const CALL_APP_KEY = "relay.callApp.exe";

/** The return route (S19): pick the call app on this PC and its audio —
 *  the other participants, never this PC's own mic — goes back to the
 *  sending PC as one more track. Locked while receiving: the engine read
 *  the choice when it started. */
function CallReturnCard({ value, locked, onChange }: {
  value: ProcessInfo | null; locked: boolean; onChange: (p: ProcessInfo | null) => void;
}) {
  const [picking, setPicking] = useState(false);
  const [procs, setProcs] = useState<ProcessInfo[]>([]);
  const open = () => {
    setPicking(true);
    void api.listProcesses().then(setProcs).catch(() => setProcs([]));
  };
  return (
    <Card title="Send the call back">
      <Kv k="Call app" v={value ? value.exe : "None"} />
      {!locked && !picking && (
        <div className="row">
          <button className="btn q" onClick={open}>{value ? "Change…" : "Pick the call app…"}</button>
          {value && <button className="btn q" onClick={() => onChange(null)}>Off</button>}
        </div>
      )}
      {!locked && picking && (
        <label className="pick">
          <select value="" aria-label="Call app"
            onChange={(e) => {
              const p = procs.find((x) => String(x.pid) === e.target.value) ?? null;
              if (p) { onChange(p); setPicking(false); }
            }}>
            <option value="" disabled>{procs.length ? "Pick the call app" : "Loading…"}</option>
            {procs.map((p) => (
              <option key={p.pid} value={String(p.pid)}>{p.exe} — {p.title}</option>
            ))}
          </select>
        </label>
      )}
      <p className="note">
        {value
          ? "The other people on the call are heard on the sending PC. They never hear themselves: only the call app's own output goes back, never this PC's microphone."
          : "Pick Discord, Zoom or whatever the call runs in, and the sending PC hears the other people — never itself."}
      </p>
    </Card>
  );
}

export function Receive() {
  const { mock } = useCore();
  const [receiving, setReceiving] = useState(false);
  // The call app whose audio goes back to the sender (S19); remembered by
  // exe name and re-found among the running programs on the next visit.
  const [callApp, setCallApp] = useState<ProcessInfo | null>(null);
  useEffect(() => {
    const saved = localStorage.getItem(CALL_APP_KEY);
    if (!saved) return;
    let live = true;
    api.listProcesses().then((ps) => {
      if (!live) return;
      const p = ps.find((x) => x.exe === saved);
      if (p) setCallApp(p);
    }).catch(() => {});
    return () => { live = false; };
  }, []);
  const pickCallApp = (p: ProcessInfo | null) => {
    setCallApp(p);
    if (p) localStorage.setItem(CALL_APP_KEY, p.exe);
    else localStorage.removeItem(CALL_APP_KEY);
  };
  const [code, setCode] = useState<string | null>(null);
  const [sender, setSender] = useState<string | null>(null);
  const [codec, setCodec] = useState<VideoCodec | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stream, setStream] = useState<StreamStatus | null>(null);
  // Who the last share came from, kept after it ends so the video area can
  // say so rather than snapping back to the idle prompt as if nothing
  // happened. A frozen last frame was the old way of "saying" it.
  const [ended, setEnded] = useState<string | null>(null);
  // The last share ended because its sender stopped it, not a drop.
  const [endedClean, setEndedClean] = useState(false);
  // This PC pressed Stop receiving: it ended the share, not the sender.
  const [stoppedHere, setStoppedHere] = useState(false);
  // The current sender connected without a code, as a remembered PC (S35);
  // and a counter that tells the remembered-PCs card to reload, because a
  // share starting or ending is what changes that list.
  const [trusted, setTrusted] = useState(false);
  const [peersTick, setPeersTick] = useState(0);
  // Stream health (S31). The tracker is a ref, not state: it is fed on every
  // stats line and only the *derived* state should cause a render.
  const health = useRef(new HealthTracker());
  const [healthState, setHealthState] = useState<HealthState>("ok");
  const [healthDelta, setHealthDelta] = useState<HealthDelta>(
    { lost: 0, recovered: 0, withheld: 0, keyframeRequests: 0 },
  );
  const [live, setLive] = useState<ShareStats | null>(null);

  useEffect(() => {
    let live = true;
    // A screen that opens mid-receive learns where things stand from the
    // shell, which saw every event go past; the core does not replay them.
    // The running receiver's call app, named by its exe. A window opened
    // mid-receive only learns it from the shell's stream status: the core's
    // replayed ReceiveStatus goes past before this page is listening.
    const showReturnApp = (pid?: number | null, exe?: string | null) => {
      if (!pid) return;
      api.listProcesses().then((ps) => {
        if (!live) return;
        setCallApp(ps.find((x) => x.pid === pid)
          ?? { pid, exe: exe || "an app that has closed", title: "", hwnd: 0 });
      }).catch(() => {});
    };
    api.streamStatus().then((s) => {
      if (!live) return;
      setStream(s);
      if (s.receiving) {
        setReceiving(true);
        if (s.code) setCode(s.code);
        if (s.sender) setSender(s.sender);
        if (s.codec) setCodec(s.codec);
        showReturnApp(s.return_pid, s.return_exe);
      }
    }).catch(() => {});
    let unsub = () => {};
    void onCoreEvents({
      receiveStatus: (s) => {
        // A restart the core is already making is still "receiving": showing
        // Idle and "ended" for the second it takes would contradict itself.
        setReceiving(s.receiving || !!s.restarting);
        if (s.code) setCode(s.code);
        // The running receiver's call app, whoever started it (a resume, or
        // another window): the card shows what the receiver is doing.
        if (s.receiving) showReturnApp(s.return_pid, s.return_exe);
        if (s.sender) {
          setSender(s.sender); setEnded(null); setEndedClean(false);
          setTrusted(!!s.trusted);
          setPeersTick((t) => t + 1);
        }
        if (s.message) setError(s.message);
        if (s.codec) setCodec(s.codec);
        if (!s.receiving) {
          setSender((was) => { if (was) setEnded(was); return null; });
          setEndedClean(!!s.ended_by_sender);
          setCode(null); setCodec(null);
          setTrusted(false);
          setPeersTick((t) => t + 1);
          // A dead share leaves no health behind it. Without this the next
          // one inherits a stale "degraded", and the tracker would diff a
          // fresh engine's counters against the old engine's totals.
          health.current.reset();
          setHealthState("ok");
          setLive(null);
        }
      },
      shareStats: (s) => {
        // Only the receiver's line carries these; the sender's has none of
        // them and the tracker ignores it.
        setHealthState(health.current.push(s));
        setHealthDelta(health.current.delta());
        setLive(s);
      },
      stream: (s) => setStream(s),
    }).then((u) => { unsub = u; });
    return () => { live = false; unsub(); };
  }, []);

  const start = async () => {
    setBusy(true); setError(null); setEnded(null); setStoppedHere(false);
    health.current.reset(); setHealthState("ok"); setLive(null);
    try {
      await api.startReceive(callApp ? { return_pid: callApp.pid } : {});
      setReceiving(true);
    }
    catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };
  const stop = async () => {
    setBusy(true); setStoppedHere(true);
    try { await api.stopReceive(); } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };
  const setMode = async (mode: "embedded" | "popout") => {
    setError(null);
    try { await api.setStreamMode(mode); } catch (e) { setError(errText(e)); }
  };

  const embedded = !!stream?.live && stream.mode === "embedded";
  const popped = !!stream?.live && stream.mode === "popout";
  // Re-measure whenever what is around the video area can change.
  const sceneRef = useVideoArea([receiving, sender, error, embedded]);

  const healthMsg = receiving ? healthText(healthState, healthDelta) : null;

  // S37: one fader per track that has actually arrived. The program track
  // is there as soon as a sender is; the others only once their packets are.
  const receiveRows: MixerRow[] = sender
    ? [
        { key: "app" as const, label: "Their audio" },
        ...(((live?.rest_packets ?? 0) > 0) ? [{ key: "rest" as const, label: "Everything else on their PC" }] : []),
        ...(((live?.mic_packets ?? 0) > 0) ? [{ key: "mic" as const, label: "Their microphone" }] : []),
      ]
    : [];

  const areaText = popped
    ? `Playing in its own window · ${sender ?? ""}`.trim()
    : embedded
      ? null
      : sender
        ? `Connected to ${sender} — waiting for the first frame…`
        : receiving
          // `ended` survives only an automatic restart: Start receiving by
          // hand clears it. So "receiving, with a name still remembered" is
          // exactly "the share dropped and the core brought the receiver
          // back on its own" (S38), and it says so rather than pretending
          // this is a fresh wait.
          ? ended
            ? endedClean
              ? `The share from ${ended} ended. Waiting for a sender to pair…`
              : `The share from ${ended} dropped — waiting for it to come back…`
            : "Waiting for a sender to pair…"
          : ended
            ? stoppedHere ? `You stopped receiving from ${ended}.` : `The share from ${ended} ended.`
            : "Press Start receiving, then enter the code on the sending PC.";

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Receive <em>— from another PC</em></h1>
          <Live on={receiving} text={receiving ? "Ready" : "Not receiving"} />
        </div>
        <OfflineBanner />
        <CodecBanner need="receive" />
        <FirewallBanner />
        <div className="preview" data-testid="video-area" data-stream={embedded ? "embedded" : popped ? "popout" : "none"}>
          {/* The stream is a native window the shell keeps over this box, so
              the box itself stays empty: anything painted here would sit
              under the picture. */}
          <div className="scene" ref={sceneRef} />
          {areaText && <div className="idlemsg">{areaText}</div>}
          {/* Sits in the corner over the picture's own window, never across
              it: the one thing worse than an unexplained bad picture is a
              message covering the picture you are trying to judge. */}
          {healthMsg && (
            <div className="healthchip" data-testid="health-chip" role="status">
              <i /><span>{healthMsg}</span>
            </div>
          )}
        </div>
      </section>
      <aside className="side">
        <Card title="Your pairing code">
          {code
            ? <div className="code paircode">{code}</div>
            // Before there is a code: six empty hairline slots in the same
            // box the digits will fill, so nothing moves when it arrives and
            // nothing at 34 px reads as an error.
            : <div className="paircode empty" role="img" aria-label="No code yet">
                {Array.from({ length: 6 }, (_, i) => <span key={i} />)}
              </div>}
          <p className="note">Type this on the other PC's Share screen.</p>
        </Card>
        <Card>
          <Kv k="Status" v={sender
            ? `Paired with ${sender}${trusted ? " · remembered, no code" : ""}`
            : receiving ? "Advertising on the LAN" : "Idle"} />
          {/* Named only once the stream says which: HEVC or H.264 is decided per
              share, by what both PCs can do. */}
          <Kv k="Codec" v={receiving && codec ? codecLabel(codec) : "—"} mono />
          {stream?.live && stream.width > 0 && (
            <Kv k="Stream" v={`${stream.width}×${stream.height}`} mono />
          )}
          {embedded && (
            <button className="btn q" onClick={() => void setMode("popout")}>Pop out into its own window</button>
          )}
          {popped && (
            <button className="btn q" onClick={() => void setMode("embedded")}>Bring back into Relay</button>
          )}
          {stream?.live && !stream.excluded_from_capture && (
            <p className="note">Windows could not hide the stream from screen capture on this PC, so
              sharing this PC's screen while receiving would show the stream inside itself.</p>
          )}
        </Card>
        <StreamHealthCard on={receiving} s={live} state={healthState} d={healthDelta} />
        <TrustedSendersCard tick={peersTick} />
        <CallReturnCard value={callApp} locked={receiving} onChange={pickCallApp} />
        <VirtualDeviceCard />
        <ErrorNote text={error} onDismiss={() => setError(null)} />
        {receiving
          ? <button className="btn acc" onClick={stop} disabled={busy}>Stop receiving</button>
          : <button className="btn acc" onClick={start} disabled={busy}>Start receiving</button>}
        {receiving && sender && (
          <MixerCard side="receive" rows={receiveRows} sessionKey={`recv-${sender}`} />
        )}
        {mock && <p className="note">Preview only — Relay isn't running.</p>}
        <p className="note">The stream plays here, in this window. Nothing on this PC is changed.</p>
      </aside>
    </>
  );
}
