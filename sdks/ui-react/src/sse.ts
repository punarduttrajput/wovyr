import type { UiFrame } from "./types.js";

/** A `ui_frame` event extracted from an `agents:stream` SSE body (UIP-104):
 * `{"type":"ui_frame","frame_id":"...","frame":{...}}`. */
export interface UiFrameSseEvent {
  frame_id: string;
  frame: UiFrame;
}

/** Extracts `ui_frame` events from a raw `text/event-stream` body — the
 * secondary consumption path (RDR-401 §2) for a host already driving an
 * agent via `POST /api/v1/agents:stream` rather than the pull-based
 * {@link usePendingFrames}. Deliberately minimal (no `id:`/`retry:`, no
 * reconnection — matching what that endpoint actually emits), self-contained
 * so this package has no dependency on `@apex-ai/sdk`. Non-`ui_frame` events
 * in the stream are silently skipped; a malformed `ui_frame` payload (bad
 * JSON, missing fields) is dropped rather than thrown — a stream consumer
 * shouldn't crash on one bad frame. */
export async function* extractUiFrames(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<UiFrameSseEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let boundary: number;
      while ((boundary = buffer.indexOf("\n\n")) !== -1) {
        const block = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        const event = parseBlock(block);
        if (event) yield event;
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function parseBlock(block: string): UiFrameSseEvent | undefined {
  const dataLines: string[] = [];
  for (const line of block.split("\n")) {
    if (line.startsWith("data:")) dataLines.push(line.slice("data:".length).trimStart());
  }
  if (dataLines.length === 0) return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(dataLines.join("\n"));
  } catch {
    return undefined;
  }
  const candidate = parsed as { type?: string; frame_id?: string; frame?: unknown };
  if (candidate.type !== "ui_frame" || !candidate.frame_id || !candidate.frame) {
    return undefined;
  }
  return { frame_id: candidate.frame_id, frame: candidate.frame as UiFrame };
}
