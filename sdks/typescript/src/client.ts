import { HttpClient } from "./http.js";
import { parseSse } from "./sse.js";
import type {
  AgentStreamEvent,
  AgentSummary,
  ApexClientOptions,
  Attestation,
  AuditEntry,
  AuditPage,
  MarketplaceSearchParams,
  MemoryQueryResult,
  Page,
  PageParams,
  PluginSummary,
  PendingUiFrame,
  PublishResult,
  PutMemoryRequest,
  QueryMemoryRequest,
  Role,
  RunRequest,
  RunResult,
  ScanReport,
  SecretMetadata,
  SubmitWorkflowRequest,
  ToolSummary,
  UiDecisionRequest,
  UiDecisionResult,
  WorkflowListParams,
  WorkflowValidation,
} from "./types.js";

function base64Encode(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64");
}

function base64Decode(b64: string): Uint8Array {
  return new Uint8Array(Buffer.from(b64, "base64"));
}

/** Every mutating resource method below takes this as its trailing `opts` (or a
 * superset adding its own headers, e.g. `project`). RM-GA-P4 API-703: the server
 * now honors `Idempotency-Key` on every mutating route, not just `agents:run` — a
 * retry with the same key replays the original response instead of re-executing.
 * Not offered on `workflows.validate` (parse-only, no side effects) or
 * `memory.query` (a read), since neither is a mutation to protect. */
interface IdempotentOpts {
  idempotencyKey?: string;
}

/** A client for one Apex `apex-server` instance, scoped to one tenant/principal
 * (construct a new client per tenant to act as a different one). Mirrors the
 * server's actual routes (see `docs/09-api/openapi.yaml`) — not the aspirational
 * conventions in `overview.md` (no opaque `agt_...` ids, no OAuth2; resources
 * are addressed by their natural key and auth is the `X-Apex-Tenant`/
 * `X-Apex-Principal` headers). */
export class ApexClient {
  private readonly http: HttpClient;

  readonly agents: AgentsResource;
  readonly workflows: WorkflowsResource;
  readonly memory: MemoryResource;
  readonly plugins: PluginsResource;
  readonly marketplace: MarketplaceResource;
  readonly secrets: SecretsResource;
  readonly organizations: OrganizationsResource;
  readonly projects: ProjectsResource;
  readonly webhooks: WebhooksResource;
  readonly audit: AuditResource;
  readonly tools: ToolsResource;
  readonly ui: UiResource;

  constructor(options: ApexClientOptions) {
    this.http = new HttpClient(options);
    this.agents = new AgentsResource(this.http);
    this.workflows = new WorkflowsResource(this.http);
    this.memory = new MemoryResource(this.http);
    this.plugins = new PluginsResource(this.http);
    this.marketplace = new MarketplaceResource(this.http);
    this.secrets = new SecretsResource(this.http);
    this.organizations = new OrganizationsResource(this.http);
    this.projects = new ProjectsResource(this.http);
    this.webhooks = new WebhooksResource(this.http);
    this.audit = new AuditResource(this.http);
    this.tools = new ToolsResource(this.http);
    this.ui = new UiResource(this.http);
  }

  /** `GET /healthz`. */
  async health(): Promise<{ status: string; version: string }> {
    return this.http.request("GET", "/healthz");
  }
}

/** Build the `Idempotency-Key` header entry (or `{}`) from an {@link IdempotentOpts}. */
function idemHeaders(opts?: IdempotentOpts): Record<string, string> {
  return opts?.idempotencyKey ? { "Idempotency-Key": opts.idempotencyKey } : {};
}

class AgentsResource {
  constructor(private readonly http: HttpClient) {}

  /** `POST /api/v1/agents:run` — run an inline manifest, no persistence. */
  async run(req: RunRequest, opts?: IdempotentOpts & { project?: string }): Promise<RunResult> {
    const headers: Record<string, string> = { ...idemHeaders(opts) };
    if (opts?.project) headers["X-Apex-Project"] = opts.project;
    return this.http.request("POST", "/api/v1/agents:run", req, { headers });
  }

  /** `POST /api/v1/agents:stream` — run an inline manifest, yielding progress
   * frames as they arrive. The final yielded event is always `result` or
   * `error`. Not idempotency-key eligible: the response is an SSE stream, not
   * a value the server can buffer and replay. */
  async *stream(req: RunRequest, opts?: { project?: string }): AsyncGenerator<AgentStreamEvent> {
    const headers: Record<string, string> = {};
    if (opts?.project) headers["X-Apex-Project"] = opts.project;
    const response = await this.http.raw("POST", "/api/v1/agents:stream", req, { headers });
    if (!response.ok || !response.body) {
      const text = await response.text();
      throw new Error(`agents:stream failed with status ${response.status}: ${text}`);
    }
    for await (const frame of parseSse(response.body)) {
      if (frame.event === "result") {
        yield { type: "result", ...(JSON.parse(frame.data) as RunResult) };
      } else if (frame.event === "error") {
        yield { type: "error", message: frame.data };
      } else {
        yield JSON.parse(frame.data) as AgentStreamEvent;
      }
    }
  }

  /** `GET /api/v1/agents` — list stored agent ids for the caller's tenant. */
  async list(params?: PageParams): Promise<Page<string>> {
    return this.http.request("GET", "/api/v1/agents", undefined, { query: params });
  }

  /** `POST /api/v1/agents` — store an agent manifest. */
  async create(manifest: string, opts?: IdempotentOpts): Promise<{ id: string; status: "created" }> {
    return this.http.request("POST", "/api/v1/agents", { manifest }, { headers: idemHeaders(opts) });
  }

  /** `GET /api/v1/agents/{id}`. */
  async get(id: string): Promise<AgentSummary> {
    return this.http.request("GET", `/api/v1/agents/${encodeURIComponent(id)}`);
  }

  /** `DELETE /api/v1/agents/{id}`. */
  async delete(id: string, opts?: IdempotentOpts): Promise<void> {
    return this.http.request("DELETE", `/api/v1/agents/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  /** `POST /api/v1/agents/{id}/run` — run a stored agent by id. */
  async runStored(
    id: string,
    req?: { input?: unknown; max_steps?: number },
    opts?: IdempotentOpts & { project?: string },
  ): Promise<RunResult> {
    const headers: Record<string, string> = { ...idemHeaders(opts) };
    if (opts?.project) headers["X-Apex-Project"] = opts.project;
    return this.http.request("POST", `/api/v1/agents/${encodeURIComponent(id)}/run`, req ?? {}, { headers });
  }
}

class WorkflowsResource {
  constructor(private readonly http: HttpClient) {}

  /** `POST /api/v1/workflows/validate` — parse-only, no side effects. Not
   * idempotency-key eligible (nothing to protect against double-execution). */
  async validate(manifest: string): Promise<WorkflowValidation> {
    return this.http.request("POST", "/api/v1/workflows/validate", { manifest });
  }

  /** `GET /api/v1/workflows` — list executions visible to the caller's tenant. */
  async list(params?: WorkflowListParams): Promise<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/workflows", undefined, { query: params });
  }

  /** `POST /api/v1/workflows` — durably start + async-drive an execution.
   * Returns immediately; poll {@link get} for completion. */
  async submit(
    req: SubmitWorkflowRequest,
    opts?: IdempotentOpts,
  ): Promise<{ execution_id: string; status: "submitted" }> {
    return this.http.request("POST", "/api/v1/workflows", req, { headers: idemHeaders(opts) });
  }

  /** `GET /api/v1/workflows/{id}` — live status + event history. */
  async get(id: string): Promise<{ execution: Record<string, unknown>; events: Record<string, unknown>[] }> {
    return this.http.request("GET", `/api/v1/workflows/${encodeURIComponent(id)}`);
  }

  /** `DELETE /api/v1/workflows/{id}` — advisory cancel. */
  async cancel(id: string, opts?: IdempotentOpts): Promise<void> {
    return this.http.request("DELETE", `/api/v1/workflows/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  /** `POST /api/v1/workflows/{id}/signal`. */
  async signal(
    id: string,
    req: { manifest: string; event: string; payload?: unknown },
    opts?: IdempotentOpts,
  ): Promise<{ execution_id: string; event: string; status: "signalled" }> {
    return this.http.request("POST", `/api/v1/workflows/${encodeURIComponent(id)}/signal`, req, {
      headers: idemHeaders(opts),
    });
  }

  /** `POST /api/v1/workflows/{id}/approve`. */
  async approve(
    id: string,
    req: { manifest: string; activity_id: string; decision?: unknown },
    opts?: IdempotentOpts,
  ): Promise<{ execution_id: string; activity_id: string; status: "approved" }> {
    return this.http.request("POST", `/api/v1/workflows/${encodeURIComponent(id)}/approve`, req, {
      headers: idemHeaders(opts),
    });
  }
}

class MemoryResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/memory/namespaces`. */
  async namespaces(): Promise<{ namespaces: Array<{ namespace: string; count: number }>; total: number }> {
    return this.http.request("GET", "/api/v1/memory/namespaces");
  }

  /** `GET /api/v1/memory/records`. */
  async listRecords(params?: PageParams & { namespace?: string }): Promise<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/memory/records", undefined, { query: params });
  }

  /** `POST /api/v1/memory/records` — store a memory record. */
  async put(req: PutMemoryRequest, opts?: IdempotentOpts): Promise<{ id: string; status: "stored" }> {
    return this.http.request("POST", "/api/v1/memory/records", req, { headers: idemHeaders(opts) });
  }

  /** `POST /api/v1/memory:query` — hybrid vector+keyword retrieval. Returns a
   * ranked result set, not a page — there is no cursor/`has_more`, since
   * retrieval is bounded by `limit`/relevance, not an offset into a stable
   * ordering (RM-GA-P4 API-701). Not idempotency-key eligible (a read). */
  async query(req: QueryMemoryRequest): Promise<{ data: MemoryQueryResult[]; count: number }> {
    return this.http.request("POST", "/api/v1/memory:query", req);
  }
}

class PluginsResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/plugins`. */
  async list(params?: PageParams): Promise<Page<PluginSummary>> {
    return this.http.request("GET", "/api/v1/plugins", undefined, { query: params });
  }

  /** `POST /api/v1/plugins:install` — `apexpkg` is the raw `.apexpkg` bytes. */
  async install(apexpkg: Uint8Array, grants: string[] = [], opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request(
      "POST",
      "/api/v1/plugins:install",
      { apexpkg: base64Encode(apexpkg), grants },
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/plugins:enable`. */
  async enable(id: string, opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request("POST", "/api/v1/plugins:enable", { id }, { headers: idemHeaders(opts) });
  }

  /** `POST /api/v1/plugins:disable`. */
  async disable(id: string, opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request("POST", "/api/v1/plugins:disable", { id }, { headers: idemHeaders(opts) });
  }

  /** `POST /api/v1/plugins:upgrade`. */
  async upgrade(apexpkg: Uint8Array, grants: string[] = [], opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request(
      "POST",
      "/api/v1/plugins:upgrade",
      { apexpkg: base64Encode(apexpkg), grants },
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/plugins:rollback`. */
  async rollback(id: string, opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request("POST", "/api/v1/plugins:rollback", { id }, { headers: idemHeaders(opts) });
  }

  /** `POST /api/v1/plugins:trust`. */
  async trust(publisher: string, publicKeyHex: string, opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request(
      "POST",
      "/api/v1/plugins:trust",
      { publisher, public_key_hex: publicKeyHex },
      { headers: idemHeaders(opts) },
    );
  }

  /** `DELETE /api/v1/plugins/{id}` — `id` is `publisher/name`. */
  async uninstall(id: string, opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request("DELETE", `/api/v1/plugins/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }
}

class MarketplaceResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/marketplace/listings`. */
  async search(params?: MarketplaceSearchParams & PageParams): Promise<Page<unknown>> {
    return this.http.request("GET", "/api/v1/marketplace/listings", undefined, { query: params });
  }

  /** `POST /api/v1/marketplace:publish`. */
  async publish(
    apexpkg: Uint8Array,
    opts?: IdempotentOpts & { categories?: string[]; channel?: string },
  ): Promise<PublishResult> {
    return this.http.request(
      "POST",
      "/api/v1/marketplace:publish",
      {
        apexpkg: base64Encode(apexpkg),
        categories: opts?.categories ?? [],
        channel: opts?.channel,
      },
      { headers: idemHeaders(opts) },
    );
  }

  /** `GET /api/v1/marketplace/listings/{id}` — `id` is `publisher/name`. */
  async get(id: string): Promise<unknown> {
    return this.http.request("GET", `/api/v1/marketplace/listings/${encodeURIComponent(id)}`);
  }

  /** `GET /api/v1/marketplace/listings/{id}/download` — returns the raw
   * `.apexpkg` bytes (already base64-decoded). */
  async download(id: string, version?: string): Promise<Uint8Array> {
    const res = await this.http.request<{ id: string; apexpkg: string }>(
      "GET",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/download`,
      undefined,
      { query: { version } },
    );
    return base64Decode(res.apexpkg);
  }

  /** `GET /api/v1/marketplace/listings/{id}/attestation`. */
  async attestation(id: string, version?: string): Promise<Attestation> {
    return this.http.request(
      "GET",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/attestation`,
      undefined,
      { query: { version } },
    );
  }

  /** `POST /api/v1/marketplace/listings/{id}/reviews`. */
  async review(
    id: string,
    req: { author: string; rating: number; body?: string },
    opts?: IdempotentOpts,
  ): Promise<unknown> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/reviews`,
      req,
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/marketplace/listings/{id}/verify` — operator override,
   * bypasses the request/approve/reject workflow below. */
  async setVerified(
    id: string,
    verified = true,
    opts?: IdempotentOpts,
  ): Promise<{ id: string; verified: boolean }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/verify`,
      { verified },
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/marketplace/listings/{id}/request-review`. */
  async requestReview(id: string, opts?: IdempotentOpts): Promise<{ id: string; status: "pending" }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/request-review`,
      undefined,
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/marketplace/listings/{id}/approve`. */
  async approveReview(
    id: string,
    reviewer?: string,
    opts?: IdempotentOpts,
  ): Promise<{ id: string; verified: boolean; reviewer: string }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/approve`,
      { reviewer },
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/marketplace/listings/{id}/reject`. */
  async rejectReview(
    id: string,
    reason: string,
    reviewer?: string,
    opts?: IdempotentOpts,
  ): Promise<{ id: string; verified: boolean; reviewer: string; reason: string }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/reject`,
      { reason, reviewer },
      { headers: idemHeaders(opts) },
    );
  }

  /** `POST /api/v1/marketplace/listings/{id}/install`. */
  async install(
    id: string,
    opts?: IdempotentOpts & { version?: string; grants?: string[] },
  ): Promise<unknown> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/install`,
      { version: opts?.version, grants: opts?.grants ?? [] },
      { headers: idemHeaders(opts) },
    );
  }
}

class SecretsResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/secrets`. */
  async list(params?: PageParams): Promise<Page<SecretMetadata>> {
    return this.http.request("GET", "/api/v1/secrets", undefined, { query: params });
  }

  /** `POST /api/v1/secrets` — value is never returned. */
  async create(name: string, value: string, opts?: IdempotentOpts): Promise<SecretMetadata> {
    return this.http.request("POST", "/api/v1/secrets", { name, value }, { headers: idemHeaders(opts) });
  }

  /** `GET /api/v1/secrets/{name}`. */
  async get(name: string): Promise<SecretMetadata> {
    return this.http.request("GET", `/api/v1/secrets/${encodeURIComponent(name)}`);
  }

  /** `DELETE /api/v1/secrets/{name}`. */
  async delete(name: string, opts?: IdempotentOpts): Promise<void> {
    return this.http.request("DELETE", `/api/v1/secrets/${encodeURIComponent(name)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  /** `POST /api/v1/secrets/{name}/rotate`. */
  async rotate(name: string, value: string, opts?: IdempotentOpts): Promise<SecretMetadata> {
    return this.http.request(
      "POST",
      `/api/v1/secrets/${encodeURIComponent(name)}/rotate`,
      { value },
      { headers: idemHeaders(opts) },
    );
  }
}

class OrganizationsResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/organizations`. */
  async list(params?: PageParams): Promise<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/organizations", undefined, { query: params });
  }

  /** `POST /api/v1/organizations`. */
  async create(name: string, opts?: IdempotentOpts): Promise<Record<string, unknown>> {
    return this.http.request("POST", "/api/v1/organizations", { name }, { headers: idemHeaders(opts) });
  }
}

class ProjectsResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/projects`. */
  async list(params?: PageParams): Promise<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/projects", undefined, { query: params });
  }

  /** `POST /api/v1/projects`. Returns the project plus its `ETag` (current
   * `version`), for a subsequent {@link update}'s `ifMatch`. */
  async create(
    name: string,
    organization: string,
    opts?: IdempotentOpts,
  ): Promise<{ project: Record<string, unknown>; etag?: string }> {
    const { data, headers } = await this.http.requestWithHeaders<Record<string, unknown>>(
      "POST",
      "/api/v1/projects",
      { name, organization },
      { headers: idemHeaders(opts) },
    );
    return { project: data, etag: headers.get("etag") ?? undefined };
  }

  /** `GET /api/v1/projects/{id}`. */
  async get(id: string): Promise<{ project: Record<string, unknown>; etag?: string }> {
    const { data, headers } = await this.http.requestWithHeaders<Record<string, unknown>>(
      "GET",
      `/api/v1/projects/${encodeURIComponent(id)}`,
    );
    return { project: data, etag: headers.get("etag") ?? undefined };
  }

  /** `PATCH /api/v1/projects/{id}`. `ifMatch` (from a prior {@link get}/
   * {@link create}'s `etag`) guards against a lost concurrent update — a stale
   * value throws {@link ApexApiError} with `status === 409`. */
  async update(
    id: string,
    patch: { settings?: Record<string, unknown>; status?: string },
    ifMatch?: string,
    opts?: IdempotentOpts,
  ): Promise<{ project: Record<string, unknown>; etag?: string }> {
    const headers: Record<string, string> = { ...idemHeaders(opts) };
    if (ifMatch) headers["If-Match"] = ifMatch;
    const { data, headers: responseHeaders } = await this.http.requestWithHeaders<Record<string, unknown>>(
      "PATCH",
      `/api/v1/projects/${encodeURIComponent(id)}`,
      patch,
      { headers },
    );
    return { project: data, etag: responseHeaders.get("etag") ?? undefined };
  }

  /** `DELETE /api/v1/projects/{id}`. */
  async delete(id: string, opts?: IdempotentOpts): Promise<void> {
    return this.http.request("DELETE", `/api/v1/projects/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  /** `GET /api/v1/projects/{id}/members`. */
  async listMembers(id: string): Promise<{ members: Array<{ user: string; role: Role }> }> {
    return this.http.request("GET", `/api/v1/projects/${encodeURIComponent(id)}/members`);
  }

  /** `POST /api/v1/projects/{id}/members`. */
  async addMember(id: string, user: string, role: Role, opts?: IdempotentOpts): Promise<unknown> {
    return this.http.request(
      "POST",
      `/api/v1/projects/${encodeURIComponent(id)}/members`,
      { user, role },
      { headers: idemHeaders(opts) },
    );
  }

  /** `DELETE /api/v1/projects/{id}/members/{uid}`. */
  async removeMember(id: string, userId: string, opts?: IdempotentOpts): Promise<void> {
    return this.http.request(
      "DELETE",
      `/api/v1/projects/${encodeURIComponent(id)}/members/${encodeURIComponent(userId)}`,
      undefined,
      { headers: idemHeaders(opts) },
    );
  }

  /** `GET /api/v1/projects/{id}/quota`. */
  async getQuota(id: string): Promise<{ scope: "project"; limits: Record<string, unknown> }> {
    return this.http.request("GET", `/api/v1/projects/${encodeURIComponent(id)}/quota`);
  }

  /** `PATCH /api/v1/projects/{id}/quota` — org.admin only. */
  async updateQuota(
    id: string,
    limits: { concurrent_agent_runs?: number; llm_cost_per_day_usd?: number },
    opts?: IdempotentOpts,
  ): Promise<{ scope: "project"; limits: Record<string, unknown> }> {
    return this.http.request("PATCH", `/api/v1/projects/${encodeURIComponent(id)}/quota`, limits, {
      headers: idemHeaders(opts),
    });
  }
}

class WebhooksResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/webhooks` — secrets redacted. */
  async list(params?: PageParams): Promise<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/webhooks", undefined, { query: params });
  }

  /** `POST /api/v1/webhooks`. */
  async register(
    req: { url: string; events: string[]; secret: string },
    opts?: IdempotentOpts,
  ): Promise<unknown> {
    return this.http.request("POST", "/api/v1/webhooks", req, { headers: idemHeaders(opts) });
  }

  /** `DELETE /api/v1/webhooks/{id}`. */
  async remove(id: string, opts?: IdempotentOpts): Promise<void> {
    return this.http.request("DELETE", `/api/v1/webhooks/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }
}

class AuditResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/audit` — tenant-scoped, tamper-evident hash-chained log,
   * most-recent first, cursor-paginated (RM-GA-P4 API-701). `after_ms`/
   * `before_ms` restrict to a timestamp range (epoch ms, inclusive; SEC-301) —
   * a paged, time-ranged query reads a bounded tail of the log rather than
   * scanning it whole, which is also why `total_estimate` is always `null`
   * on this route specifically (see {@link AuditPage}). */
  async query(
    params?: {
      principal?: string;
      action?: string;
      after_ms?: number;
      before_ms?: number;
    } & PageParams,
  ): Promise<AuditPage<AuditEntry>> {
    return this.http.request("GET", "/api/v1/audit", undefined, { query: params });
  }
}

class ToolsResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/tools` — built-ins + enabled plugin tools. Unauthenticated. */
  async list(params?: PageParams): Promise<Page<ToolSummary>> {
    return this.http.request("GET", "/api/v1/tools", undefined, { query: params });
  }
}

/** Generative UI (PRD-005 RM-GUI-P1): pending validated frames and typed human
 * decisions. `@apex/ui-react` builds on these same three routes — reach for it
 * instead of hand-rolling a renderer against this resource directly. */
class UiResource {
  constructor(private readonly http: HttpClient) {}

  /** `GET /api/v1/ui/frames` — the caller's tenant's pending validated frames. */
  async list(): Promise<{ data: PendingUiFrame[] }> {
    return this.http.request("GET", "/api/v1/ui/frames");
  }

  /** `GET /api/v1/ui/frames/{frame_id}` — one pending frame. */
  async get(frameId: string): Promise<PendingUiFrame> {
    return this.http.request("GET", `/api/v1/ui/frames/${encodeURIComponent(frameId)}`);
  }

  /** `POST /api/v1/ui/decisions/{frame_id}` (HIL-302/303) — validated
   * fail-closed against the frame it answers before anything reaches the
   * workflow; an undeclared action or a constraint violation is a `400`
   * ({@link ApexApiError}), not a silently-accepted decision. */
  async decide(
    frameId: string,
    req: UiDecisionRequest,
    opts?: IdempotentOpts,
  ): Promise<UiDecisionResult> {
    return this.http.request(
      "POST",
      `/api/v1/ui/decisions/${encodeURIComponent(frameId)}`,
      req,
      { headers: idemHeaders(opts) },
    );
  }
}
