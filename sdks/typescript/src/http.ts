import { ApexApiError, type ApexErrorBody } from "./errors.js";
import type { ApexClientOptions } from "./types.js";

/** Extra per-request knobs layered on top of the client's defaults. `query` is
 * typed as a bare `object` (rather than an indexed `Record`) so any of the
 * resource-specific params interfaces (`PageParams`, `WorkflowListParams`, …)
 * can be passed directly without a structural-typing mismatch against an
 * index signature. */
export interface RequestOptions {
  query?: object;
  headers?: Record<string, string>;
}

/** The shared request/response plumbing every resource method calls through:
 * base-URL joining, default tenant/principal headers, query-string building,
 * JSON encode/decode, and error-envelope mapping into {@link ApexApiError}. */
export class HttpClient {
  private readonly baseUrl: string;
  private readonly tenant: string | undefined;
  private readonly principal: string | undefined;
  private readonly fetchImpl: typeof fetch;

  constructor(options: ApexClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.tenant = options.tenant;
    this.principal = options.principal;
    this.fetchImpl = options.fetchImpl ?? fetch;
  }

  private url(path: string, query?: RequestOptions["query"]): string {
    const url = new URL(this.baseUrl + path);
    for (const [key, value] of Object.entries(query ?? {}) as Array<[string, unknown]>) {
      if (value !== undefined && value !== null) url.searchParams.set(key, String(value));
    }
    return url.toString();
  }

  private defaultHeaders(extra?: Record<string, string>): Record<string, string> {
    const headers: Record<string, string> = { ...extra };
    if (this.tenant !== undefined) headers["X-Apex-Tenant"] = this.tenant;
    if (this.principal !== undefined) headers["X-Apex-Principal"] = this.principal;
    return headers;
  }

  /** Perform a request and return the raw `Response` (used by the SSE helper,
   * which needs the body stream rather than a parsed value). */
  async raw(method: string, path: string, body?: unknown, options?: RequestOptions): Promise<Response> {
    const headers = this.defaultHeaders(options?.headers);
    const init: RequestInit = { method, headers };
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    return this.fetchImpl(this.url(path, options?.query), init);
  }

  /** Perform a request and decode the JSON body, throwing {@link ApexApiError}
   * on any non-2xx status. `T` is `void` for `204 No Content` responses. */
  async request<T>(method: string, path: string, body?: unknown, options?: RequestOptions): Promise<T> {
    const response = await this.raw(method, path, body, options);
    if (response.status === 204) {
      return undefined as T;
    }
    const text = await response.text();
    if (!response.ok) {
      let errorBody: ApexErrorBody | undefined;
      try {
        const parsed = JSON.parse(text) as { error?: ApexErrorBody };
        errorBody = parsed.error;
      } catch {
        // Non-JSON error body (e.g. a proxy in front of the server) — fall
        // back to the raw text captured in ApexApiError.message.
      }
      throw new ApexApiError(response.status, errorBody, text);
    }
    if (text.length === 0) {
      return undefined as T;
    }
    return JSON.parse(text) as T;
  }

  /** Response headers from the most recent call, for endpoints that carry
   * state in headers rather than the body (`ETag` on projects). */
  async requestWithHeaders<T>(
    method: string,
    path: string,
    body?: unknown,
    options?: RequestOptions,
  ): Promise<{ data: T; headers: Headers }> {
    const response = await this.raw(method, path, body, options);
    const text = await response.text();
    if (!response.ok) {
      let errorBody: ApexErrorBody | undefined;
      try {
        errorBody = (JSON.parse(text) as { error?: ApexErrorBody }).error;
      } catch {
        // see request()
      }
      throw new ApexApiError(response.status, errorBody, text);
    }
    return { data: (text.length ? JSON.parse(text) : undefined) as T, headers: response.headers };
  }
}
