import { useEffect, useState } from "react";
import { Card, ErrorNote, Kv, Live } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { api, onCoreEvents, type ShareCapabilities, type VdeviceStatus } from "../lib/ipc";

/** Warn before the user tries, not after it fails.
 *
 *  Hardware HEVC *decode* on Windows goes through the Microsoft HEVC Video
 *  Extension; GPU vendors register encode MFTs only. Without it `recv` dies on
 *  the first frame, which looks like a network problem and is not one. */
export function CodecBanner({ need }: { need: "share" | "receive" }) {
  const { offline } = useCore();
  const [caps, setCaps] = useState<ShareCapabilities | null>(null);

  useEffect(() => {
    let live = true;
    api.shareCapabilities()
      .then((c) => { if (live) setCaps(c); })
      .catch(() => { if (live) setCaps(null); });
    return () => { live = false; };
  }, [offline]);

  if (!caps) return null;
  if (need === "receive" && !caps.can_receive) {
    return (
      <div className="offline"><i />
        No HEVC decoder on this PC, so receiving would fail on the first frame. Install the free
        "HEVC Video Extensions from Device Manufacturer" from the{" "}
        <a href={HEVC_STORE_SEARCH}>Microsoft Store</a>, then reopen Relay. Relay cannot bundle it —
        Microsoft licenses that package to PC makers, not for redistribution by apps.
      </div>
    );
  }
  if (need === "share" && !caps.can_share) {
    // Name the GPU. "This GPU" is useless on the machine this matters on most:
    // a laptop where the display hangs off the iGPU and the encoder is on the
    // dGPU, so the user has somewhere specific to go.
    const gpu = caps.adapters.length ? caps.adapters.join(" and ") : "this PC's GPU";
    return (
      <div className="offline"><i />
        No hardware HEVC encoder on {gpu}. Relay encodes in hardware only (NVENC / Quick Sync /
        AMF) — there is no software encode path, so this PC can receive a share but not send one.
        If the GPU is recent, update its graphics driver: Windows only lists the encoder once the
        vendor driver is installed.
      </div>
    );
  }
  return null;
}

/** The Store has two HEVC packages and the free one's product ID is not
 *  something we can verify from here, so link the search rather than ship a
 *  deep link that might open an error page. */
const HEVC_STORE_SEARCH = "ms-windows-store://search/?query=HEVC%20Video%20Extensions";

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
        <p className="note">{offline && !mock ? "Core offline — status unknown." : "Reading…"}</p>
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
        <p className="note">Without the virtual camera the share still plays in its own window — it just
          cannot be picked as a webcam.</p>
      )}
      {vd.obs_virtualcam && !vd.camera_registered && (
        <p className="note">OBS VirtualCam is installed on this PC, but Relay does not feed it.</p>
      )}
    </Card>
  );
}

/**
 * Receive mode. Advertises this PC over mDNS and renders an incoming share in
 * a native D3D11 window (opened by the share engine, not the webview). The
 * pairing code shown here is what the sender types on its Share screen.
 */
export function Receive() {
  const { mock } = useCore();
  const [receiving, setReceiving] = useState(false);
  const [code, setCode] = useState<string | null>(null);
  const [sender, setSender] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let unsub = () => {};
    void onCoreEvents({
      receiveStatus: (s) => {
        setReceiving(s.receiving);
        if (s.code) setCode(s.code);
        if (s.sender) setSender(s.sender);
        if (s.message) setError(s.message);
        if (!s.receiving) { setSender(null); setCode(null); }
      },
    }).then((u) => { unsub = u; });
    return () => unsub();
  }, []);

  const start = async () => {
    setBusy(true); setError(null);
    try { await api.startReceive({}); setReceiving(true); }
    catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };
  const stop = async () => {
    setBusy(true);
    try { await api.stopReceive(); } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Receive <em>— from another PC</em></h1>
          <Live on={receiving} text={receiving ? "Ready" : "Not receiving"} />
        </div>
        <OfflineBanner />
        <CodecBanner need="receive" />
        <div className="preview">
          <div className={"scene" + (sender ? "" : " idle")} />
          {sender
            ? <div className="cap">Playing in a separate window · {sender}</div>
            : <div className="idlemsg">
                {receiving
                  ? "Waiting for a sender to pair…"
                  : "Press Start receiving, then enter the code on the sending PC."}
              </div>}
        </div>
      </section>
      <aside className="side">
        <Card title="Your pairing code">
          <div className="code" style={{ fontSize: 34, letterSpacing: 6, fontVariantNumeric: "tabular-nums" }}>
            {code ?? "— — — — — —"}
          </div>
          <p className="note">Type this on the other PC's Share screen.</p>
        </Card>
        <Card>
          <Kv k="Status" v={sender ? `Paired with ${sender}` : receiving ? "Advertising on the LAN" : "Idle"} />
          <Kv k="Codec" v={receiving ? "HEVC" : "—"} mono />
        </Card>
        <VirtualDeviceCard />
        <ErrorNote text={error} onDismiss={() => setError(null)} />
        {receiving
          ? <button className="btn acc" onClick={stop} disabled={busy}>Stop receiving</button>
          : <button className="btn acc" onClick={start} disabled={busy}>Start receiving</button>}
        {mock && <p className="note">Preview only — the core service isn't running.</p>}
        <p className="note">The stream appears as a normal window. Nothing on this PC is changed.</p>
      </aside>
    </>
  );
}
