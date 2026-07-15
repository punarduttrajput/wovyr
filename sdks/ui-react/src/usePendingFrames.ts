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
      setFrames(data);
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
