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
  ApoStatus, AudioEffectsStatus, CatalogEntry, CoreState, HardwareItem, HardwareReply, ListeningDevice, PresetsReply, Preview,
  ElevatedOp, FirewallStatus, Peer,
  ProbeReport, ProcessInfo, Profile, ProfileSummary, RecordingSettings, ShareCapabilities,
  SharePresetDef, StreamStatus, UiPrefs, UpdateStatus, VdeviceStatus, AudioDevices, DeviceTrack, MixerSide,
} from "../lib/ipc";
import type { EndpointApo, GameEqAction, GameEqExport, GameEqStatus, LearnFileStatus, LearnView, LookTargets, VideoFile } from "../lib/ipc";
import { LOOK_PRIVACY, TOURNAMENT_NOTICE, VIDEO_LOCAL_ONLY, VIDEO_PRIVACY, devicePrefKey, newProfile, opEndpoint, opKind, summarize } from "../lib/ipc";
import type { InvokeHandler } from "./tauriMock";

/** The two render endpoints the fake APO card lists (S42). */
export const DAC = "{f8ae226b-a4e3-45ab-97fc-3977dad232d1}";
export const SPDIF = "{0b5c7e21-1d2e-4f3a-9b8c-5d6e7f8a9b0c}";

/** Build an `ApoStatus` from its per-output list, the way the core does:
 *  the top-level fields describe the default output. */
export function apoStatus(endpoints: EndpointApo[]): ApoStatus {
  const d = endpoints.find((e) => e.is_default);
  return { installed: d?.installed ?? false, endpoint: d?.endpoint ?? null, running: d?.running ?? false, endpoints };
}

/** Install on / remove from one output; `null` removes from every output. */
function setApo(core: FakeCore, endpoint: string | null, installed: boolean) {
  const list = (core.apo.endpoints ?? []).map((e) =>
    endpoint === null || e.endpoint === endpoint
      ? { ...e, installed, backed_up: installed, running: installed }
      : e);
  core.apo = apoStatus(list);
}

export interface FakeCore {
  state: CoreState;
  profiles: Map<string, Profile>;
  hardware: HardwareReply;
  presets: SharePresetDef[];
  recording: RecordingSettings;
  processes: ProcessInfo[];
  catalog: CatalogEntry[];
  apo: ApoStatus;
  /** S44: Windows' protected-audiodg switch. Off by default, as shipped. */
  audioEffects: AudioEffectsStatus;
  vdevice: VdeviceStatus;
  capabilities: ShareCapabilities;
  /** What Windows Firewall will do to an incoming share. Defaults to the
   *  healthy machine so screens that do not care show no banner. */
  firewall: FirewallStatus;
  /** The received stream's native window, as the shell reports it (S29). */
  stream: StreamStatus;
  /** Remembered PCs (S35). Empty by default: most screens must not change
   *  when the list is empty, which is how every user starts. */
  peers: Peer[];
  autostart: boolean;
  /** `settings.json`: what closing the window means. */
  prefs: UiPrefs;
  /** The updater (S45). Nothing on offer by default. */
  update: UpdateStatus;
  /** The version "Skip this version" last recorded. */
  skippedUpdate: string | null;
  /** Active audio endpoints (S40), both directions. */
  audioDevices: AudioDevices;
  /** How the next UAC prompt is answered. `decline` is a normal answer, not
   *  an error: Windows resolves, nothing was attempted, nothing changed. */
  elevation: { decline: boolean };
  /** S46: what the learner has gathered per profile id (the core keeps it
   *  per exe; per profile is enough for screens). */
  gameEq: Map<string, { progress: number; candidate: [number, number][] | null; needsRelearn: boolean; learningNow: boolean }>;
  /** S47: learn views by lower-case exe. Missing = never touched. */
  learn: Map<string, LearnView>;
  /** S48: video jobs by profile id (the core keeps the current one). */
  fileJobs: Map<string, LearnFileStatus>;
  /** S48: what `list_learn_videos` lists, and what the native picker returns. */
  videos: VideoFile[];
  pickedVideo: string | null;
  /** S48: the ETA the fake game EQ status reports (seconds). */
  gameEqEta: number | null;
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
    apo: apoStatus([
      { endpoint: DAC, name: "Headphones (USB DAC)", is_default: true, installed: false, backed_up: false, running: false },
      { endpoint: SPDIF, name: "Digital Output (S/PDIF)", is_default: false, installed: false, backed_up: false, running: false },
    ]),
    audioEffects: {
      value: null, allowed: false, changed_by_relay: false, set_elsewhere: false, unknown: false,
    },
    vdevice: {
      windows_build: 26200,
      camera_supported: true,
      camera_path: "frame_server",
      camera_registered: false,
      obs_virtualcam: null,
      mic_targets: [{ endpoint_id: "{0.0.0}.{cable}", name: "CABLE Input (VB-Audio Virtual Cable)", kind: "vb_cable" }],
      // Decided already, so the app opens on the normal shell. Tests that
      // want the first-run screen set this back to null.
      consent: { decided_at: "2026-09-14T00:00:00Z", apo: false, camera: false, microphone: false },
      elevated: false,
    },
    capabilities: {
      can_share: true, can_receive: true, adapters: ["NVIDIA GeForce RTX 3090"],
      encoders: ["NVIDIA HEVC Encoder MFT", "NVIDIA H.264 Encoder MFT"],
      decoders: ["Microsoft HEVC Video Extension", "Microsoft H264 Video Decoder MFT"],
      share_codecs: ["hevc", "h264"], receive_codecs: ["hevc", "h264"],
    },
    firewall: {
      state: "allowed",
      program: "C:\\Relay\\relay-share.exe",
      rule_present: true, blocking_rules: 0, stale_rules: 0,
      policy: { active_profiles: 2, enabled: true, default_inbound_block: true },
      unknown: false,
    },
    stream: { live: false, mode: "none", width: 0, height: 0, excluded_from_capture: true, receiving: false },
    peers: [],
    autostart: false,
    prefs: {
      close_action: "keep_running", resilience: true, close_notice: true, audio_devices: {},
      auto_check_updates: true, auto_install_updates: false, prerelease_updates: false,
    },
    update: {
      current: "0.1.0", phase: "idle", available: null, last_check: null,
      last_error: null, last_result: null, waiting_for: null,
    },
    skippedUpdate: null,
    audioDevices: {
      render: [
        { id: "{spk}", name: "Speakers (USB Audio 2.0)", is_default: true },
        { id: "{hdmi}", name: "LG ULTRAGEAR+ (NVIDIA HDA)", is_default: false },
      ],
      capture: [
        { id: "{rode}", name: "Microphone (Rodecaster)", is_default: true },
        { id: "{cam}", name: "Webcam microphone", is_default: false },
      ],
    },
    elevation: { decline: false },
    gameEq: new Map(),
    learn: new Map(),
    fileJobs: new Map(),
    videos: [
      { name: "Relay 2026-10-01 21-04.mp4", path: "C:\\Users\\test\\Videos\\Relay\\Relay 2026-10-01 21-04.mp4", size_bytes: 1_800_000_000, modified_unix: 1_790_000_000, relay: true },
      { name: "match.mkv", path: "C:\\Users\\test\\Videos\\match.mkv", size_bytes: 900_000_000, modified_unix: 1_789_000_000, relay: false },
    ],
    pickedVideo: "C:\\Users\\test\\Desktop\\clip.webm",
    gameEqEta: 240,
    fail: new Map(),
    handler: () => undefined,
    ...overrides,
  };

  const learnView = (exe: string): LearnView => {
    const key = exe.toLowerCase();
    let v = core.learn.get(key);
    if (!v) {
      v = { exe: key, enabled: false, status: "off", sampling: false, monitors: [], imported: null,
        privacy: LOOK_PRIVACY, tournament: TOURNAMENT_NOTICE };
      core.learn.set(key, v);
    }
    return v;
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
    get_ui_prefs: () => structuredClone(core.prefs),
    set_ui_prefs: (a) => (core.prefs = structuredClone(a.prefs as UiPrefs)),
    ack_crash: () => void (core.state.last_crash = null),
    game_eq: (a) => {
      const id = a.id as string;
      const p = core.profiles.get(id);
      if (!p) throw new Error("no such profile");
      const action = a.action as GameEqAction;
      const rec = core.gameEq.get(id) ?? { progress: 0, candidate: null, needsRelearn: false, learningNow: false };
      const au = p.audio;
      let exported: GameEqExport | undefined;
      switch (action.kind) {
        case "set_learning": au.learn_game_eq = action.enabled; break;
        case "set_auto_apply": au.game_eq_auto_apply = action.enabled; break;
        case "set_goal": {
          au.game_eq_goal = action.goal;
          // The fake "re-derives" by scaling: Immersion is gentler.
          if (rec.candidate) {
            const k = action.goal === "immersion" ? 0.5 : 1;
            rec.candidate = rec.candidate.map(([hz, db]) => [hz, Math.round(db * k * 10) / 10]);
            if (au.game_eq?.source === "learned") au.game_eq = { ...au.game_eq, curve: rec.candidate };
          }
          break;
        }
        case "apply":
          if (!rec.candidate) throw new Error("there is no learned curve to apply yet");
          au.game_eq = { curve: rec.candidate, source: au.game_eq && au.game_eq.source !== "learned" ? "tuned" : "learned" };
          break;
        case "relearn": rec.candidate = null; rec.progress = 0; break;
        case "reset": delete au.game_eq; delete au.learn_game_eq; rec.candidate = null; rec.progress = 0; break;
        case "import": {
          const f = JSON.parse(action.text) as { game: { exe: string }; curve: [number, number][] };
          if (f.game.exe.toLowerCase() !== p.game.exe.toLowerCase()) {
            throw new Error(`this file is for ${f.game.exe}, not ${p.game.exe}`);
          }
          au.game_eq = { curve: f.curve, source: "imported", base: f.curve };
          delete au.learn_game_eq;
          break;
        }
        case "export": {
          const c = au.game_eq?.curve ?? rec.candidate;
          if (!c) throw new Error("there is no game EQ to export yet");
          exported = {
            text: JSON.stringify({ format: "relay-game-eq", schema: 1, game: { exe: p.game.exe }, curve: c, note: action.note }),
            path: `C:\\Users\\test\\AppData\\Local\\Relay\\data\\exports\\${p.game.exe.replace(/\.exe$/i, "")}-game-eq.json`,
          };
          break;
        }
        default: break;
      }
      core.gameEq.set(id, rec);
      const processing = au.bands.length > 0 || au.hrtf || !!au.limiter || !!au.game_eq;
      const imported = au.game_eq?.source === "imported" || au.game_eq?.source === "tuned";
      const on = au.learn_game_eq ?? (processing && !imported);
      const applied = au.game_eq?.curve ?? null;
      const offer = rec.candidate;
      const same = !!offer && !!applied && JSON.stringify(offer) === JSON.stringify(applied);
      const state: GameEqStatus["state"] = rec.needsRelearn && !offer
        ? (applied || on ? "needs_relearn" : "off")
        : same || (applied && offer && !on) ? "applied"
          : offer && on ? "ready" : applied ? "applied" : on ? "learning" : "off";
      const status: GameEqStatus = {
        exe: p.game.exe, state, learning_on: on, needs_goal: on && !au.game_eq_goal,
        learning_now: rec.learningNow, goal: au.game_eq_goal ?? null, auto_apply: !!au.game_eq_auto_apply,
        source: au.game_eq?.source ?? null, progress: offer ? 100 : rec.progress, active_minutes: 4.5,
        targets: 120, maskers: 20, min_targets: 300, min_maskers: 60, distinct_voices: 2,
        exe_version: "1.0.0.0", applied, offer, note: "", last_error: null,
        eta_secs: offer ? 0 : core.gameEqEta,
      };
      return { status, ...(exported ? { export: exported } : {}) };
    },
    list_learn_videos: () => ({
      videos: structuredClone(core.videos), recording_dir: "C:\\Users\\test\\Videos\\Relay",
      privacy: VIDEO_PRIVACY, local_only: VIDEO_LOCAL_ONLY,
    }),
    pick_video_file: () => core.pickedVideo,
    learn_from_file: (a) => {
      const id = String(a.id), path = String(a.path);
      const p = core.profiles.get(id);
      if (!p) throw new Error("no such profile");
      if (/:\/\/|youtube/i.test(path)) throw new Error("Relay learns from video files on this PC only; it does not download from websites");
      if (core.fileJobs.get(id)?.state === "running") throw new Error("Relay is already learning from a video; cancel it first");
      const job: LearnFileStatus = {
        profile: id, exe: p.game.exe.toLowerCase(), file_name: path.split(/[\\/]/).pop() ?? path,
        state: "running", progress: 0, position_secs: 0, duration_secs: 600, speed: null,
        audio_secs: 0, look_frames: 0, notes: [], message: null, privacy: VIDEO_PRIVACY, local_only: VIDEO_LOCAL_ONLY,
      };
      core.fileJobs.set(id, job);
      return structuredClone(job);
    },
    learn_file_status: (a) => {
      const j = core.fileJobs.get(String(a.id));
      return j ? structuredClone(j) : null;
    },
    learn_file_cancel: (a) => {
      const j = core.fileJobs.get(String(a.id));
      if (j && j.state === "running") j.state = "cancelled";
      return j ? structuredClone(j) : null;
    },
    update_status: () => structuredClone(core.update),
    learn_display_status: (a) => structuredClone(learnView(String(a.exe))),
    learn_display_set: (a) => {
      const v = learnView(String(a.exe));
      v.enabled = Boolean(a.enabled);
      if (v.status === "off" && v.enabled) v.status = "learning";
      else if (v.status === "learning" && !v.enabled) v.status = "off";
      return structuredClone(v);
    },
    learn_display_apply: (a) => {
      const v = learnView(String(a.exe));
      const ready = v.monitors.filter((m) => m.converged);
      if (ready.length === 0) throw new Error("this game's look has not settled yet; keep playing");
      for (const m of ready) { m.applied = m.converged; m.use_learned = true; m.status = "applied"; }
      v.status = "applied";
      return structuredClone(v);
    },
    learn_display_auto_apply: (a) => {
      const v = learnView(String(a.exe));
      v.auto_apply = Boolean(a.enabled);
      if (v.auto_apply) {
        for (const m of v.monitors.filter((x) => x.offer)) {
          m.applied = m.offer ?? null; m.adjustments = m.offer_adjustments ?? null;
          m.offer = null; m.offer_adjustments = null; m.use_learned = true; m.status = "applied";
          v.status = "applied";
        }
      }
      return structuredClone(v);
    },
    learn_display_relearn: (a) => {
      const v = learnView(String(a.exe));
      for (const m of v.monitors) { m.converged = null; m.phase = "learning"; m.readiness.frames = 0; m.readiness.progress = 0; }
      return structuredClone(v);
    },
    learn_display_reset: (a) => {
      core.learn.delete(String(a.exe).toLowerCase());
      return structuredClone(learnView(String(a.exe)));
    },
    learn_display_export: (a) => JSON.stringify({
      format: "relay-game-display", version: 1, game: { exe: String(a.exe).toLowerCase() },
      look: { shadow: 0.3, saturation: 0.1, highlight: 0 }, evidence: { frames: 900, scenes: 3 },
      note: String(a.note ?? ""),
    }),
    learn_display_import: (a) => {
      const f = JSON.parse(String(a.json)) as { format?: string; game?: { exe?: string }; look?: LookTargets; note?: string };
      if (f.format !== "relay-game-display" || !f.look) throw new Error("this is not a Relay game display file");
      if (f.game?.exe?.toLowerCase() !== String(a.exe).toLowerCase()) throw new Error(`this file is for ${f.game?.exe}, not ${a.exe}`);
      const v = learnView(String(a.exe));
      v.imported = { look: f.look, note: f.note ?? "" };
      v.enabled = false;
      v.status = "applied_imported";
      return structuredClone(v);
    },
    check_for_updates: () => {
      core.update.last_check = 1_790_000_000;
      return structuredClone(core.update);
    },
    install_update: () => {
      if (core.update.available) core.update.phase = "downloading";
      return structuredClone(core.update);
    },
    update_later: () => {
      core.update.available = null;
      return structuredClone(core.update);
    },
    skip_update: (a) => {
      core.skippedUpdate = a.version as string;
      core.update.available = null;
      return structuredClone(core.update);
    },
    // A core is already answering, so there is nothing to launch.
    start_core: () => false,
    start_share: (a) => {
      const req = a.request as { peer?: string | null };
      core.state.sharing = { kind: "sharing", peer: req.peer ?? "peer" };
    },
    stop_share: () => void (core.state.sharing = { kind: "off" }),
    start_share_preset: (a) => {
      // As the service does: a peer id resolves to a remembered PC, and an
      // unknown id or a missing code is refused before anything starts.
      const peerId = a.peerId as string | null | undefined;
      if (peerId) {
        const p = core.peers.find((x) => x.id === peerId);
        if (!p) throw new Error("That PC is no longer remembered. Pair with its code once and it will be.");
        core.state.sharing = { kind: "sharing", peer: p.name };
        return;
      }
      if (!String(a.code ?? "").trim()) {
        throw new Error("Enter the six-digit code shown on the receiving PC.");
      }
      core.state.sharing = { kind: "sharing", peer: (a.peer as string | null) ?? "living-room-pc" };
    },
    record: () => undefined,
    save_replay: () => undefined,
    switch_source: () => undefined,
    set_mixer: () => undefined,
    list_audio_devices: () => structuredClone(core.audioDevices),
    set_audio_device: (a) => {
      // As the service does: saved in settings, whether or not anything runs.
      const key = devicePrefKey(a.side as MixerSide, a.track as DeviceTrack);
      if (!key) throw new Error("a receiver has no microphone to choose");
      core.prefs = { ...core.prefs, audio_devices: { ...core.prefs.audio_devices, [key]: (a.device as string | null) ?? undefined } };
    },
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
    set_video_area: () => undefined,
    set_stream_mode: () => undefined,
    stream_status: () => structuredClone(core.stream),
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
    set_listening_devices: (a) => {
      const c = core.hardware.connected;
      const devices = a.devices as ListeningDevice[];
      const rest = (c.listening ?? []).filter((l) => l.endpoint !== a.endpoint);
      c.listening = devices.length ? [...rest, { endpoint: a.endpoint as string, devices: structuredClone(devices), active: null }] : rest;
    },
    set_active_listening: (a) => {
      const l = (core.hardware.connected.listening ?? []).find((x) => x.endpoint === a.endpoint);
      if (!l) throw new Error("nothing is listed for that output yet");
      l.active = structuredClone(a.device as ListeningDevice);
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
    firewall_status: () => structuredClone(core.firewall),
    apo_status: () => structuredClone(core.apo),
    audio_effects_status: () => structuredClone(core.audioEffects),
    // The two halves of the S6 flow. `elevation_plan` is read-only and is
    // what the user reads *before* Windows asks; `run_elevated` is the only
    // thing that changes the machine.
    elevation_plan: (a) => {
      const cam = "HKLM\SOFTWARE\Classes\CLSID\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}";
      const tail = ["", "Windows will ask for permission before any of this happens. Decline and nothing on this PC changes."];
      const op = a.op as ElevatedOp;
      const ep = opEndpoint(op) ?? DAC;
      switch (opKind(op)) {
        case "install_apo":
          return [
            "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\\Render\\" + ep + "\\FxProperties :: {d04e05a6-594b-4fb6-a80d-01af5eed7d1d},15",
            "HKLM\SOFTWARE\Classes\CLSID\{5A8E9C3B-1F6D-4B0A-9C41-7E2D83A6F0B4}",
            "backup: %LOCALAPPDATA%\Relay\apo-backup\\" + ep + ".json (written before anything is changed)",
            ...tail,
          ];
        case "install_camera":
          return [cam, cam + "\InprocServer32", ...tail];
        case "uninstall_apo":
          return ["[x] Restore the endpoint audio chain — " + ep + " (needs admin)", ...tail];
        case "set_audio_effects_allowed": {
          const { on, restart_audio } = (op as { set_audio_effects_allowed: { on: boolean; restart_audio: boolean } }).set_audio_effects_allowed;
          const target = "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Audio :: DisableProtectedAudioDG";
          return [
            on ? target + ": absent → 1 (REG_DWORD)" : target + ": 1 → absent (the state before Relay)",
            restart_audio
              ? "Then Windows audio restarts (AudioEndpointBuilder and Audiosrv): sound cuts out for 2–3 seconds."
              : "Takes effect after Windows audio restarts or the PC restarts.",
            ...tail,
          ];
        }
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
      if (opKind(op) === "install_apo") setApo(core, opEndpoint(op) ?? DAC, true);
      if (opKind(op) === "uninstall_apo") setApo(core, opEndpoint(op), false);
      if (op === "install_camera") core.vdevice.camera_registered = true;
      if (opKind(op) === "set_audio_effects_allowed") {
        const on = (op as { set_audio_effects_allowed: { on: boolean } }).set_audio_effects_allowed.on;
        core.audioEffects = on
          ? { value: 1, allowed: true, changed_by_relay: true, prior: null, set_elsewhere: false, unknown: false }
          : { value: null, allowed: false, changed_by_relay: false, set_elsewhere: false, unknown: false };
      }
      if (op === "uninstall_camera") core.vdevice.camera_registered = false;
      if (op === "allow_firewall") {
        core.firewall = {
          ...core.firewall, state: "allowed", rule_present: true, blocking_rules: 0,
        };
      }
      if (op === "remove_firewall") {
        core.firewall = {
          ...core.firewall, state: "will_prompt", rule_present: false, blocking_rules: 0,
        };
      }
      return { declined: false, ok: true, lines: [`${op}: done.`] };
    },
    install_apo: (a) => {
      setApo(core, (a.endpoint as string | null) ?? DAC, true);
    },
    uninstall_apo: (a) => {
      setApo(core, (a.endpoint as string | null) ?? null, false);
    },
    vdevice_status: () => structuredClone(core.vdevice),
    set_vdevice_consent: (a) => {
      core.vdevice.consent = {
        decided_at: "2026-09-14T00:00:00Z",
        apo: a.apo as boolean, camera: a.camera as boolean, microphone: a.microphone as boolean,
      };
    },
    vdevice_dry_run: () => core.vdevice.camera_path === "direct_show" ? [
      "HKCU\\Software\\Classes\\CLSID\\{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}",
      "HKCU\\Software\\Classes\\CLSID\\{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}\\InprocServer32",
      "HKCU\\Software\\Classes\\CLSID\\{860BB310-5D01-11D0-BD3B-00A0C911CE86}\\Instance\\{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}",
      "file: <install dir>\\relay_vdevice.dll (stays in place; only registered)",
    ] : [
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
    list_peers: () => structuredClone(core.peers),
    forget_peer: (a) => {
      const before = core.peers.length;
      core.peers = core.peers.filter((p) => p.id !== a.id);
      if (core.peers.length === before) throw new Error("no remembered PC with that id");
    },
    set_peer_favourite: (a) => {
      const p = core.peers.find((x) => x.id === a.id);
      if (!p) throw new Error("no remembered PC with that id");
      p.favourite = !!a.favourite;
    },
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
  "get_ui_prefs", "set_ui_prefs", "ack_crash", "start_core",
  "update_status", "check_for_updates", "install_update", "update_later", "skip_update",
  "game_eq",
  "list_learn_videos", "pick_video_file", "learn_from_file", "learn_file_status", "learn_file_cancel",
  "learn_display_status", "learn_display_set", "learn_display_apply", "learn_display_relearn", "learn_display_auto_apply",
  "learn_display_reset", "learn_display_export", "learn_display_import",
  "start_share", "stop_share", "start_share_preset", "record", "save_replay",
  "switch_source", "set_mixer", "list_audio_devices", "set_audio_device", "list_presets", "save_preset", "delete_preset",
  "set_recording_settings", "start_receive", "stop_receive", "set_video_area",
  "set_stream_mode", "stream_status", "list_hardware",
  "save_hardware", "delete_hardware", "set_listening_devices", "set_active_listening", "probe_hardware", "import_curve",
  "render_preview", "share_capabilities", "firewall_status",
  "apo_status", "audio_effects_status", "install_apo", "uninstall_apo",
  "elevation_plan", "run_elevated",
  "vdevice_status", "set_vdevice_consent", "vdevice_dry_run", "install_vcam",
  "uninstall_vcam", "search_catalog", "add_headset_from_catalog", "uninstall_plan",
  "launch_uninstaller", "discover_receivers",
  "list_peers", "forget_peer", "set_peer_favourite",
];
