/** One parsed Server-Sent Events frame: `event:` defaults to `"message"` per
 * the SSE spec when the server omits it (as `agents:stream`'s `data:`-only
 * frames do); `data` is the concatenation of every `data:` line, newline-joined. */
export interface SseFrame {
  event: string;
  data: string;
}

/** Parse a `text/event-stream` body into frames, one per blank-line-terminated
 * block. Minimal by design — only what `agents:stream` actually emits (no
 * `id:`/`retry:` support, no reconnection): named events split unnamed
 * `data:` frames from the terminal `result`/`error` one. */
export async function* parseSse(body: ReadableStream<Uint8Array>): AsyncGenerator<SseFrame> {
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
        const frame = parseBlock(block);
        if (frame) yield frame;
      }
    }
    const trailing = buffer.trim();
    if (trailing.length > 0) {
      const frame = parseBlock(trailing);
      if (frame) yield frame;
    }
  } finally {
    reader.releaseLock();
  }
}

function parseBlock(block: string): SseFrame | undefined {
  let event = "message";
  const dataLines: string[] = [];
  for (const line of block.split("\n")) {
    if (line.startsWith("event:")) {
      event = line.slice("event:".length).trim();
    } else if (line.startsWith("data:")) {
      dataLines.push(line.slice("data:".length).trimStart());
    }
  }
  if (dataLines.length === 0) return undefined;
  return { event, data: dataLines.join("\n") };
}
