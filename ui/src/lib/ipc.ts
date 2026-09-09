/**
 * TypeScript mirror of `crates/core/src/ipc.rs` + `types.rs`.
 * Keep field names and enum spellings identical to the serde output.
 *
 * In the browser (plain `pnpm dev` without Tauri) the commands are not
 * available, so every call falls back to mock data shaped like the mocks/.
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
export interface Foreground { pid: number; exe: string; title: string }
export type ShareState = { kind: "off" } | { kind: "sharing"; peer: string };
export type AudioChainState = "bypass" | "active" | "exclusivebypassed";
export type DisplayState = "default" | "applied";
export interface Footprint { rss_bytes: number; cpu_percent: number }
export interface CoreState {
  active_profile: ProfileSummary | null; foreground: Foreground | null;
  sharing: ShareState; audio_chain: AudioChainState; display_state: DisplayState; footprint: Footprint;
}

export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export const mockState: CoreState = {
  active_profile: null,
  foreground: null,
  sharing: { kind: "off" },
  audio_chain: "bypass",
  display_state: "default",
  footprint: { rss_bytes: 9 * 1024 * 1024, cpu_percent: 0 },
};

export const mockProfiles: ProfileSummary[] = [
  { id: "1", name: "Call of Duty", note: "Footsteps · dark-map colors", exe: "cod.exe", headset: "HD 560S", monitor: "LG 27GP850", share: "game", status: "ready" },
  { id: "2", name: "Call of Duty", note: "Same tuning · IEM set", exe: "cod.exe", headset: "Moondrop Blessing 3", monitor: "LG 27GP850", share: "game", status: "ready" },
  { id: "3", name: "Valorant", note: "Neutral EQ · vivid", exe: "valorant.exe", headset: "HD 560S", monitor: "Zowie XL2566K", share: "game", status: "ready" },
  { id: "4", name: "FL Studio", note: "Relay Send on master", exe: "fl64.exe", headset: "Scarlett 2i2 → DT 770", monitor: null, share: "daw", status: "ready" },
  { id: "5", name: "Elden Ring", note: "Untouched audio · warm colors", exe: "eldenring.exe", headset: null, monitor: "LG C2", share: "off", status: "draft" },
];

export const api = {
  async status(): Promise<CoreState> {
    if (!isTauri()) return mockState;
    return invoke<CoreState>("core_status");
  },
  async listProfiles(): Promise<ProfileSummary[]> {
    if (!isTauri()) return mockProfiles;
    return invoke<ProfileSummary[]>("list_profiles");
  },
  async getProfile(id: string): Promise<Profile> {
    return invoke<Profile>("get_profile", { id });
  },
  async saveProfile(profile: Profile): Promise<void> {
    return invoke<void>("save_profile", { profile });
  },
  async applyProfile(id: string): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("apply_profile", { id });
  },
  async restoreAll(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("restore_all");
  },
};

/** Subscribe to pushed core events. Returns an unsubscribe fn. */
export async function onCoreEvents(handlers: {
  state?: (s: CoreState) => void;
  notice?: (text: string) => void;
  offline?: () => void;
}): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const unlisteners = await Promise.all([
    listen<CoreState>("core://state", (e) => handlers.state?.(e.payload)),
    listen<string>("core://notice", (e) => handlers.notice?.(e.payload)),
    listen<void>("core://offline", () => handlers.offline?.()),
  ]);
  return () => unlisteners.forEach((u) => u());
}

export const fmtMb = (bytes: number) => (bytes / (1024 * 1024)).toFixed(0);
