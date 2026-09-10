import { useEffect, useState } from "react";
import { Card, Kv, Live } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, onCoreEvents } from "../lib/ipc";

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
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const stop = async () => {
    setBusy(true);
    try { await api.stopReceive(); } catch (e) { setError(String(e)); }
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
          <Kv k="Decode" v={receiving ? "Hardware (DXVA HEVC)" : "—"} mono />
          <Kv k="Window" v={sender ? "Native D3D11 swapchain" : "—"} mono />
        </Card>
        {error && <p className="note" style={{ color: "#d98b6a" }}>{error}</p>}
        {receiving
          ? <button className="btn acc" onClick={stop} disabled={busy}>Stop receiving</button>
          : <button className="btn acc" onClick={start} disabled={busy}>Start receiving</button>}
        {mock && <p className="note">Preview only — the core service isn't running.</p>}
        <p className="note">The stream appears as a normal window. Nothing on this PC is changed.</p>
      </aside>
    </>
  );
}
