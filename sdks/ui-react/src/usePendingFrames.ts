import { useCallback, useEffect, useRef, useState } from "react";
import type { UiClient } from "./client.js";
import type { PendingUiFrame, UiDecision } from "./types.js";

export interface UsePendingFramesOptions {
  /** Poll interval in ms. Default 2000. Set `0` to disable polling (manual
   * `refresh()` only — e.g. a host driving frames off an SSE stream instead). */
  intervalMs?: number;
}

export interface UsePendingFramesResult {
  frames: PendingUiFrame[];
  /** The most recent poll/decide error, if any (stale data is kept on
   * screen rather than cleared, so a transient network blip doesn't yank a
   * frame the human was about to act on). */
  error: unknown;
  loading: boolean;
  refresh: () => Promise<void>;
  /** Submit a decision for `frameId` and optimistically drop it from
   * `frames` on success (a background `refresh()` reconciles either way). */
  decide: (frameId: string, decision: UiDecision) => Promise<void>;
}

/** Every poll parses a fresh response body, so an unchanged frame still
 * arrives as a brand-new object each cycle. Reusing the previous object
 * reference for anything whose `frame_hash` hasn't changed keeps referential
 * stability across polls — otherwise `UiFrameView`'s integrity-check effect
 * (keyed on the `frame` prop's identity) re-fires on every poll for frames
 * that never actually changed, flashing "Verifying frame integrity…" over
 * already-rendered content. New/changed frames still get their fresh object,
 * so their effect runs exactly when it should. */
function reconcile(prev: PendingUiFrame[], next: PendingUiFrame[]): PendingUiFrame[] {
  const prevByFrameId = new Map(prev.map((f) => [f.frame_id, f]));
  return next.map((f) => {
    const existing = prevByFrameId.get(f.frame_id);
    return existing && existing.frame_hash === f.frame_hash ? existing : f;
  });
}

/** Polls `GET /api/v1/ui/frames` (RDR-104's pull path) and exposes a `decide`
 * that posts to `/api/v1/ui/decisions/{id}`. For a host already consuming an
 * `agents:stream` SSE connection, extract `ui_frame` events directly instead —
 * this hook is the standalone/pull-only path (UIP-104 §2). */
export function usePendingFrames(
  client: UiClient,
  options?: UsePendingFramesOptions,
): UsePendingFramesResult {
  const [frames, setFrames] = useState<PendingUiFrame[]>([]);
  const [error, setError] = useState<unknown>(undefined);
  const [loading, setLoading] = useState(true);
  const intervalMs = options?.intervalMs ?? 2000;
  const clientRef = useRef(client);
  clientRef.current = client;

  const refresh = useCallback(async () => {
    try {
      const data = await clientRef.current.listFrames();
      setFrames((prev) => reconcile(prev, data));
      setError(undefined);
    } catch (err) {
      setError(err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    if (intervalMs <= 0) return;
    const id = setInterval(() => void refresh(), intervalMs);
    return () => clearInterval(id);
  }, [refresh, intervalMs]);

  const decide = useCallback(
    async (frameId: string, decision: UiDecision) => {
      await clientRef.current.decide(frameId, decision);
      setFrames((prev) => prev.filter((f) => f.frame_id !== frameId));
      void refresh();
    },
    [refresh],
  );

  return { frames, error, loading, refresh, decide };
}
