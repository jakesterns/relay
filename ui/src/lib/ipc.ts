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
export interface AudioSettings {
  bands: EqBand[]; hrtf: boolean; limiter?: Limiter; apply_to_share: boolean;
  /** Apply the headset's imported correction curve ahead of `bands`. */
  headset_correction: boolean;
}
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
/** What a panel reports about its own colour, read from its EDID. Mirrors
 *  `hardware::edid_color::ColorInfo`. */
export interface Chromaticity { red: [number, number]; green: [number, number]; blue: [number, number]; white: [number, number] }
export interface Coverage { srgb: number; dci_p3: number; bt2020: number }
export interface Colorimetry {
  bt2020_rgb: boolean; bt2020_ycc: boolean; bt2020_cycc: boolean;
  adobe_rgb: boolean; adobe_ycc: boolean; s_ycc601: boolean;
  xv_ycc709: boolean; xv_ycc601: boolean;
}
export interface Hdr {
  hdr10: boolean; hlg: boolean; hdr_gamma: boolean;
  dolby_vision: boolean; hdr10_plus: boolean;
  max_nits?: number; max_frame_avg_nits?: number; min_nits?: number;
}
export interface ColorInfo {
  chromaticity?: Chromaticity; gamma?: number; bit_depth?: number;
  digital: boolean; colorimetry: Colorimetry; hdr: Hdr; coverage?: Coverage;
}
export interface HardwareMonitor {
  id: MonitorId; name: string; panel: string; ddcci?: number[]; color?: ColorInfo;
}
/** One model in the bundled headphone catalogue. Mirrors
 *  `hardware::catalog::CatalogEntry`. */
export interface CatalogEntry { name: string; source: string; rig: string; path: string }
/** A base64 JPEG thumbnail of the live capture (Event::SharePreview). */
export interface SharePreview { width: number; height: number; jpeg: string }
export interface AudioInterface { id: string; name: string }
export interface EndpointInfo { key: string; name: string; default: boolean }
export interface MonitorProbe {
  id: MonitorId; name: string; native?: [number, number]; refresh_hz?: number;
  primary: boolean; hmonitor: number; gdi_name: string; ddc?: number[]; color?: ColorInfo;
}
export interface ProbeReport { endpoints: EndpointInfo[]; monitors: MonitorProbe[] }
export interface HardwareView { endpoints: EndpointInfo[]; monitors: MonitorProbe[]; headset: HeadsetId | null }
/** Vendor-private DDC/CI controls the core has a *verified* opcode for on one
 *  monitor. Mirrors `hardware::MonitorVendorControls`. The core decides: the
 *  quirks table and the evidence behind each opcode live in Rust, so the UI
 *  must never infer a vendor control from the advertised opcode list. A
 *  missing entry means every vendor slider for that panel stays disabled. */
export interface MonitorVendorControls {
  monitor: MonitorId; black_equalizer: boolean; response: string[];
}
export interface HardwareReply {
  headsets: Headset[]; monitors: HardwareMonitor[]; interfaces: AudioInterface[]; connected: HardwareView;
  vendor_controls?: MonitorVendorControls[];
}
export type HardwareItem =
  | { kind: "headset"; value: Headset }
  | { kind: "monitor"; value: HardwareMonitor };

export interface Foreground { pid: number; exe: string; title: string; hmonitor: number }
export interface ProcessInfo { pid: number; exe: string; title: string; hwnd: number }
export type ShareState =
  | { kind: "off" }
  | { kind: "sharing"; peer: string }
  /** The share dropped and Relay is bringing it back (S38). */
  | { kind: "reconnecting"; peer: string; attempt: number };
export type AudioChainState = "bypass" | "active" | "exclusivebypassed";
export type DisplayState = "default" | "applied";
/** Which paths carried the current display apply (`types::DisplayVia`). */
export interface DisplayVia { nvapi: boolean; amd: boolean; gamma: boolean; ddcci: boolean; unsupported?: string[] }
export interface Footprint { rss_bytes: number; cpu_percent: number }
export interface CoreState {
  active_profile: ProfileSummary | null; foreground: Foreground | null;
  sharing: ShareState; audio_chain: AudioChainState; display_state: DisplayState;
  display_via: DisplayVia; footprint: Footprint;
  hardware: HardwareView;
  build?: BuildInfo;
  /** One sentence about a crash not yet shown to the user (S38). */
  last_crash?: string | null;
}
/** Version and the paths this core actually uses (`--data-dir` moves them). */
export interface BuildInfo { version: string; data_dir: string; log_file: string }

export interface ShareRequest {
  peer?: string | null; code: string;
  /** A remembered PC to connect to without a code (S35). */
  peer_id?: string | null;
  bitrate_mbps: number; fps: number;
  size?: [number, number];
  audio: boolean; audio_pid?: number; mic?: boolean; cursor: boolean;
  preset?: string; record?: boolean; replay_secs?: number; record_dir?: string;
  container?: RecordingContainer;
  /** Thumbnails per second for the in-app preview; 0 = off. */
  preview_fps?: number;
}
/** Mirror of `crates/core/src/share.rs` `SourceTarget` (serde-tagged on `kind`). */
export type SourceTarget =
  | { kind: "display"; index: number }
  | { kind: "window"; hwnd: number }
  | { kind: "region"; display: number; x: number; y: number; w: number; h: number };
/** Mirror of `crates/core/src/presets.rs`. */
export type DesktopAudio = "system" | "game" | "off";
/**
 * The share's audio *source set*. The sender carries two Opus tracks, so the
 * microphone is independent of the desktop source rather than one of four
 * exclusive choices. See `docs/dev/dual-audio-decision.md`.
 *
 * The core still reads the pre-S2 four-way string from disk, but everything
 * it hands out and takes back on the wire is this object.
 */
export interface PresetAudio {
  desktop: DesktopAudio; mic: boolean;
  /** With `game`: everything else on the PC as its own track too (S37). */
  rest?: boolean;
}

/** Human summary of an audio source set, e.g. "Game only + microphone". */
export function presetAudioLabel(a: PresetAudio): string {
  let desktop = { system: "System mix", game: "Game only", off: "" }[a.desktop];
  if (a.desktop === "game" && a.rest) desktop = "Game + everything else";
  if (desktop && a.mic) return `${desktop} + microphone`;
  if (desktop) return desktop;
  return a.mic ? "Microphone" : "None";
}

/** Mixer (S37). Mirrors `crates/core/src/share.rs`. */
export type MixerSide = "send" | "receive";
/** `call` is the audio coming back from the receiving PC (S19); sender only. */
export type MixerTrack = "app" | "rest" | "mic" | "call";
/** `gain` is linear, 0–2 (unity 1). */
export interface FaderLevel { gain: number; mute: boolean }
export type FaderSet = Partial<Record<MixerTrack, FaderLevel>>;
/**
 * Recording container. Same video + Opus bitstream either way — the choice
 * never re-encodes. `mkv` survives a crash mid-file where `mp4` does not.
 */
export type RecordingContainer = "mp4" | "mkv";
export interface SharePresetDef {
  id: string; name: string; bitrate_mbps: number; fps: number;
  size?: [number, number]; audio: PresetAudio; cursor: boolean;
  record: boolean; replay_secs: number; container: RecordingContainer;
  /** Also show the share as "Relay Camera" on this PC while it runs (S36). */
  vcam?: boolean;
}
export interface RecordingSettings { dir?: string; cap_gb: number; free_floor_gb: number }
export interface PresetsReply { presets: SharePresetDef[]; recording: RecordingSettings }
export interface ReceiveRequest {
  name?: string | null; code?: string;
  /** Send this app's audio back to the sender (S19): the call app's PID.
   *  Absent or 0 = no return route. */
  return_pid?: number;
}
export interface DiscoveredReceiver { name: string; addr: string; port: number }
/** A PC this one has paired with (S35). Mirror of `crates/core/src/peers.rs`.
 *  `fingerprint` is the credential the core matches on; it is public (it is
 *  in every SDP), and the UI only ever shows the name. */
export interface Peer {
  id: string; name: string; fingerprint: string;
  first_paired_unix: number; last_seen_unix: number;
  /** From this PC's point of view: we sent to them, or they sent to us. */
  last_direction?: "sent_to" | "received_from" | null;
  favourite: boolean;
}
/** One `stats` NDJSON line from the share/receive engine (loose shape). */
export interface ShareStats {
  event: string;
  /** The codec this share negotiated. */
  codec?: VideoCodec;
  bitrate_mbps?: number; fps?: number; frames?: number; keyframes?: number;
  dropped?: number; encode_ms?: number; capture_to_send_ms?: number;
  capture_to_present_ms?: number; audio_packets?: number; audio_peak?: number;
  /** Second audio track (microphone); absent when only one track is sent. */
  mic_packets?: number; mic_peak?: number;
  /** Third track: everything on the PC except the shared app (S37). */
  rest_packets?: number; rest_peak?: number;
  /** Frames handed to "Relay Camera" on the sending PC (S36). */
  vcam_frames?: number;
  /** The call coming back (S19): on the sender, packets received and the
   *  peak played here; on the receiver, packets sent and the peak encoded. */
  return_packets?: number; return_peak?: number;
  cpu_percent?: number; rss_mb?: number;
  /** Present while the engine is recording-capable. */
  recording?: boolean; rec_mb?: number; rec_dropped?: number;
  replay_fill?: number; replays_saved?: number; rec_stopped_disk?: boolean;

  // --- Receiver-side stream health (S30, S33). Absent on the sender's line. ---
  /** Access units assembled, and frames actually put on screen. */
  aus?: number; presented?: number;
  /** Sequence gaps and packets still missing *after* NACK repair. */
  rtp_gaps?: number; rtp_lost?: number;
  /** Holes NACK filled in time. High with `rtp_lost` at 0 means the link is
   *  lossy and Relay is coping — which is not a problem to report. */
  rtp_recovered?: number;
  /** Keyframes asked for because a gap could not be repaired. */
  keyframe_requests?: number;
  /** Frames held back while waiting for that keyframe (capped at ~1 s). */
  frames_withheld?: number;
  audio?: AudioHealth;
}
/** The receiver's audio pipeline, from `playback.rs` (S33's B16 work). */
export interface AudioHealth {
  buffered_ms?: number; render_ms?: number; queue_ms?: number;
  channel_packets?: number; engine_ms?: number; render_buffer_ms?: number;
  underruns?: number; slew_skipped?: number; slew_repeated?: number;
  dropped_frames?: number;
}
export interface RecordingStatus { on: boolean; path: string | null }
export interface ReplaySaved { path: string; ms: number }
/** The `source_changed` event's `data` (engine `source` line, verbatim). */
export interface SourceChangedData {
  target?: SourceTarget; width?: number; height?: number;
}
/** A/B listening-test render (`Method::RenderPreview`). Paths are absolute. */
export interface Preview { original: string; processed: string; sample_rate: number; hrtf_applied: boolean }

/** A share's video codec, negotiated per share: HEVC when both ends can,
 *  H.264 otherwise (S27). Never a user setting. */
export type VideoCodec = "hevc" | "h264";
export const codecLabel = (c: VideoCodec | string): string => (c === "h264" ? "H.264" : c === "hevc" ? "HEVC" : c);

/** Video support on this PC (`Reply::Capabilities`). Sending needs a hardware
 *  encoder for either codec; receiving needs a decoder for either. H.264
 *  decode ships with Windows, so a PC without HEVC decode still receives. */
export interface ShareCapabilities {
  can_share: boolean; can_receive: boolean; adapters: string[]; encoders: string[]; decoders: string[];
  /** Codecs this PC can send / receive, preference order. Absent from a core
   *  that predates S27, which only knew HEVC. */
  share_codecs?: VideoCodec[]; receive_codecs?: VideoCodec[];
}
/** Mirror of relay-core's `firewall::Verdict`. `blocked` is the one this
 *  whole feature exists for: a Block rule written when somebody dismissed
 *  Windows' prompt, which looks exactly like a dead network and is not. */
export type FirewallState =
  | "allowed" | "blocked" | "will_prompt" | "permissive" | "public_network" | "firewall_off";
/** Mirror of relay-core's `firewall::Policy`. */
export interface FirewallPolicy {
  active_profiles: number; enabled: boolean; default_inbound_block: boolean;
}
/** Mirror of relay-core's `firewall::FirewallStatus` (`Reply::Firewall`). */
export interface FirewallStatus {
  state: FirewallState;
  /** The relay-share.exe the verdict is about. */
  program: string;
  rule_present: boolean;
  /** Block rules matching this exact binary on a connected profile. */
  blocking_rules: number;
  /** Rules for a relay-share.exe somewhere else (another worktree, an old
   *  install). Harmless; shown only on a dev machine. */
  stale_rules: number;
  policy: FirewallPolicy;
  /** Firewall state could not be read; every field above is a conservative
   *  default and must not be reported as fact. */
  unknown: boolean;
}

/** Mirror of relay-core's `elevate::ElevatedOp` — the complete set of things
 *  the elevated helper will do. There is no free-form variant: this is the
 *  allow-list, and the core refuses anything else. */
export type ElevatedOp =
  | "install_apo" | "uninstall_apo" | "install_camera" | "uninstall_camera"
  | "allow_firewall" | "remove_firewall";

/** Result of one `runElevated`. `declined` means the user dismissed the UAC
 *  prompt, which is a normal answer: nothing was attempted. */
export interface ElevationResult { declined: boolean; ok: boolean; lines: string[] }

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
/** Mirror of `crates/core/src/uiprefs.rs`. */
export type CloseAction = "keep_running" | "quit_relay";
export interface UiPrefs {
  close_action: CloseAction;
  /** Bring a share back on its own after a crash, drop or reboot (S38). */
  resilience: boolean;
  /** Say in the notification area that Relay kept running on close (S38). */
  close_notice: boolean;
}

export interface ShareStatus {
  sharing: boolean; peer?: string | null; message?: string | null;
  /** Connected without a code, as a remembered PC (S35). */
  trusted?: boolean;
}
export interface ReceiveStatus {
  receiving: boolean; code?: string | null; sender?: string | null; message?: string | null;
  /** Negotiated codec, sent once the first video packet names it. */
  codec?: VideoCodec | null;
  /** The sender connected without a code and DTLS proved it (S35). */
  trusted?: boolean;
  /** The share ended because the sender stopped it, not because it dropped. */
  ended_by_sender?: boolean;
}

/** How the received stream's native window is hosted (S29). `embedded` =
 *  inside this window over the Receive screen's video area; `popout` = a
 *  window of its own; `none` = no stream window right now. */
export type StreamMode = "embedded" | "popout" | "none";
/** Mirror of the shell's `stream_host::StreamStatus`. */
export interface StreamStatus {
  live: boolean; mode: StreamMode; width: number; height: number;
  /** Windows confirmed the window is hidden from screen capture (B9). */
  excluded_from_capture: boolean;
  /** The receive state the core last pushed, for a screen that mounts
   *  mid-receive. Absent from a shell that predates it. */
  receiving?: boolean; code?: string | null; sender?: string | null; codec?: VideoCodec | null;
}
/** The video area in CSS px, relative to the viewport. */
export interface VideoArea { x: number; y: number; w: number; h: number }

const noStream: StreamStatus = { live: false, mode: "none", width: 0, height: 0, excluded_from_capture: true };

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
    // Correction on by default, matching the core: a curve only exists
    // because someone imported it for this headset.
    audio: { bands: [], hrtf: false, apply_to_share: false, headset_correction: true },
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
  // Empty on purpose: no model in `vcp::QUIRKS` has verified vendor evidence
  // yet, so the mock shows exactly what the real core reports today.
  vendor_controls: [],
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
  display_via: { nvapi: false, amd: false, gamma: false, ddcci: false },
  footprint: { rss_bytes: 9 * 1024 * 1024, cpu_percent: 0 },
  hardware: mockHardware.connected,
  build: {
    version: "0.1.0",
    data_dir: "C:\\Users\\you\\AppData\\Local\\Relay",
    log_file: "C:\\Users\\you\\AppData\\Local\\Relay\\logs\\core.log",
  },
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
  { pid: 1001, exe: "cod.exe", title: "Call of Duty", hwnd: 0x11001 },
  { pid: 1002, exe: "valorant.exe", title: "VALORANT", hwnd: 0x11002 },
  { pid: 1003, exe: "fl64.exe", title: "FL Studio 21", hwnd: 0x11003 },
  { pid: 1004, exe: "discord.exe", title: "Discord", hwnd: 0x11004 },
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

/** Browser-only stand-in for the dry-run listings, so the Settings cards can
 *  be read without a core. The real lines come from the uninstall planner and
 *  the live FX store. */
function mockElevationPlan(op: ElevatedOp): string[] {
  const cam = "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}";
  const tail = ["", "Windows will ask for permission before any of this happens. Decline and nothing on this PC changes."];
  switch (op) {
    case "install_apo":
      return [
        "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Render\\{endpoint}\\FxProperties :: {d04e05a6-594b-4fb6-a80d-01af5eed7d1d},15",
        "HKLM\\SOFTWARE\\Classes\\CLSID\\{5A8E9C3B-1F6D-4B0A-9C41-7E2D83A6F0B4}",
        "backup: %LOCALAPPDATA%\\Relay\\apo-backup\\{endpoint}.json (written before anything is changed)",
        ...tail,
      ];
    case "install_camera":
      return [cam, cam + "\\InprocServer32", ...tail];
    case "uninstall_apo":
      return ["[x] Restore the endpoint audio chain — {endpoint} (needs admin)", ...tail];
    default:
      return ["[x] Unregister the virtual camera — " + cam + " (needs admin)", ...tail];
  }
}

/** The three built-ins, mirroring `presets.rs::builtins()`. */
const mockPresets: SharePresetDef[] = [
  { id: "game", name: "Game", bitrate_mbps: 60, fps: 60, audio: { desktop: "game", mic: false }, cursor: false, record: false, replay_secs: 60, container: "mp4" },
  { id: "daw", name: "DAW", bitrate_mbps: 40, fps: 60, size: [2560, 1440], audio: { desktop: "system", mic: false }, cursor: true, record: false, replay_secs: 0, container: "mp4" },
  { id: "desktop", name: "Desktop", bitrate_mbps: 60, fps: 60, audio: { desktop: "system", mic: false }, cursor: true, record: false, replay_secs: 0, container: "mp4" },
];
const mockRecording: RecordingSettings = { cap_gb: 50, free_floor_gb: 10 };

/** A handful of real catalogue rows so the browser build can exercise search. */
const mockCatalog: CatalogEntry[] = [
  { name: "Sennheiser HD 560S", source: "oratory1990", rig: "", path: "oratory1990/over-ear/Sennheiser%20HD%20560S" },
  { name: "Sennheiser HD 600", source: "oratory1990", rig: "", path: "oratory1990/over-ear/Sennheiser%20HD%20600" },
  { name: "Sennheiser HD 560S", source: "crinacle", rig: "GRAS 43AG-7", path: "crinacle/GRAS%2043AG-7%20over-ear/Sennheiser%20HD%20560S" },
  { name: "Moondrop Blessing 3", source: "crinacle", rig: "711", path: "crinacle/711%20in-ear/Moondrop%20Blessing%203" },
  { name: "Beyerdynamic DT 770 Pro 80 Ohm", source: "oratory1990", rig: "", path: "oratory1990/over-ear/Beyerdynamic%20DT%20770%20Pro%2080%20Ohm" },
];

/** Browser-mode stand-in for `settings.json`. */
let mockUiPrefs: UiPrefs = { close_action: "keep_running", resilience: true, close_notice: true };

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
  /** Start the core if it is not up, and wait for it to answer.
   *
   *  Resolves `true` when it had to launch one, `false` when a core was
   *  already running. Rejects with a sentence written for a person — never a
   *  command to type. Outside Tauri there is no core to start, and the mock
   *  store is already "live", so this is a no-op. */
  async startCore(): Promise<boolean> {
    if (!isTauri()) return false;
    return invoke<boolean>("start_core");
  },
  async getUiPrefs(): Promise<UiPrefs> {
    if (!isTauri()) return structuredClone(mockUiPrefs);
    return invoke<UiPrefs>("get_ui_prefs");
  },
  async setUiPrefs(prefs: UiPrefs): Promise<UiPrefs> {
    if (!isTauri()) { mockUiPrefs = structuredClone(prefs); return structuredClone(mockUiPrefs); }
    return invoke<UiPrefs>("set_ui_prefs", { prefs });
  },
  async getAutostart(): Promise<boolean> {
    if (!isTauri()) return mockAutostart;
    return invoke<boolean>("get_autostart");
  },
  async setAutostart(enabled: boolean): Promise<boolean> {
    if (!isTauri()) { mockAutostart = enabled; return enabled; }
    return invoke<boolean>("set_autostart", { enabled });
  },
  /** Start a share from explicit settings rather than a preset.
   *
   *  No screen calls this by design: the Share screen goes through
   *  `startSharePreset`, which lets the core resolve the game's audio pid and
   *  the recording folder. This is the escape hatch the CLI uses
   *  (`relay-core share-start`) and the seam a future scripted share would
   *  use, so it stays wired end to end. */
  async startShare(request: ShareRequest): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("start_share", { request });
  },
  async stopShare(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("stop_share");
  },
  /** `peerId` names a remembered PC (S35): the code may then be empty, and the
   *  core resolves the id itself -- the UI never handles a fingerprint. */
  async startSharePreset(
    preset: string, code: string, peer?: string | null, peerId?: string | null,
  ): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("start_share_preset", {
      preset, code, peer: peer ?? null, peerId: peerId ?? null,
    });
  },
  async record(on: boolean): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("record", { on });
  },
  async saveReplay(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("save_replay");
  },
  async switchSource(target: SourceTarget): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("switch_source", { target });
  },
  /** Per-track gain and mute on the running share or receive, live (S37). */
  async setMixer(side: MixerSide, faders: FaderSet): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("set_mixer", { side, faders });
  },
  async listPresets(): Promise<PresetsReply> {
    if (!isTauri()) return structuredClone({ presets: mockPresets, recording: mockRecording });
    return invoke<PresetsReply>("list_presets");
  },
  async savePreset(preset: SharePresetDef): Promise<void> {
    if (!isTauri()) {
      const i = mockPresets.findIndex((p) => p.id === preset.id);
      if (i >= 0) mockPresets[i] = structuredClone(preset);
      else mockPresets.push(structuredClone(preset));
      return;
    }
    return invoke<void>("save_preset", { preset });
  },
  async deletePreset(id: string): Promise<void> {
    if (!isTauri()) {
      const i = mockPresets.findIndex((p) => p.id === id);
      if (i >= 0) mockPresets.splice(i, 1);
      return;
    }
    return invoke<void>("delete_preset", { id });
  },
  async setRecordingSettings(settings: RecordingSettings): Promise<void> {
    if (!isTauri()) {
      mockRecording.dir = settings.dir;
      mockRecording.cap_gb = settings.cap_gb;
      mockRecording.free_floor_gb = settings.free_floor_gb;
      return;
    }
    return invoke<void>("set_recording_settings", { settings });
  },
  async startReceive(request: ReceiveRequest): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("start_receive", { request });
  },
  async stopReceive(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("stop_receive");
  },
  /** Where the Receive screen's video area is, so the shell can put the
   *  stream window over it; `null` when the screen is not showing. */
  async setVideoArea(area: VideoArea | null): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("set_video_area", { area });
  },
  async setStreamMode(mode: "embedded" | "popout"): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("set_stream_mode", { mode });
  },
  async streamStatus(): Promise<StreamStatus> {
    if (!isTauri()) return { ...noStream };
    return invoke<StreamStatus>("stream_status");
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
  /** What this PC can do with video. Spawns a probe child in the core, so call
   *  it once when a screen opens, not on every state refresh. */
  async shareCapabilities(): Promise<ShareCapabilities> {
    if (!isTauri()) {
      return {
        can_share: true, can_receive: true, adapters: ["NVIDIA GeForce RTX 3090"],
        encoders: ["NVIDIA HEVC Encoder MFT", "NVIDIA H.264 Encoder MFT"],
        decoders: ["Microsoft HEVC Video Extension", "Microsoft H264 Video Decoder MFT"],
        share_codecs: ["hevc", "h264"], receive_codecs: ["hevc", "h264"],
      };
    }
    return invoke<ShareCapabilities>("share_capabilities");
  },
  /** Will an inbound share reach this PC, or is Windows Firewall dropping
   *  it? Read-only and unelevated. Call it when a screen opens. */
  async firewallStatus(): Promise<FirewallStatus> {
    if (!isTauri()) {
      return {
        state: "allowed", program: "C:\\Users\\you\\AppData\\Local\\Relay\\relay-share.exe",
        rule_present: true, blocking_rules: 0, stale_rules: 0,
        policy: { active_profiles: 2, enabled: true, default_inbound_block: true },
        unknown: false,
      };
    }
    return invoke<FirewallStatus>("firewall_status");
  },
  /** Read-only probe: is the Relay APO on the default render endpoint? */
  async apoStatus(): Promise<ApoStatus> {
    if (!isTauri()) return { installed: false, endpoint: null, running: false };
    return invoke<ApoStatus>("apo_status");
  },
  /** Register the APO (backup-then-apply). Direct, unelevated path — the
   *  Settings card goes through `runElevated` instead. Kept for the CLI and
   *  the VM runbook, where the gates are already armed. */
  async installApo(): Promise<void> {
    if (!isTauri()) throw new Error("Installing the APO needs the Relay core");
    return invoke<void>("install_apo");
  },
  /** Restore the endpoint's FX chain from the install backup and unregister. */
  async uninstallApo(): Promise<void> {
    if (!isTauri()) throw new Error("Removing the APO needs the Relay core");
    return invoke<void>("uninstall_apo");
  },
  /** Exactly what an elevated op would change on this PC. Read-only, and the
   *  listing the user reads *before* the Windows permission prompt. */
  async elevationPlan(op: ElevatedOp): Promise<string[]> {
    if (!isTauri()) return mockElevationPlan(op);
    return invoke<string[]>("elevation_plan", { op });
  },
  /** Ask for administrator rights and run one op. Resolves (never throws) on
   *  a declined prompt — `declined` is the answer, and nothing changed. */
  async runElevated(op: ElevatedOp): Promise<ElevationResult> {
    if (!isTauri()) {
      if (op === "install_camera") mockVdevice.camera_registered = true;
      if (op === "uninstall_camera") mockVdevice.camera_registered = false;
      return { declined: false, ok: true, lines: [`${op}: done (mock)`] };
    }
    return invoke<ElevationResult>("run_elevated", { op });
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
  /** Search the bundled headphone catalogue. Offline — the index ships with
   *  Relay; only picking a model fetches anything. */
  async searchCatalog(query: string): Promise<CatalogEntry[]> {
    if (!isTauri()) {
      const q = query.trim().toLowerCase();
      return q
        ? mockCatalog.filter((e) => e.name.toLowerCase().includes(q))
        : [];
    }
    return invoke<CatalogEntry[]>("search_catalog", { query });
  },
  /** Add a catalogue model: downloads its measurement once, then caches. */
  async addHeadsetFromCatalog(entry: CatalogEntry, endpoint: string | null): Promise<void> {
    if (!isTauri()) {
      mockHardware.headsets.push({
        id: `${entry.name}-${entry.source}`.toLowerCase().replace(/[^a-z0-9]+/g, "-"),
        name: entry.name, kind: entry.path.includes("in-ear") ? "iem" : "headphone",
        curve: [[20, 6], [1000, 0], [20000, -3]], source: entry.source,
        endpoints: endpoint ? [endpoint] : [],
      });
      return;
    }
    return invoke<void>("add_headset_from_catalog", { entry, endpoint });
  },
  /** Remove the recorded registration; empties installed.json. */
  async uninstallVcam(): Promise<void> {
    if (!isTauri()) { mockVdevice.camera_registered = false; return; }
    return invoke<void>("uninstall_vcam");
  },
  /** Everything the uninstaller would touch, in order. Read-only; generated
   *  by the same code that runs the uninstall (`relay-core uninstall`). */
  async uninstallPlan(keepData: boolean): Promise<string[]> {
    if (!isTauri()) return [
      "[ ] Close the Relay window — relay-ui.exe",
      "[x] Stop the core (restores your audio and display settings) — \\\\.\\pipe\\relay-core",
      "[ ] Restore the endpoint audio chain — not installed",
      "[ ] Unregister the virtual camera — not installed",
      "[ ] Remove the start-at-login entry — HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\Relay",
      "[x] Delete the program files — <install dir>",
      keepData
        ? "[ ] Delete your profiles and settings — %LOCALAPPDATA%\\Relay"
        : "[x] Delete your profiles and settings — %LOCALAPPDATA%\\Relay",
      "",
      keepData
        ? "Your profiles and hardware library are kept in %LOCALAPPDATA%\\Relay."
        : "Everything above is removed. Nothing else on this PC was changed by Relay.",
    ];
    return invoke<string[]>("uninstall_plan", { keepData });
  },
  /** Hand over to the Windows uninstaller and stop the core. */
  async launchUninstaller(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("launch_uninstaller");
  },
  async discoverReceivers(): Promise<DiscoveredReceiver[]> {
    if (!isTauri()) return [{ name: "living-room-pc", addr: "192.168.1.42", port: 0 }];
    return invoke<DiscoveredReceiver[]>("discover_receivers");
  },

  // Remembered PCs (S35).
  async listPeers(): Promise<Peer[]> {
    if (!isTauri()) return [];
    return invoke<Peer[]>("list_peers");
  },
  /** Real: the PC needs a code again next time, like a stranger. */
  async forgetPeer(id: string): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("forget_peer", { id });
  },
  async setPeerFavourite(id: string, favourite: boolean): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("set_peer_favourite", { id, favourite });
  },

  /** The user has read the last-crash line; the core clears it (S38). */
  async ackCrash(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("ack_crash");
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
  /** The stream window appeared, changed mode, or went away. */
  stream?: (s: StreamStatus) => void;
  recordingStatus?: (s: RecordingStatus) => void;
  replaySaved?: (s: ReplaySaved) => void;
  sourceChanged?: (s: SourceChangedData) => void;
  sharePreview?: (s: SharePreview) => void;
  /** The shell began trying to start a core. */
  starting?: () => void;
  /** A core is up (it was already running, or the shell started one). */
  started?: () => void;
  /** The core could not be started; the text is ready to show as-is. */
  startFailed?: (message: string) => void;
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
    listen<StreamStatus>("core://stream", (e) => handlers.stream?.(e.payload)),
    listen<RecordingStatus>("core://recording-status", (e) => handlers.recordingStatus?.(e.payload)),
    listen<ReplaySaved>("core://replay-saved", (e) => handlers.replaySaved?.(e.payload)),
    listen<SourceChangedData>("core://source-changed", (e) => handlers.sourceChanged?.(e.payload)),
    listen<SharePreview>("core://share-preview", (e) => handlers.sharePreview?.(e.payload)),
    listen<void>("core://starting", () => handlers.starting?.()),
    listen<void>("core://started", () => handlers.started?.()),
    listen<string>("core://start-failed", (e) => handlers.startFailed?.(e.payload)),
  ]);
  return () => unlisteners.forEach((u) => u());
}

export const fmtMb = (bytes: number) => (bytes / (1024 * 1024)).toFixed(0);
