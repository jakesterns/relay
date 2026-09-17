import { useEffect, useState } from "react";
import { Card, ErrorNote, Kv, Live } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { api, onCoreEvents, type FirewallStatus, type ShareCapabilities, type VdeviceStatus } from "../lib/ipc";

/** Warn before the user tries, not after it fails.
 *
 *  Hardware HEVC *decode* on Windows goes through the Microsoft HEVC Video
 *  Extension; GPU vendors register encode MFTs only. Without it `recv` dies on
 *  the first frame, which looks like a network problem and is not one. */
export function CodecBanner({ need }: { need: "share" | "receive" }) {
  const { offline } = useCore();
  const [caps, setCaps] = useState<ShareCapabilities | null>(null);

  // Re-probe when the window regains focus. Installing the codec happens in
  // the Store, i.e. in another window, so the user comes back expecting Relay
  // to have noticed. Telling them to restart the app for something Windows
  // already knows is the kind of small indignity that reads as broken.
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
    // `.offline` is a flex row, so every text node and the <a> would each
    // become a column — on a real machine the link rendered one word per
    // line. `.msg` keeps it a single inline flow.
    return (
      <div className="offline"><i />
        <span className="msg">
          This PC cannot decode HEVC video, so Relay cannot show the shared screen.
          Windows includes the decoder for free only on PCs whose manufacturer licensed it;
          otherwise Microsoft sells it in the Store as{" "}
          <a href={HEVC_STORE_PAID}>HEVC Video Extensions</a>. Relay cannot bundle the decoder —
          Microsoft does not license it for redistribution by apps. This banner clears
          itself once a decoder is present.
        </span>
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
        <span className="msg">
          No hardware HEVC encoder on {gpu}. Relay encodes in hardware only (NVENC / Quick Sync /
          AMF) — there is no software encode path, so this PC can receive a share but not send one.
          If the GPU is recent, update its graphics driver: Windows only lists the encoder once the
          vendor driver is installed.
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

/** Deep link to the *paid* HEVC extension, deliberately.
 *
 *  There are two Microsoft packages. The free one (9N4WGH0Z6VHQ,
 *  `Microsoft.HEVCVideoExtension`, singular) is OEM-entitlement only: its
 *  catalog entry has no Purchase action, and on a PC whose manufacturer did
 *  not license it the Store shows the page with Install greyed out — verified
 *  on a real Windows 10 machine 2026-09-16, not inferred. The paid one
 *  (9NMZLZ57R3T7, `Microsoft.HEVCVideoExtensions`, plural) has a Purchase
 *  action and is installable by anyone.
 *
 *  This used to link to a Store *search*, which is worse than useless: the
 *  free package does not appear in search results at all, so the user was
 *  shown CapCut and third-party "HEVC Player" apps instead. Never link the
 *  search.
 *
 *  Price is not hardcoded — it varies by market (0.99 USD, 0.79 GBP) and the
 *  Store page states it. */
const HEVC_STORE_PAID = "ms-windows-store://pdp/?ProductId=9NMZLZ57R3T7";

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
        <FirewallBanner />
        <div className="preview">
          {/* The share plays in the engine's own window, never here, so this
              frame stays empty rather than painting a stand-in for it. */}
          <div className="scene" />
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
          <Kv k="Status" v={sender ? `Paired with ${sender}` : receiving ? "Advertising on the LAN" : "Idle"} />
          <Kv k="Codec" v={receiving ? "HEVC" : "—"} mono />
        </Card>
        <VirtualDeviceCard />
        <ErrorNote text={error} onDismiss={() => setError(null)} />
        {receiving
          ? <button className="btn acc" onClick={stop} disabled={busy}>Stop receiving</button>
          : <button className="btn acc" onClick={start} disabled={busy}>Start receiving</button>}
        {mock && <p className="note">Preview only — Relay isn't running.</p>}
        <p className="note">The stream appears as a normal window. Nothing on this PC is changed.</p>
      </aside>
    </>
  );
}
