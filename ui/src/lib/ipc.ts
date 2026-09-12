/**
 * TypeScript mirror of `crates/core/src/ipc.rs` + `types.rs`.
 * Keep field names and enum spellings identical to the serde output.
 *
 * In the browser (plain `pnpm dev` without Tauri) the commands are not
 * available, so every call falls back to an in-memory mock store shaped like
 * the mocks/ so the forms can still be exercised.
 */

export type HeadsetId = string;
export type MonitorId = string;

export interface GameMatch { exe: string; title_contains?: string }
export interface EqBand { freq_hz: number; gain_db: number; q: number }
export interface Limiter { below_hz: number; threshold_db: number }
export interface AudioSettings { bands: EqBand[]; hrtf: boolean; limiter?: Limiter; apply_to_share: boolean }
export interface GpuColor { vibrance: number; gamma: number; contrast: number; shadow_lift: number; hue_deg: number }
export interface MonitorSettings { brightness?: number; contrast?: number; black_equalizer?: number; response?: string; sharpness?: number }
export interface DisplaySettings { gpu: GpuColor; monitor: MonitorSettings; follow_focus: boolean; leave_other_monitors: boolean; share_true_colors: boolean }
export type SharePreset = "game" | "daw" | "desktop" | "off";
export type ProfileStatus = "draft" | "ready";

export interface Profile {
  id: string; name: string; note: string; game: GameMatch;
  headset?: HeadsetId; monitor?: MonitorId;
  audio: AudioSettings; display: DisplaySettings; share: SharePreset; status: ProfileStatus;
}
export interface ProfileSummary {
  id: string; name: string; note: string; exe: string;
  headset: HeadsetId | null; monitor: MonitorId | null; share: SharePreset; status: ProfileStatus;
}
/** Mirror of `crates/core/src/hardware/mod.rs`. */
export type HeadsetKind = "headphone" | "iem" | "speakers";
export interface Headset {
  id: HeadsetId; name: string; kind: HeadsetKind;
  curve?: [number, number][]; source: string; endpoints: string[];
}
export interface HardwareMonitor {
  id: MonitorId; name: string; panel: string; ddcci?: number[];
}
export interface AudioInterface { id: string; name: string }
export interface EndpointInfo { key: string; name: string; default: boolean }
export interface MonitorProbe {
  id: MonitorId; name: string; native?: [number, number]; refresh_hz?: number;
  primary: boolean; hmonitor: number; gdi_name: string; ddc?: number[];
}
export interface ProbeReport { endpoints: EndpointInfo[]; monitors: MonitorProbe[] }
export interface HardwareView { endpoints: EndpointInfo[]; monitors: MonitorProbe[]; headset: HeadsetId | null }
export interface HardwareReply {
  headsets: Headset[]; monitors: HardwareMonitor[]; interfaces: AudioInterface[]; connected: HardwareView;
}
export type HardwareItem =
  | { kind: "headset"; value: Headset }
  | { kind: "monitor"; value: HardwareMonitor };

export interface Foreground { pid: number; exe: string; title: string; hmonitor: number }
export interface ProcessInfo { pid: number; exe: string; title: string }
export type ShareState = { kind: "off" } | { kind: "sharing"; peer: string };
export type AudioChainState = "bypass" | "active" | "exclusivebypassed";
export type DisplayState = "default" | "applied";
/** Which paths carried the current display apply (`types::DisplayVia`). */
export interface DisplayVia { nvapi: boolean; gamma: boolean; ddcci: boolean; unsupported?: string[] }
export interface Footprint { rss_bytes: number; cpu_percent: number }
export interface CoreState {
  active_profile: ProfileSummary | null; foreground: Foreground | null;
  sharing: ShareState; audio_chain: AudioChainState; display_state: DisplayState;
  display_via: DisplayVia; footprint: Footprint;
  hardware: HardwareView;
}

export interface ShareRequest {
  peer?: string | null; code: string; bitrate_mbps: number; fps: number;
  audio: boolean; audio_pid?: number; cursor: boolean;
}
export interface ReceiveRequest { name?: string | null; code?: string }
export interface DiscoveredReceiver { name: string; addr: string; port: number }
/** One `stats` NDJSON line from the share/receive engine (loose shape). */
export interface ShareStats {
  event: string;
  bitrate_mbps?: number; fps?: number; frames?: number; keyframes?: number;
  dropped?: number; encode_ms?: number; capture_to_send_ms?: number;
  capture_to_present_ms?: number; audio_packets?: number; audio_peak?: number;
  cpu_percent?: number; rss_mb?: number;
}
/** A/B listening-test render (`Method::RenderPreview`). Paths are absolute. */
export interface Preview { original: string; processed: string; sample_rate: number; hrtf_applied: boolean }

/** Mirror of relay-core's `audio_apo::ApoStatus`. */
export interface ApoStatus { installed: boolean; endpoint: string | null; running: boolean }
/** Mirror of relay-vdevice's `installed::Consent`. */
export interface VdeviceConsent { decided_at: string; apo: boolean; camera: boolean; microphone: boolean }
export type MicTargetKind = "vb_cable" | "voice_meeter";
export interface MicTarget { endpoint_id: string; name: string; kind: MicTargetKind }
/** Mirror of relay-core's `vdevice::VdeviceStatus`. */
export interface VdeviceStatus {
  windows_build: number | null;
  camera_supported: boolean;
  camera_registered: boolean;
  obs_virtualcam: string | null;
  mic_targets: MicTarget[];
  consent: VdeviceConsent | null;
  elevated: boolean;
}
export interface ShareStatus { sharing: boolean; peer?: string | null; message?: string | null }
export interface ReceiveStatus { receiving: boolean; code?: string | null; sender?: string | null; message?: string | null }

export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

/** A fresh profile with the same defaults as `Profile::new` in Rust. */
export function newProfile(name = "", exe = ""): Profile {
  return {
    id: crypto.randomUUID(),
    name,
    note: "",
    game: { exe },
    audio: { bands: [], hrtf: false, apply_to_share: false },
    display: {
      gpu: { vibrance: 50, gamma: 1.0, contrast: 0, shadow_lift: 0, hue_deg: 0 },
      monitor: {},
      follow_focus: true,
      leave_other_monitors: true,
      share_true_colors: true,
    },
    share: "off",
    status: "draft",
  };
}

export function summarize(p: Profile): ProfileSummary {
  return {
    id: p.id, name: p.name, note: p.note, exe: p.game.exe,
    headset: p.headset ?? null, monitor: p.monitor ?? null, share: p.share, status: p.status,
  };
}

/** Browser-only mock hardware, shaped like the dev PC's real probe. */
export const mockHardware: HardwareReply = {
  headsets: [
    { id: "hd560s", name: "HD 560S", kind: "headphone", source: "oratory1990", endpoints: ["ep:c:31f634a2-usb-dac"], curve: [[20, -4.11], [1000, 0], [20000, -6.2]] },
    { id: "blessing3", name: "Moondrop Blessing 3", kind: "iem", source: "crinacle", endpoints: ["ep:c:77c0f2b1-dongle"] },
  ],
  monitors: [
    { id: "mon:GSM5C7C:402NTCZ9E219", name: "LG ULTRAGEAR+", panel: "Nano IPS", ddcci: [0x10, 0x12, 0x60] },
    { id: "mon:GSM7654:311NDX55X942", name: "LG C2", panel: "OLED" },
  ],
  interfaces: [],
  connected: {
    endpoints: [
      { key: "ep:c:31f634a2-usb-dac", name: "USB Audio 2.0", default: true },
      { key: "ep:c:9a11c3d0-hdmi", name: "LG ULTRAGEAR+ (NVIDIA HDA)", default: false },
    ],
    monitors: [
      { id: "mon:GSM5C7C:402NTCZ9E219", name: "LG ULTRAGEAR+", native: [3840, 2160], refresh_hz: 144, primary: true, hmonitor: 65537, gdi_name: "\\\\.\\DISPLAY1" },
    ],
    headset: "hd560s",
  },
};

export const mockState: CoreState = {
  active_profile: null,
  foreground: null,
  sharing: { kind: "off" },
  audio_chain: "bypass",
  display_state: "default",
  display_via: { nvapi: false, gamma: false, ddcci: false },
  footprint: { rss_bytes: 9 * 1024 * 1024, cpu_percent: 0 },
  hardware: mockHardware.connected,
};

function mockProfile(id: string, name: string, note: string, exe: string, headset: string | null, monitor: string | null, share: SharePreset, status: ProfileStatus): Profile {
  const p = newProfile(name, exe);
  p.id = id; p.note = note; p.share = share; p.status = status;
  if (headset) p.headset = headset;
  if (monitor) p.monitor = monitor;
  return p;
}

/** Browser-only store so New / Edit / Delete work without the core. */
const mockStore: Map<string, Profile> = new Map(
  [
    mockProfile("1", "Call of Duty", "Footsteps · dark-map colors", "cod.exe", "HD 560S", "LG 27GP850", "game", "ready"),
    mockProfile("2", "Call of Duty", "Same tuning · IEM set", "cod.exe", "Moondrop Blessing 3", "LG 27GP850", "game", "ready"),
    mockProfile("3", "Valorant", "Neutral EQ · vivid", "valorant.exe", "HD 560S", "Zowie XL2566K", "game", "ready"),
    mockProfile("4", "FL Studio", "Relay Send on master", "fl64.exe", "Scarlett 2i2 → DT 770", null, "daw", "ready"),
    mockProfile("5", "Elden Ring", "Untouched audio · warm colors", "eldenring.exe", null, "LG C2", "off", "draft"),
  ].map((p) => [p.id, p]),
);

export const mockProfiles: ProfileSummary[] = [...mockStore.values()].map(summarize);

const mockProcesses: ProcessInfo[] = [
  { pid: 1001, exe: "cod.exe", title: "Call of Duty" },
  { pid: 1002, exe: "valorant.exe", title: "VALORANT" },
  { pid: 1003, exe: "fl64.exe", title: "FL Studio 21" },
  { pid: 1004, exe: "discord.exe", title: "Discord" },
];

let mockAutostart = false;

/** Browser-only virtual-device state: fresh machine, consent not decided. */
const mockVdevice: VdeviceStatus = {
  windows_build: 26200,
  camera_supported: true,
  camera_registered: false,
  obs_virtualcam: null,
  mic_targets: [
    { endpoint_id: "{0.0.0.00000000}.{mock-cable}", name: "CABLE Input (VB-Audio Virtual Cable)", kind: "vb_cable" },
  ],
  consent: null,
  elevated: false,
};

export const api = {
  async status(): Promise<CoreState> {
    if (!isTauri()) return mockState;
    return invoke<CoreState>("core_status");
  },
  async listProfiles(): Promise<ProfileSummary[]> {
    if (!isTauri()) return [...mockStore.values()].map(summarize);
    return invoke<ProfileSummary[]>("list_profiles");
  },
  async getProfile(id: string): Promise<Profile> {
    if (!isTauri()) {
      const p = mockStore.get(id);
      if (!p) throw new Error("no such profile");
      return structuredClone(p);
    }
    return invoke<Profile>("get_profile", { id });
  },
  async saveProfile(profile: Profile): Promise<void> {
    if (!isTauri()) { mockStore.set(profile.id, structuredClone(profile)); return; }
    return invoke<void>("save_profile", { profile });
  },
  async deleteProfile(id: string): Promise<void> {
    if (!isTauri()) { mockStore.delete(id); return; }
    return invoke<void>("delete_profile", { id });
  },
  async applyProfile(id: string): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("apply_profile", { id });
  },
  async restoreAll(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("restore_all");
  },
  async listProcesses(): Promise<ProcessInfo[]> {
    if (!isTauri()) return mockProcesses;
    return invoke<ProcessInfo[]>("list_processes");
  },
  async getAutostart(): Promise<boolean> {
    if (!isTauri()) return mockAutostart;
    return invoke<boolean>("get_autostart");
  },
  async setAutostart(enabled: boolean): Promise<boolean> {
    if (!isTauri()) { mockAutostart = enabled; return enabled; }
    return invoke<boolean>("set_autostart", { enabled });
  },
  async startShare(request: ShareRequest): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("start_share", { request });
  },
  async stopShare(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("stop_share");
  },
  async startReceive(request: ReceiveRequest): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("start_receive", { request });
  },
  async stopReceive(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("stop_receive");
  },
  async listHardware(): Promise<HardwareReply> {
    if (!isTauri()) return structuredClone(mockHardware);
    return invoke<HardwareReply>("list_hardware");
  },
  async saveHardware(item: HardwareItem): Promise<void> {
    if (!isTauri()) {
      if (item.kind === "headset") {
        const i = mockHardware.headsets.findIndex((h) => h.id === item.value.id);
        if (i >= 0) mockHardware.headsets[i] = structuredClone(item.value);
        else mockHardware.headsets.push(structuredClone(item.value));
      } else {
        const i = mockHardware.monitors.findIndex((m) => m.id === item.value.id);
        if (i >= 0) mockHardware.monitors[i] = structuredClone(item.value);
        else mockHardware.monitors.push(structuredClone(item.value));
      }
      return;
    }
    return invoke<void>("save_hardware", { item });
  },
  async deleteHardware(id: string): Promise<void> {
    if (!isTauri()) {
      mockHardware.headsets = mockHardware.headsets.filter((h) => h.id !== id);
      mockHardware.monitors = mockHardware.monitors.filter((m) => m.id !== id);
      return;
    }
    return invoke<void>("delete_hardware", { id });
  },
  async probeHardware(): Promise<ProbeReport> {
    if (!isTauri()) return structuredClone(mockHardware.connected);
    return invoke<ProbeReport>("probe_hardware");
  },
  /** Parse AutoEQ text and attach it to a headset. Returns the parsed points. */
  async importCurve(headset: HeadsetId, csv: string): Promise<[number, number][]> {
    if (!isTauri()) {
      const h = mockHardware.headsets.find((x) => x.id === headset);
      if (!h) throw new Error("no such headset");
      h.curve = [[20, 0], [20000, 0]];
      return h.curve;
    }
    return invoke<[number, number][]>("import_curve", { headset, csv });
  },
  /** Render the A/B pair for a profile; `wav` optional (demo clip without). */
  async renderPreview(id: string, wav?: string): Promise<Preview> {
    if (!isTauri()) throw new Error("A/B rendering needs the Relay core");
    return invoke<Preview>("render_preview", { id, wav: wav ?? null });
  },
  /** Read-only probe: is the Relay APO on the default render endpoint? */
  async apoStatus(): Promise<ApoStatus> {
    if (!isTauri()) return { installed: false, endpoint: null, running: false };
    return invoke<ApoStatus>("apo_status");
  },
  /** Register the APO (backup-then-apply). Gated in the core; errors explain. */
  async installApo(): Promise<void> {
    if (!isTauri()) throw new Error("Installing the APO needs the Relay core");
    return invoke<void>("install_apo");
  },
  /** Restore the endpoint's FX chain from the install backup and unregister. */
  async uninstallApo(): Promise<void> {
    if (!isTauri()) throw new Error("Removing the APO needs the Relay core");
    return invoke<void>("uninstall_apo");
  },
  /** Read-only: Windows support, registration, consent, OBS / VB-Cable. */
  async vdeviceStatus(): Promise<VdeviceStatus> {
    if (!isTauri()) return structuredClone(mockVdevice);
    return invoke<VdeviceStatus>("vdevice_status");
  },
  /** Record the first-run decision. Installs nothing by itself. */
  async setVdeviceConsent(apo: boolean, camera: boolean, microphone: boolean): Promise<void> {
    if (!isTauri()) {
      mockVdevice.consent = { decided_at: new Date().toISOString(), apo, camera, microphone };
      return;
    }
    return invoke<void>("set_vdevice_consent", { apo, camera, microphone });
  },
  /** Exactly what a camera install would create (registry keys + the DLL). */
  async vdeviceDryRun(): Promise<string[]> {
    if (!isTauri()) return [
      "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}",
      "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}\\InprocServer32",
      "file: <install dir>\\relay_vdevice.dll (stays in place; only registered)",
    ];
    return invoke<string[]>("vdevice_dry_run");
  },
  /** Register the camera media source. Gated in the core; errors explain. */
  async installVcam(): Promise<void> {
    if (!isTauri()) { mockVdevice.camera_registered = true; return; }
    return invoke<void>("install_vcam");
  },
  /** Remove the recorded registration; empties installed.json. */
  async uninstallVcam(): Promise<void> {
    if (!isTauri()) { mockVdevice.camera_registered = false; return; }
    return invoke<void>("uninstall_vcam");
  },
  async discoverReceivers(): Promise<DiscoveredReceiver[]> {
    if (!isTauri()) return [{ name: "living-room-pc", addr: "192.168.1.42", port: 0 }];
    return invoke<DiscoveredReceiver[]>("discover_receivers");
  },
};

/** Subscribe to pushed core events. Returns an unsubscribe fn. */
export async function onCoreEvents(handlers: {
  state?: (s: CoreState) => void;
  notice?: (text: string) => void;
  offline?: () => void;
  shareStats?: (s: ShareStats) => void;
  shareStatus?: (s: ShareStatus) => void;
  receiveStatus?: (s: ReceiveStatus) => void;
}): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const unlisteners = await Promise.all([
    listen<CoreState>("core://state", (e) => handlers.state?.(e.payload)),
    listen<string>("core://notice", (e) => handlers.notice?.(e.payload)),
    listen<void>("core://offline", () => handlers.offline?.()),
    listen<ShareStats>("core://share-stats", (e) => handlers.shareStats?.(e.payload)),
    listen<ShareStatus>("core://share-status", (e) => handlers.shareStatus?.(e.payload)),
    listen<ReceiveStatus>("core://receive-status", (e) => handlers.receiveStatus?.(e.payload)),
  ]);
  return () => unlisteners.forEach((u) => u());
}

export const fmtMb = (bytes: number) => (bytes / (1024 * 1024)).toFixed(0);
