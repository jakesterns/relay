import { Card, Kv } from "./Controls";

/** The stream window's title as call apps and OBS list it. Mirror of
 *  `relay_capture::render::placement::window_title`, pinned by tests on both
 *  sides: control characters dropped, long names cut at 48 characters. */
export function streamWindowTitle(sender: string | null | undefined): string {
  const name = (sender ?? "").replace(/[\u0000-\u001f\u007f-\u009f]/g, "").trim();
  if (!name) return "Relay — receiving";
  const chars = Array.from(name);
  const shown = chars.length > 48 ? chars.slice(0, 48).join("").trimEnd() + "…" : name;
  return `Relay — from ${shown}`;
}

/** S50 "share and go": how to get the received stream into a call or OBS on
 *  this PC, on one card. Two ways, and which is which: pick the stream
 *  window (full quality, and the sound where the app shares a window's
 *  sound), or pick Relay Camera as the webcam (picture only). Plain words,
 *  one line per app. */
export function CallGuide({ sender }: { sender: string | null }) {
  const title = streamWindowTitle(sender);
  return (
    <Card title="Use with Discord / Zoom / Teams / Meet / OBS">
      <p className="note" data-testid="guide-window">
        Press <b>Share to a call</b>, then pick the window <b>“{title}”</b> in the app's screen
        share. That gives the full picture, plus the sound where the app shares a window's sound.
      </p>
      <Kv k="Discord" v="Go Live → the window. Sound comes with it." />
      <Kv k="Zoom" v="Share Screen → the window, tick Share sound." />
      <Kv k="Teams" v="Share → Window, turn on Include sound." />
      <Kv k="Meet" v="Present → A window. Picture only." />
      <Kv k="OBS" v="Window Capture → the window; Application Audio Capture for sound." />
      <p className="note" data-testid="guide-camera">
        Or pick <b>Relay Camera</b> as the webcam. That is the picture only, no sound.
      </p>
    </Card>
  );
}
