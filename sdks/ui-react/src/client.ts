import type { PendingUiFrame, UiDecision } from "./types.js";

/** Options for {@link createUiClient}. Deliberately standalone (no dependency
 * on `@apex-ai/sdk`) — the renderer is meant to be embeddable in *any* host
 * (PRD-005 EMB-701), including one that never adopts the rest of the
 * platform's TypeScript SDK. */
export interface UiClientOptions {
  /** Base URL of a running `apex-server` (e.g. `http://127.0.0.1:8080`). */
  baseUrl: string;
  /** Sent as `X-Apex-Tenant`. Defaults to `"default"`. */
  tenant?: string;
  /** Sent as `X-Apex-Principal`, if set. */
  principal?: string;
  /** `fetch` override, mainly for tests. Defaults to the global `fetch`. */
  fetchImpl?: typeof fetch;
}

/** Thrown on any non-2xx response from the three ui routes. Mirrors the
 * shape of `@apex-ai/sdk`'s `ApexApiError` closely enough to switch on
 * `.status`, without importing that package. */
export class UiApiError extends Error {
  readonly status: number;
  readonly code?: string;

  constructor(status: number, body: unknown, rawText: string) {
    const parsed = (body ?? {}) as { code?: string; message?: string };
    super(parsed.message ?? (rawText || `request failed with status ${status}`));
    this.name = "UiApiError";
    this.status = status;
    this.code = parsed.code;
  }
}

/** A minimal client over the three generative-UI routes (PRD-005 RM-GUI-P1):
 * pull pending frames, and post typed decisions. This is deliberately a thin
 * fetch wrapper, not a general Apex API client — reach for `@apex-ai/sdk`'s
 * `ui` resource instead if the host already depends on it. */
export interface UiClient {
  listFrames(): Promise<PendingUiFrame[]>;
  getFrame(frameId: string): Promise<PendingUiFrame>;
  decide(frameId: string, decision: UiDecision): Promise<{ status: "decided" }>;
}

export function createUiClient(options: UiClientOptions): UiClient {
  const baseUrl = options.baseUrl.replace(/\/+$/, "");
  const fetchImpl = options.fetchImpl ?? fetch;

  function headers(extra?: Record<string, string>): Record<string, string> {
    const h: Record<string, string> = { ...extra };
    if (options.tenant !== undefined) h["X-Apex-Tenant"] = options.tenant;
    if (options.principal !== undefined) h["X-Apex-Principal"] = options.principal;
    return h;
  }

  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const h = headers();
    const init: RequestInit = { method, headers: h };
    if (body !== undefined) {
      h["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    const response = await fetchImpl(`${baseUrl}${path}`, init);
    const text = await response.text();
    if (!response.ok) {
      let parsed: unknown;
      try {
        parsed = (JSON.parse(text) as { error?: unknown }).error;
      } catch {
        // Non-JSON error body (e.g. a proxy in front of the server).
      }
      throw new UiApiError(response.status, parsed, text);
    }
    return text.length ? (JSON.parse(text) as T) : (undefined as T);
  }

  return {
    async listFrames() {
      const { data } = await request<{ data: PendingUiFrame[] }>("GET", "/api/v1/ui/frames");
      return data;
    },
    async getFrame(frameId: string) {
      return request<PendingUiFrame>("GET", `/api/v1/ui/frames/${encodeURIComponent(frameId)}`);
    },
    async decide(frameId: string, decision: UiDecision) {
      return request<{ status: "decided" }>(
        "POST",
        `/api/v1/ui/decisions/${encodeURIComponent(frameId)}`,
        decision,
      );
    },
  };
}
