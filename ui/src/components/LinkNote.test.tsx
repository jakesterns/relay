import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { LinkCell, LinkNote, linkView } from "./LinkNote";
import type { LinkInfo, ShareStats } from "../lib/ipc";

const wired: LinkInfo = { kind: "wired", label: "Wired (1 Gb/s)", wifi: false, band: null, mbps: 1000 };
const wifi: LinkInfo = { kind: "wi_fi", label: "Wi-Fi (5 GHz, 866 Mb/s)", wifi: true, band: "5", mbps: 866 };

const line = (over: Partial<ShareStats>): ShareStats => ({ event: "stats", ...over });

describe("S49 link note", () => {
  it("shows nothing at all on a wired share", () => {
    expect(linkView(line({ link: wired, peer_link: wired }))).toBeNull();
    expect(linkView(line({ link: wired }))).toBeNull();
    expect(linkView(null)).toBeNull();
    const { container } = render(<><LinkCell s={line({ link: wired, peer_link: wired })} live />
      <LinkNote s={line({ link: wired, peer_link: wired })} live /></>);
    expect(container).toBeEmptyDOMElement();
  });

  it("names the link on the strip when this PC is on Wi-Fi", () => {
    render(<LinkCell s={line({ link: wifi, peer_link: wired })} live />);
    expect(screen.getByTestId("link-cell")).toHaveTextContent("Wi-Fi (5 GHz, 866 Mb/s)");
    expect(screen.getByTestId("link-cell")).toHaveTextContent("Other PC: Wired (1 Gb/s)");
  });

  it("recommends Ethernet or 6E and says what Relay is doing, calmly", () => {
    const s = line({
      link: wired,
      peer_link: wifi,
      adapt: {
        rung: "1440p60", top: "2160p60", width: 2560, height: 1440, fps: 60,
        target_mbps: 18, cause: "queue", note: "Lowered to 1440p60 to stay smooth",
      },
    });
    render(<LinkNote s={s} live />);
    const note = screen.getByTestId("link-note").textContent ?? "";
    expect(note).toContain("The other PC is on Wi-Fi");
    expect(note).toContain("Ethernet or Wi-Fi 6E");
    expect(note).toContain("Lowered to 1440p60 to stay smooth.");
    // Never blames the user.
    for (const w of ["your ", "you ", "fault", "poor", "bad"]) expect(note.toLowerCase()).not.toContain(w);
  });

  it("falls back to a general line when nothing has been adjusted", () => {
    render(<LinkNote s={line({ link: wifi, peer_link: wifi })} live />);
    expect(screen.getByTestId("link-note")).toHaveTextContent(
      "Both PCs are on Wi-Fi. For steady 4K60, Ethernet or Wi-Fi 6E holds up best. Relay adjusts quality to keep the picture smooth.",
    );
  });

  it("an older peer that reports no link is said so, not guessed", () => {
    expect(linkView(line({ link: wifi }))?.hint).toBe("Other PC: not reported");
  });

  it("is gone when the share is not live", () => {
    const { container } = render(<LinkNote s={line({ link: wifi })} live={false} />);
    expect(container).toBeEmptyDOMElement();
  });
});
