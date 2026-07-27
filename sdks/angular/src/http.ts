import { HttpClient, HttpHeaders, HttpParams } from "@angular/common/http";
import { Observable, throwError } from "rxjs";
import { catchError, map } from "rxjs/operators";
import { WovyrApiError, type WovyrErrorBody } from "./errors.js";
import type { WovyrClientOptions } from "./types.js";

interface RequestOpts {
  query?: object;
  headers?: Record<string, string>;
  /** Response type for non-JSON bodies (e.g. the SSE stream). */
  responseType?: "json" | "text" | "blob" | "arraybuffer";
}

/** Thin wrapper over Angular's `HttpClient` that injects the tenant/principal
 * headers from {@link WovyrClientOptions} and normalizes non-2xx responses
 * into {@link WovyrApiError} — mirroring `@wovyr/sdk`'s `HttpClient` so the two
 * SDKs share identical error semantics. */
export class WovyrHttpClient {
  private readonly http: HttpClient;
  private readonly baseUrl: string;
  private readonly defaultHeaders: Record<string, string>;

  constructor(options: WovyrClientOptions) {
    this.http = options.http;
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.defaultHeaders = {
      "X-Wovyr-Tenant": options.tenant ?? "default",
    };
    if (options.principal) this.defaultHeaders["X-Wovyr-Principal"] = options.principal;
  }

  request<T>(method: string, path: string, body?: unknown, opts: RequestOpts = {}): Observable<T> {
    const url = `${this.baseUrl}${path}`;
    const headers = new HttpHeaders({ ...this.defaultHeaders, ...(opts.headers ?? {}) });
    const params = toHttpParams(opts.query);

    return this.http
      .request(method, url, {
        body,
        headers,
        params,
        responseType: (opts.responseType ?? "json") as "json",
        observe: "response",
      })
      .pipe(
        map((res) => res.body as T),
        catchError((err) => throwError(() => normalizeError(err))),
      );
  }

  /** Raw response access (used by SSE streaming, which the SDK does not
   * bundle — callers wanting streams should use `@wovyr/sdk`). */
  raw(method: string, path: string, body?: unknown, opts: RequestOpts = {}) {
    const url = `${this.baseUrl}${path}`;
    const headers = new HttpHeaders({ ...this.defaultHeaders, ...(opts.headers ?? {}) });
    const params = toHttpParams(opts.query);
    return this.http.request(method, url, {
      body,
      headers,
      params,
      observe: "response",
      responseType: "text",
    });
  }
}

function toHttpParams(query?: object): HttpParams {
  let params = new HttpParams();
  if (!query) return params;
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null) continue;
    params = params.set(key, String(value));
  }
  return params;
}

function normalizeError(err: unknown): unknown {
  // Angular's HttpErrorResponse carries the parsed body on `.error`.
  const body = (err as { error?: unknown })?.error;
  if (body && typeof body === "object" && "code" in body && "message" in body) {
    const b = body as WovyrErrorBody;
    const status = (err as { status?: number })?.status ?? b.status ?? 0;
    const rawText = typeof body === "string" ? body : JSON.stringify(body);
    return new WovyrApiError(status, b, rawText);
  }
  if (body && typeof body === "string") {
    const status = (err as { status?: number })?.status ?? 0;
    return new WovyrApiError(status, undefined, body);
  }
  return err;
}
