/** The `{ error: { code, message, type, status, request_id } }` envelope every
 * Wovyr API error response carries (see docs/09-api/overview.md §8). */
export interface WovyrErrorBody {
  code: string;
  message: string;
  type: "client_error" | "server_error";
  status: number;
  request_id: string;
}

/** Thrown for any non-2xx response. Carries the parsed error envelope when the
 * server returned one (it always does, for JSON endpoints); falls back to the
 * raw response text otherwise (e.g. a proxy/network failure upstream of the
 * server). */
export class WovyrApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string | undefined;
  readonly body: WovyrErrorBody | undefined;

  constructor(status: number, body: WovyrErrorBody | undefined, rawText: string) {
    super(body?.message ?? `Wovyr API request failed with status ${status}: ${rawText}`);
    this.name = "WovyrApiError";
    this.status = status;
    this.code = body?.code ?? "unknown_error";
    this.requestId = body?.request_id;
    this.body = body;
  }
}

/** Thrown when a client-side wait (e.g.
 * `workflows.waitForCompletion`) exhausts its timeout before the awaited
 * condition holds. Not an API error — the server never rejected anything; the
 * caller's deadline simply passed. */
export class WovyrTimeoutError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WovyrTimeoutError";
  }
}
