/** The `{ error: { code, message, type, status, request_id } }` envelope every
 * Apex API error response carries (see docs/09-api/overview.md §8). */
export interface ApexErrorBody {
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
export class ApexApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string | undefined;
  readonly body: ApexErrorBody | undefined;

  constructor(status: number, body: ApexErrorBody | undefined, rawText: string) {
    super(body?.message ?? `Apex API request failed with status ${status}: ${rawText}`);
    this.name = "ApexApiError";
    this.status = status;
    this.code = body?.code ?? "unknown_error";
    this.requestId = body?.request_id;
    this.body = body;
  }
}
