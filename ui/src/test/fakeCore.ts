/**
 * A scripted stand-in for `relay-core` behind the Tauri command layer.
 *
 * It answers every command in `ui/src/lib/ipc.ts` with the shapes that file
 * declares — which is the point: the TypeScript mirror of `ipc.rs` is the
 * contract, and a test that renders a screen against this fake proves the
 * screen reads that contract correctly. State is mutable, so a test can
 * assert that Save actually reached the core and that a re-read sees it.
 */
import type {
  ApoStatus, CatalogEntry, CoreState, HardwareItem, HardwareReply, PresetsReply, Preview,
  ElevatedOp,
  ProbeReport, ProcessInfo, Profile, ProfileSummary, RecordingSettings, ShareCapabilities,
  SharePresetDef, VdeviceStatus,
} from "../lib/ipc";
import { newProfile, summarize } from "../lib/ipc";
import type { InvokeHandler } from "./tauriMock";

export interface FakeCore {
  state: CoreState;
  profiles: Map<string, Profile>;
  hardware: HardwareReply;
  presets: SharePresetDef[];
  recording: RecordingSettings;
  processes: ProcessInfo[];
  catalog: CatalogEntry[];
  apo: ApoStatus;
  vdevice: VdeviceStatus;
  capabilities: ShareCapabilities;
  autostart: boolean;
  /** How the next UAC prompt is answered. `decline` is a normal answer, not
   *  an error: Windows resolves, nothing was attempted, nothing changed. */
  elevation: { decline: boolean };
  /** Commands that should reject, with the message the core would give. */
  fail: Map<string, string>;
  handler: InvokeHandler;
}

function profile(
  id: string, name: string, note: string, exe: string,
  headset: string | null, monitor: string | null,
  share: Profile["share"], status: Profile["status"],
): Profile {
  const p = newProfile(name, exe);
  p.id = id; p.note = note; p.share = share; p.status = status;
  if (headset) p.headset = headset;
  if (monitor) p.monitor = monitor;
  return p;
}

export function makeFakeCore(overrides: Partial<Omit<FakeCore, "handler">> = {}): FakeCore {
  const hardware: HardwareReply = {
    headsets: [
      { id: "hd560s", name: "HD 560S", kind: "headphone", source: "oratory1990", endpoints: ["ep:dac"], curve: [[20, -4.11], [1000, 0], [20000, -6.2]] },
      { id: "blessing3", name: "Moondrop Blessing 3", kind: "iem", source: "crinacle", endpoints: ["ep:dongle"] },
    ],
    monitors: [
      { id: "mon:ULTRAGEAR", name: "LG ULTRAGEAR+", panel: "Nano IPS", ddcci: [0x10, 0x12, 0x60] },
    ],
    interfaces: [],
    connected: {
      endpoints: [
        { key: "ep:dac", name: "USB Audio 2.0", default: true },
        { key: "ep:hdmi", name: "LG ULTRAGEAR+ (NVIDIA HDA)", default: false },
      ],
      monitors: [
        { id: "mon:ULTRAGEAR", name: "LG ULTRAGEAR+", native: [3840, 2160], refresh_hz: 144, primary: true, hmonitor: 65537, gdi_name: "\\\\.\\DISPLAY1" },
      ],
      headset: "hd560s",
    },
  };

  const core: FakeCore = {
    state: {
      active_profile: null,
      foreground: null,
      sharing: { kind: "off" },
      audio_chain: "bypass",
      display_state: "default",
      display_via: { nvapi: false, amd: false, gamma: false, ddcci: false },
      footprint: { rss_bytes: 9 * 1024 * 1024, cpu_percent: 0 },
      hardware: hardware.connected,
      build: {
        version: "0.1.0-test",
        data_dir: "C:\\Users\\test\\AppData\\Local\\Relay",
        log_file: "C:\\Users\\test\\AppData\\Local\\Relay\\logs\\core.log",
      },
    },
    profiles: new Map(
      [
        profile("1", "Call of Duty", "Footsteps · dark-map colors", "cod.exe", "hd560s", "mon:ULTRAGEAR", "game", "ready"),
        profile("2", "Valorant", "Neutral EQ · vivid", "valorant.exe", "hd560s", null, "game", "ready"),
        profile("3", "Elden Ring", "Untouched audio · warm colors", "eldenring.exe", null, null, "off", "draft"),
      ].map((p) => [p.id, p] as const),
    ),
    hardware,
    presets: [
      { id: "game", name: "Game", bitrate_mbps: 60, fps: 60, audio: { desktop: "game", mic: false }, cursor: false, record: false, replay_secs: 60, container: "mp4" },
      { id: "daw", name: "DAW", bitrate_mbps: 40, fps: 60, size: [2560, 1440], audio: { desktop: "system", mic: false }, cursor: true, record: false, replay_secs: 0, container: "mp4" },
      { id: "desktop", name: "Desktop", bitrate_mbps: 60, fps: 60, audio: { desktop: "system", mic: false }, cursor: true, record: false, replay_secs: 0, container: "mp4" },
    ],
    recording: { cap_gb: 50, free_floor_gb: 10 },
    processes: [
      { pid: 1001, exe: "cod.exe", title: "Call of Duty", hwnd: 0x11001 },
      { pid: 1004, exe: "discord.exe", title: "Discord", hwnd: 0x11004 },
    ],
    catalog: [
      { name: "Sennheiser HD 560S", source: "oratory1990", rig: "", path: "oratory1990/over-ear/Sennheiser%20HD%20560S" },
      { name: "Sennheiser HD 600", source: "oratory1990", rig: "", path: "oratory1990/over-ear/Sennheiser%20HD%20600" },
      { name: "Moondrop Blessing 3", source: "crinacle", rig: "711", path: "crinacle/711%20in-ear/Moondrop%20Blessing%203" },
    ],
    apo: { installed: false, endpoint: null, running: false },
    vdevice: {
      windows_build: 26200,
      camera_supported: true,
      camera_registered: false,
      obs_virtualcam: null,
      mic_targets: [{ endpoint_id: "{0.0.0}.{cable}", name: "CABLE Input (VB-Audio Virtual Cable)", kind: "vb_cable" }],
      // Decided already, so the app opens on the normal shell. Tests that
      // want the first-run screen set this back to null.
      consent: { decided_at: "2026-09-14T00:00:00Z", apo: false, camera: false, microphone: false },
      elevated: false,
    },
    capabilities: {
      can_share: true, can_receive: true,
      encoders: ["NVIDIA HEVC Encoder MFT"], decoders: ["Microsoft HEVC Video Extension"],
    },
    autostart: false,
    elevation: { decline: false },
    fail: new Map(),
    handler: () => undefined,
    ...overrides,
  };

  const summaries = (): ProfileSummary[] => [...core.profiles.values()].map(summarize);

  const table: Record<string, (a: Record<string, unknown>) => unknown> = {
    core_status: () => structuredClone(core.state),
    list_profiles: () => summaries(),
    get_profile: (a) => {
      const p = core.profiles.get(a.id as string);
      if (!p) throw new Error("no such profile");
      return structuredClone(p);
    },
    save_profile: (a) => {
      const p = a.profile as Profile;
      core.profiles.set(p.id, structuredClone(p));
    },
    delete_profile: (a) => void core.profiles.delete(a.id as string),
    apply_profile: (a) => {
      const p = core.profiles.get(a.id as string);
      if (!p) throw new Error("no such profile");
      core.state.active_profile = summarize(p);
      core.state.display_state = "applied";
    },
    restore_all: () => {
      core.state.active_profile = null;
      core.state.display_state = "default";
      core.state.audio_chain = "bypass";
    },
    list_processes: () => structuredClone(core.processes),
    get_autostart: () => core.autostart,
    set_autostart: (a) => (core.autostart = a.enabled as boolean),
    start_share: (a) => {
      const req = a.request as { peer?: string | null };
      core.state.sharing = { kind: "sharing", peer: req.peer ?? "peer" };
    },
    stop_share: () => void (core.state.sharing = { kind: "off" }),
    start_share_preset: (a) => {
      core.state.sharing = { kind: "sharing", peer: (a.peer as string | null) ?? "living-room-pc" };
    },
    record: () => undefined,
    save_replay: () => undefined,
    switch_source: () => undefined,
    list_presets: (): PresetsReply => structuredClone({ presets: core.presets, recording: core.recording }),
    save_preset: (a) => {
      const p = a.preset as SharePresetDef;
      const i = core.presets.findIndex((x) => x.id === p.id);
      if (i >= 0) core.presets[i] = structuredClone(p);
      else core.presets.push(structuredClone(p));
    },
    delete_preset: (a) => {
      const i = core.presets.findIndex((x) => x.id === a.id);
      if (i >= 0) core.presets.splice(i, 1);
    },
    set_recording_settings: (a) => void (core.recording = structuredClone(a.settings as RecordingSettings)),
    start_receive: () => undefined,
    stop_receive: () => undefined,
    list_hardware: (): HardwareReply => structuredClone(core.hardware),
    save_hardware: (a) => {
      const item = a.item as HardwareItem;
      if (item.kind === "headset") {
        const i = core.hardware.headsets.findIndex((h) => h.id === item.value.id);
        if (i >= 0) core.hardware.headsets[i] = structuredClone(item.value);
        else core.hardware.headsets.push(structuredClone(item.value));
      } else {
        const i = core.hardware.monitors.findIndex((m) => m.id === item.value.id);
        if (i >= 0) core.hardware.monitors[i] = structuredClone(item.value);
        else core.hardware.monitors.push(structuredClone(item.value));
      }
    },
    delete_hardware: (a) => {
      core.hardware.headsets = core.hardware.headsets.filter((h) => h.id !== a.id);
      core.hardware.monitors = core.hardware.monitors.filter((m) => m.id !== a.id);
    },
    probe_hardware: (): ProbeReport => structuredClone(core.hardware.connected),
    import_curve: (a) => {
      const h = core.hardware.headsets.find((x) => x.id === a.headset);
      if (!h) throw new Error("no such headset");
      h.curve = [[20, 0], [20000, 0]];
      return h.curve;
    },
    render_preview: (): Preview => ({
      original: "C:\\Relay\\preview\\original.wav",
      processed: "C:\\Relay\\preview\\processed.wav",
      sample_rate: 48000,
      hrtf_applied: true,
    }),
    share_capabilities: () => structuredClone(core.capabilities),
    apo_status: () => structuredClone(core.apo),
    // The two halves of the S6 flow. `elevation_plan` is read-only and is
    // what the user reads *before* Windows asks; `run_elevated` is the only
    // thing that changes the machine.
    elevation_plan: (a) => {
      const cam = "HKLM\SOFTWARE\Classes\CLSID\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}";
      const tail = ["", "Windows will ask for permission before any of this happens. Decline and nothing on this PC changes."];
      switch (a.op as ElevatedOp) {
        case "install_apo":
          return [
            "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\ep:dac\FxProperties :: {d04e05a6-594b-4fb6-a80d-01af5eed7d1d},15",
            "HKLM\SOFTWARE\Classes\CLSID\{5A8E9C3B-1F6D-4B0A-9C41-7E2D83A6F0B4}",
            "backup: %LOCALAPPDATA%\Relay\apo-backup\ep:dac.json (written before anything is changed)",
            ...tail,
          ];
        case "install_camera":
          return [cam, cam + "\InprocServer32", ...tail];
        case "uninstall_apo":
          return ["[x] Restore the endpoint audio chain — ep:dac (needs admin)", ...tail];
        default:
          return ["[x] Unregister the virtual camera — " + cam + " (needs admin)", ...tail];
      }
    },
    run_elevated: (a) => {
      const op = a.op as ElevatedOp;
      if (core.elevation.decline) {
        return {
          declined: true, ok: false,
          lines: ["Windows permission was declined. Nothing on this PC was changed."],
        };
      }
      if (op === "install_apo") core.apo = { installed: true, endpoint: "ep:dac", running: true };
      if (op === "uninstall_apo") core.apo = { installed: false, endpoint: null, running: false };
      if (op === "install_camera") core.vdevice.camera_registered = true;
      if (op === "uninstall_camera") core.vdevice.camera_registered = false;
      return { declined: false, ok: true, lines: [`${op}: done.`] };
    },
    install_apo: () => {
      core.apo = { installed: true, endpoint: "ep:dac", running: true };
    },
    uninstall_apo: () => {
      core.apo = { installed: false, endpoint: null, running: false };
    },
    vdevice_status: () => structuredClone(core.vdevice),
    set_vdevice_consent: (a) => {
      core.vdevice.consent = {
        decided_at: "2026-09-14T00:00:00Z",
        apo: a.apo as boolean, camera: a.camera as boolean, microphone: a.microphone as boolean,
      };
    },
    vdevice_dry_run: () => [
      "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}",
      "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}\\InprocServer32",
      "file: <install dir>\\relay_vdevice.dll (stays in place; only registered)",
    ],
    install_vcam: () => void (core.vdevice.camera_registered = true),
    uninstall_vcam: () => void (core.vdevice.camera_registered = false),
    search_catalog: (a) => {
      const q = String(a.query ?? "").trim().toLowerCase();
      return q ? core.catalog.filter((e) => e.name.toLowerCase().includes(q)) : [];
    },
    add_headset_from_catalog: (a) => {
      const e = a.entry as CatalogEntry;
      const endpoint = a.endpoint as string | null;
      core.hardware.headsets.push({
        id: `${e.name}-${e.source}`.toLowerCase().replace(/[^a-z0-9]+/g, "-"),
        name: e.name,
        kind: e.path.includes("in-ear") ? "iem" : "headphone",
        curve: [[20, 6], [1000, 0], [20000, -3]],
        source: e.source,
        endpoints: endpoint ? [endpoint] : [],
      });
    },
    uninstall_plan: (a) => {
      const keep = a.keepData as boolean;
      return [
        "[ ] Close the Relay window — relay-ui.exe",
        "[x] Stop the core (restores your audio and display settings) — \\\\.\\pipe\\relay-core",
        core.apo.installed
          ? "[x] Restore the endpoint audio chain — ep:dac"
          : "[ ] Restore the endpoint audio chain — not installed",
        core.vdevice.camera_registered
          ? "[x] Unregister the virtual camera — HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}"
          : "[ ] Unregister the virtual camera — not installed",
        core.autostart
          ? "[x] Remove the start-at-login entry — HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\Relay"
          : "[ ] Remove the start-at-login entry — not set",
        "[x] Delete the program files — <install dir>",
        keep
          ? "[ ] Delete your profiles and settings — %LOCALAPPDATA%\\Relay"
          : "[x] Delete your profiles and settings — %LOCALAPPDATA%\\Relay",
        "",
        keep
          ? "Your profiles and hardware library are kept in %LOCALAPPDATA%\\Relay."
          : "Everything above is removed. Nothing else on this PC was changed by Relay.",
      ];
    },
    launch_uninstaller: () => undefined,
    discover_receivers: () => [
      { name: "living-room-pc", addr: "192.168.1.42", port: 0 },
      { name: "studio-pc", addr: "192.168.1.51", port: 0 },
    ],
  };

  core.handler = (cmd, args) => {
    const failure = core.fail.get(cmd);
    if (failure) throw new Error(failure);
    const fn = table[cmd];
    if (!fn) throw new Error(`fake core has no handler for "${cmd}"`);
    return fn(args);
  };

  return core;
}

/** Every command the fake core knows. The UI must not invent one outside it. */
export const KNOWN_COMMANDS: readonly string[] = [
  "core_status", "list_profiles", "get_profile", "save_profile", "delete_profile",
  "apply_profile", "restore_all", "list_processes", "get_autostart", "set_autostart",
  "start_share", "stop_share", "start_share_preset", "record", "save_replay",
  "switch_source", "list_presets", "save_preset", "delete_preset",
  "set_recording_settings", "start_receive", "stop_receive", "list_hardware",
  "save_hardware", "delete_hardware", "probe_hardware", "import_curve",
  "render_preview", "share_capabilities", "apo_status", "install_apo", "uninstall_apo",
  "elevation_plan", "run_elevated",
  "vdevice_status", "set_vdevice_consent", "vdevice_dry_run", "install_vcam",
  "uninstall_vcam", "search_catalog", "add_headset_from_catalog", "uninstall_plan",
  "launch_uninstaller", "discover_receivers",
];
