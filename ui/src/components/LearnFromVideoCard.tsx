import { useEffect, useState } from "react";
import { Card, ErrorNote, Kv } from "./Controls";
import { errText } from "../lib/err";
import { clockText } from "../lib/eta";
import {
  api, VIDEO_LOCAL_ONLY, VIDEO_PRIVACY,
  type LearnFileStatus, type LearnVideos, type VideoFile,
} from "../lib/ipc";

/** How often the card re-reads a running job. */
export const VIDEO_POLL_MS = 1000;

function sizeText(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

/**
 * S48: "Learn faster: use a recording". Instead of waiting for enough live
 * play, Relay can learn a game's sound and look from a local gameplay video,
 * decoded faster than real time by an on-demand helper. The evidence goes
 * into the same per-game records as live play, so the result is offered the
 * usual way and your own play keeps refining it. Local files only.
 */
export function LearnFromVideoCard({ profileId }: { profileId: string | null }) {
  const [job, setJob] = useState<LearnFileStatus | null>(null);
  const [list, setList] = useState<LearnVideos | null>(null);
  const [choosing, setChoosing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setJob(null); setChoosing(false); setErr(null);
    if (!profileId) return;
    let live = true;
    api.learnFileStatus(profileId).then((j) => { if (live) setJob(j); }).catch(() => {});
    return () => { live = false; };
  }, [profileId]);

  const running = job?.state === "running";
  useEffect(() => {
    if (!profileId || !running) return;
    const t = setInterval(() => {
      api.learnFileStatus(profileId).then(setJob).catch(() => {});
    }, VIDEO_POLL_MS);
    return () => clearInterval(t);
  }, [profileId, running]);

  const open = async () => {
    setErr(null);
    setChoosing(true);
    try { setList(await api.listLearnVideos()); } catch (e) { setErr(errText(e)); }
  };

  const start = async (path: string | null) => {
    if (!profileId || !path) return;
    setErr(null); setBusy(true);
    try {
      setJob(await api.learnFromFile(profileId, path));
      setChoosing(false);
    } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };

  const browse = async () => {
    setErr(null);
    try { await start(await api.pickVideoFile()); } catch (e) { setErr(errText(e)); }
  };

  const cancel = async () => {
    if (!profileId) return;
    try { setJob(await api.learnFileCancel(profileId)); } catch (e) { setErr(errText(e)); }
  };

  const pct = job?.progress === null || job?.progress === undefined ? null : Math.round(job.progress * 100);
  const relay = list?.videos.filter((v) => v.relay) ?? [];
  const other = list?.videos.filter((v) => !v.relay) ?? [];
  return (
    <Card title="Learn faster: use a recording">
      {!profileId ? <p className="p">No profile selected.</p> : (
        <>
          <p className="p">
            Instead of waiting for enough play, Relay can learn this game's sound and look from a
            gameplay video on this PC, faster than real time. The result is offered the same way —
            Apply, or applied automatically if you allow it — and your own play keeps refining it.
          </p>
          <p className="note" data-testid="video-what">
            What is analysed: the game's sound (footsteps, reloads, gunfire, explosions, voices, music)
            and one frame every half second of the picture (brightness, shadows, colour). Menus,
            loading screens, letterboxed cutscenes and commentary over the game are left out.
          </p>
          {job && (
            <div data-testid="video-job">
              <Kv k="Video" v={job.file_name} />
              {job.state === "running" && (
                <>
                  <div className="meter" role="progressbar" aria-label="Video learning progress"
                    aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct ?? undefined}>
                    <i style={{ width: `${pct ?? 0}%` }} />
                  </div>
                  <Kv k="Progress" mono v={`${clockText(job.position_secs)}${job.duration_secs ? ` of ${clockText(job.duration_secs)}` : ""}${pct !== null ? ` · ${pct}%` : ""}${job.speed ? ` · ${job.speed.toFixed(0)}× real time` : ""}`} />
                  <button type="button" className="btn q" onClick={() => void cancel()}>Cancel</button>
                </>
              )}
              {job.state === "done" && (
                <p className="note" role="status" data-testid="video-done">
                  Learned from {clockText(job.audio_secs)} of sound and {job.look_frames} frames
                  {job.look_gameplay_frames !== undefined ? ` (${job.look_gameplay_frames} of gameplay)` : ""}
                  {job.speed ? `, ${job.speed.toFixed(0)}× faster than real time` : ""}.
                  {" "}Anything ready is offered on the game's sound and look cards. Learning stays on so your play refines it.
                </p>
              )}
              {job.state === "cancelled" && (
                <p className="note" role="status" data-testid="video-cancelled">Cancelled. Nothing from the file was kept.</p>
              )}
              {job.state === "failed" && <ErrorNote text={job.message ?? "Learning from the video failed."} />}
              {job.notes?.map((n) => <p key={n} className="note">{n}</p>)}
            </div>
          )}
          {!running && !choosing && (
            <button type="button" className="btn acc" disabled={busy} onClick={() => void open()}>
              Learn from a video file…
            </button>
          )}
          {choosing && !running && (
            <div role="group" aria-label="Choose a video" className="goals">
              {list === null ? <p className="p small">Reading…</p> : (
                <>
                  {relay.length > 0 && <p className="p small"><b>Your Relay recordings</b></p>}
                  {relay.map((v) => <VideoButton key={v.path} v={v} busy={busy} onPick={() => void start(v.path)} />)}
                  {other.length > 0 && <p className="p small"><b>Other videos</b></p>}
                  {other.map((v) => <VideoButton key={v.path} v={v} busy={busy} onPick={() => void start(v.path)} />)}
                  {list.videos.length === 0 && (
                    <p className="p small">No videos in {list.recording_dir} or your Videos folder.</p>
                  )}
                </>
              )}
              <button type="button" className="btn" disabled={busy} onClick={() => void browse()}>Choose another file…</button>
              <button type="button" className="btn q" onClick={() => setChoosing(false)}>Not now</button>
            </div>
          )}
          <ErrorNote text={err} onDismiss={() => setErr(null)} />
          <p className="note" data-testid="video-local-only">{list?.local_only ?? job?.local_only ?? VIDEO_LOCAL_ONLY}</p>
          <p className="note">{list?.privacy ?? job?.privacy ?? VIDEO_PRIVACY} Nothing on your PC is changed by learning.</p>
        </>
      )}
    </Card>
  );
}

function VideoButton({ v, busy, onPick }: { v: VideoFile; busy: boolean; onPick: () => void }) {
  const when = v.modified_unix ? new Date(v.modified_unix * 1000).toLocaleDateString() : "";
  return (
    <button type="button" className="btn q" disabled={busy} aria-label={`Learn from ${v.name}`} onClick={onPick}>
      <b>{v.name}</b> <small>{sizeText(v.size_bytes)}{when ? ` · ${when}` : ""}{v.relay ? " · Relay recording" : ""}</small>
    </button>
  );
}
