import { Observable, of, throwError, timer } from "rxjs";
import { map, switchMap } from "rxjs/operators";
import { WovyrTimeoutError } from "./errors.js";
import { WovyrHttpClient } from "./http.js";
import { versionSkew } from "./version.js";
import type {
  AgentSummary,
  AuditEntry,
  AuditPage,
  Health,
  MarketplaceListing,
  MarketplaceSearchParams,
  McpConnectionRequest,
  McpConnectionWithTools,
  McpConnectionsPage,
  McpRefreshResult,
  Organization,
  Page,
  PageParams,
  PendingUiFrame,
  PluginSummary,
  Project,
  PublishResult,
  PutMemoryRequest,
  QueryMemoryRequest,
  MemoryQueryResult,
  Role,
  RunRequest,
  RunResult,
  SecretMetadata,
  SubmitWorkflowRequest,
  ToolSummary,
  UiDecisionOutcome,
  UiDecisionRequest,
  UiDecisionResult,
  UiFrame,
  Webhook,
  WorkflowListParams,
  WorkflowValidation,
  WovyrClientOptions,
} from "./types.js";

const TERMINAL_WORKFLOW_STATUSES = new Set(["completed", "failed", "cancelled"]);

interface IdempotentOpts {
  idempotencyKey?: string;
}

function idemHeaders(opts?: IdempotentOpts): Record<string, string> {
  return opts?.idempotencyKey ? { "Idempotency-Key": opts.idempotencyKey } : {};
}

function b64encode(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return typeof btoa === "function" ? btoa(bin) : Buffer.from(bin, "binary").toString("base64");
}

function b64decode(b64: string): Uint8Array {
  const bin = typeof atob === "function" ? atob(b64) : Buffer.from(b64, "base64").toString("binary");
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** A client for one Wovyr `wovyr-server` instance, scoped to one tenant/principal
 * (construct a new client per tenant to act as a different one). Mirrors the
 * server's actual routes (see `docs/09-api/openapi.yaml`) — not the aspirational
 * conventions in `overview.md`. Angular-idiomatic: every method returns an
 * `Observable`, built on `HttpClient`. */
export class WovyrClient {
  private readonly http: WovyrHttpClient;
  private warnedSkew = false;

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
  readonly mcp: McpResource;

  constructor(options: WovyrClientOptions) {
    this.http = new WovyrHttpClient(options);
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
    this.mcp = new McpResource(this.http);
  }

  /** `GET /healthz`. Also the SDK's version handshake (DX-303): when the
   * server's major.minor differs from the release this SDK tracks, a warning
   * is logged via `console.warn` — once per client, never thrown. */
  health(): Observable<Health> {
    return this.http.request<Health>("GET", "/healthz").pipe(
      map((h) => {
        if (!this.warnedSkew) {
          const warning = versionSkew(h.version);
          if (warning) {
            this.warnedSkew = true;
            console.warn(warning);
          }
        }
        return h;
      }),
    );
  }
}

class AgentsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  run(req: RunRequest, opts?: IdempotentOpts & { project?: string }): Observable<RunResult> {
    const headers: Record<string, string> = { ...idemHeaders(opts) };
    if (opts?.project) headers["X-Wovyr-Project"] = opts.project;
    return this.http.request("POST", "/api/v1/agents:run", req, { headers });
  }

  list(params?: PageParams): Observable<Page<string>> {
    return this.http.request("GET", "/api/v1/agents", undefined, { query: params });
  }

  create(manifest: string, opts?: IdempotentOpts): Observable<{ id: string; status: "created" }> {
    return this.http.request("POST", "/api/v1/agents", { manifest }, { headers: idemHeaders(opts) });
  }

  get(id: string): Observable<AgentSummary> {
    return this.http.request("GET", `/api/v1/agents/${encodeURIComponent(id)}`);
  }

  delete(id: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request("DELETE", `/api/v1/agents/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  runStored(
    id: string,
    req?: { input?: unknown; max_steps?: number },
    opts?: IdempotentOpts & { project?: string },
  ): Observable<RunResult> {
    const headers: Record<string, string> = { ...idemHeaders(opts) };
    if (opts?.project) headers["X-Wovyr-Project"] = opts.project;
    return this.http.request("POST", `/api/v1/agents/${encodeURIComponent(id)}/run`, req ?? {}, { headers });
  }
}

class WorkflowsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  validate(manifest: string): Observable<WorkflowValidation> {
    return this.http.request("POST", "/api/v1/workflows/validate", { manifest });
  }

  list(params?: WorkflowListParams): Observable<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/workflows", undefined, { query: params });
  }

  submit(
    req: SubmitWorkflowRequest,
    opts?: IdempotentOpts,
  ): Observable<{ execution_id: string; status: "submitted" }> {
    return this.http.request("POST", "/api/v1/workflows", req, { headers: idemHeaders(opts) });
  }

  get(id: string): Observable<{ execution: Record<string, unknown>; events: Record<string, unknown>[] }> {
    return this.http.request("GET", `/api/v1/workflows/${encodeURIComponent(id)}`);
  }

  /** Poll {@link get} until the execution reaches a terminal status
   * (`completed`/`failed`/`cancelled` — compared case-insensitively) and emit
   * the final snapshot exactly once (DX-301). Errors with {@link WovyrTimeoutError}
   * if `timeoutMs` (default 60s) elapses before terminal. Built on `get()` so it
   * shares that method's error handling and retry semantics. */
  waitForCompletion(
    id: string,
    opts?: { intervalMs?: number; timeoutMs?: number },
  ): Observable<{ execution: Record<string, unknown>; events: Record<string, unknown>[] }> {
    const intervalMs = opts?.intervalMs ?? 500;
    const timeoutMs = opts?.timeoutMs ?? 60_000;
    const deadline = Date.now() + timeoutMs;
    const poll = (): Observable<{ execution: Record<string, unknown>; events: Record<string, unknown>[] }> =>
      this.get(id).pipe(
        switchMap((snapshot) => {
          const status = String(snapshot.execution["status"] ?? "").toLowerCase();
          if (TERMINAL_WORKFLOW_STATUSES.has(status)) return of(snapshot);
          if (Date.now() + intervalMs > deadline) {
            return throwError(
              () => new WovyrTimeoutError(`workflow execution ${id} still \`${status}\` after ${timeoutMs}ms`),
            );
          }
          return timer(intervalMs).pipe(switchMap(poll));
        }),
      );
    return poll();
  }

  cancel(id: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request("DELETE", `/api/v1/workflows/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  signal(
    id: string,
    req: { manifest: string; event: string; payload?: unknown },
    opts?: IdempotentOpts,
  ): Observable<{ execution_id: string; event: string; status: "signalled" }> {
    return this.http.request("POST", `/api/v1/workflows/${encodeURIComponent(id)}/signal`, req, {
      headers: idemHeaders(opts),
    });
  }

  approve(
    id: string,
    req: { manifest: string; activity_id: string; decision?: unknown },
    opts?: IdempotentOpts,
  ): Observable<{ execution_id: string; activity_id: string; status: "approved" }> {
    return this.http.request("POST", `/api/v1/workflows/${encodeURIComponent(id)}/approve`, req, {
      headers: idemHeaders(opts),
    });
  }
}

class MemoryResource {
  constructor(private readonly http: WovyrHttpClient) {}

  namespaces(): Observable<{ namespaces: Array<{ namespace: string; count: number }>; total: number }> {
    return this.http.request("GET", "/api/v1/memory/namespaces");
  }

  listRecords(params?: PageParams & { namespace?: string }): Observable<Page<Record<string, unknown>>> {
    return this.http.request("GET", "/api/v1/memory/records", undefined, { query: params });
  }

  put(req: PutMemoryRequest, opts?: IdempotentOpts): Observable<{ id: string; status: "stored" }> {
    return this.http.request("POST", "/api/v1/memory/records", req, { headers: idemHeaders(opts) });
  }

  query(req: QueryMemoryRequest): Observable<{ data: MemoryQueryResult[]; count: number }> {
    return this.http.request("POST", "/api/v1/memory:query", req);
  }
}

class PluginsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<Page<PluginSummary>> {
    return this.http.request("GET", "/api/v1/plugins", undefined, { query: params });
  }

  install(wovyrpkg: Uint8Array, grants: string[] = [], opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request(
      "POST",
      "/api/v1/plugins:install",
      { wovyrpkg: b64encode(wovyrpkg), grants },
      { headers: idemHeaders(opts) },
    );
  }

  enable(id: string, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request("POST", "/api/v1/plugins:enable", { id }, { headers: idemHeaders(opts) });
  }

  disable(id: string, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request("POST", "/api/v1/plugins:disable", { id }, { headers: idemHeaders(opts) });
  }

  upgrade(wovyrpkg: Uint8Array, grants: string[] = [], opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request(
      "POST",
      "/api/v1/plugins:upgrade",
      { wovyrpkg: b64encode(wovyrpkg), grants },
      { headers: idemHeaders(opts) },
    );
  }

  rollback(id: string, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request("POST", "/api/v1/plugins:rollback", { id }, { headers: idemHeaders(opts) });
  }

  trust(publisher: string, publicKeyHex: string, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request(
      "POST",
      "/api/v1/plugins:trust",
      { publisher, public_key_hex: publicKeyHex },
      { headers: idemHeaders(opts) },
    );
  }

  uninstall(id: string, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request("DELETE", `/api/v1/plugins/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }
}

class MarketplaceResource {
  constructor(private readonly http: WovyrHttpClient) {}

  search(params?: MarketplaceSearchParams & PageParams): Observable<Page<MarketplaceListing>> {
    return this.http.request("GET", "/api/v1/marketplace/listings", undefined, { query: params });
  }

  publish(
    wovyrpkg: Uint8Array,
    opts?: IdempotentOpts & { categories?: string[]; channel?: string },
  ): Observable<PublishResult> {
    return this.http.request(
      "POST",
      "/api/v1/marketplace:publish",
      { wovyrpkg: b64encode(wovyrpkg), categories: opts?.categories ?? [], channel: opts?.channel },
      { headers: idemHeaders(opts) },
    );
  }

  get(id: string): Observable<unknown> {
    return this.http.request("GET", `/api/v1/marketplace/listings/${encodeURIComponent(id)}`);
  }

  download(id: string, version?: string): Observable<Uint8Array> {
    return this.http
      .request<{ id: string; wovyrpkg: string }>(
        "GET",
        `/api/v1/marketplace/listings/${encodeURIComponent(id)}/download`,
        undefined,
        { query: { version } },
      )
      .pipe(map((res) => b64decode(res.wovyrpkg)));
  }

  attestation(id: string, version?: string): Observable<unknown> {
    return this.http.request(
      "GET",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/attestation`,
      undefined,
      { query: { version } },
    );
  }

  review(id: string, req: { author: string; rating: number; body?: string }, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/reviews`,
      req,
      { headers: idemHeaders(opts) },
    );
  }

  setVerified(id: string, verified = true, opts?: IdempotentOpts): Observable<{ id: string; verified: boolean }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/verify`,
      { verified },
      { headers: idemHeaders(opts) },
    );
  }

  requestReview(id: string, opts?: IdempotentOpts): Observable<{ id: string; status: "pending" }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/request-review`,
      undefined,
      { headers: idemHeaders(opts) },
    );
  }

  approveReview(
    id: string,
    reviewer?: string,
    opts?: IdempotentOpts,
  ): Observable<{ id: string; verified: boolean; reviewer: string }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/approve`,
      { reviewer },
      { headers: idemHeaders(opts) },
    );
  }

  rejectReview(
    id: string,
    reason: string,
    reviewer?: string,
    opts?: IdempotentOpts,
  ): Observable<{ id: string; verified: boolean; reviewer: string; reason: string }> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/reject`,
      { reason, reviewer },
      { headers: idemHeaders(opts) },
    );
  }

  install(id: string, opts?: IdempotentOpts & { version?: string; grants?: string[] }): Observable<unknown> {
    return this.http.request(
      "POST",
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/install`,
      { version: opts?.version, grants: opts?.grants ?? [] },
      { headers: idemHeaders(opts) },
    );
  }
}

class SecretsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<Page<SecretMetadata>> {
    return this.http.request("GET", "/api/v1/secrets", undefined, { query: params });
  }

  create(name: string, value: string, opts?: IdempotentOpts): Observable<{ reference: string; status: "created" }> {
    return this.http.request("POST", "/api/v1/secrets", { name, value }, { headers: idemHeaders(opts) });
  }

  get(name: string): Observable<SecretMetadata> {
    return this.http.request("GET", `/api/v1/secrets/${encodeURIComponent(name)}`);
  }

  delete(name: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request("DELETE", `/api/v1/secrets/${encodeURIComponent(name)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  rotate(name: string, opts?: IdempotentOpts): Observable<{ reference: string; status: "rotated" }> {
    return this.http.request("POST", `/api/v1/secrets/${encodeURIComponent(name)}/rotate`, {}, {
      headers: idemHeaders(opts),
    });
  }
}

class OrganizationsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<Page<Organization>> {
    return this.http.request("GET", "/api/v1/organizations", undefined, { query: params });
  }

  create(name: string, opts?: IdempotentOpts): Observable<Organization> {
    return this.http.request("POST", "/api/v1/organizations", { name }, { headers: idemHeaders(opts) });
  }
}

class ProjectsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<Page<Project>> {
    return this.http.request("GET", "/api/v1/projects", undefined, { query: params });
  }

  create(org: string, name: string, opts?: IdempotentOpts): Observable<Project> {
    return this.http.request("POST", "/api/v1/projects", { org, name }, { headers: idemHeaders(opts) });
  }

  get(id: string): Observable<Project> {
    return this.http.request("GET", `/api/v1/projects/${encodeURIComponent(id)}`);
  }

  patch(id: string, patch: Partial<Project>, opts?: IdempotentOpts): Observable<Project> {
    return this.http.request("PATCH", `/api/v1/projects/${encodeURIComponent(id)}`, patch, {
      headers: idemHeaders(opts),
    });
  }

  delete(id: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request("DELETE", `/api/v1/projects/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }

  members(id: string): Observable<Page<{ user: string; role: Role }>> {
    return this.http.request("GET", `/api/v1/projects/${encodeURIComponent(id)}/members`);
  }

  addMember(id: string, user: string, role: Role, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request(
      "POST",
      `/api/v1/projects/${encodeURIComponent(id)}/members`,
      { user, role },
      { headers: idemHeaders(opts) },
    );
  }

  removeMember(id: string, uid: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request(
      "DELETE",
      `/api/v1/projects/${encodeURIComponent(id)}/members/${encodeURIComponent(uid)}`,
      undefined,
      { headers: idemHeaders(opts) },
    );
  }

  quota(id: string): Observable<{ limits: Record<string, unknown> }> {
    return this.http.request("GET", `/api/v1/projects/${encodeURIComponent(id)}/quota`);
  }

  setQuota(id: string, limits: Record<string, unknown>, opts?: IdempotentOpts): Observable<unknown> {
    return this.http.request(
      "PATCH",
      `/api/v1/projects/${encodeURIComponent(id)}/quota`,
      { limits },
      { headers: idemHeaders(opts) },
    );
  }
}

class WebhooksResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<Page<Webhook>> {
    return this.http.request("GET", "/api/v1/webhooks", undefined, { query: params });
  }

  create(url: string, events: string[], opts?: IdempotentOpts): Observable<{ id: string; status: "created" }> {
    return this.http.request("POST", "/api/v1/webhooks", { url, events }, { headers: idemHeaders(opts) });
  }

  delete(id: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request("DELETE", `/api/v1/webhooks/${encodeURIComponent(id)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }
}

class AuditResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams & { after_ms?: number; before_ms?: number }): Observable<AuditPage<AuditEntry>> {
    return this.http.request("GET", "/api/v1/audit", undefined, { query: params });
  }
}

class ToolsResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<Page<ToolSummary>> {
    return this.http.request("GET", "/api/v1/tools", undefined, { query: params });
  }
}

class UiResource {
  constructor(private readonly http: WovyrHttpClient) {}

  present(frame: UiFrame, opts?: IdempotentOpts): Observable<PendingUiFrame> {
    return this.http.request("POST", "/api/v1/ui/present", frame, { headers: idemHeaders(opts) });
  }

  decision(frameId: string, req: UiDecisionRequest): Observable<UiDecisionResult> {
    return this.http.request("POST", `/api/v1/ui/decisions/${encodeURIComponent(frameId)}`, req);
  }

  decisionOutcome(frameId: string): Observable<UiDecisionOutcome> {
    return this.http.request("GET", `/api/v1/ui/decisions/${encodeURIComponent(frameId)}`);
  }
}

class McpResource {
  constructor(private readonly http: WovyrHttpClient) {}

  list(params?: PageParams): Observable<McpConnectionsPage> {
    return this.http.request("GET", "/api/v1/mcp/connections", undefined, { query: params });
  }

  create(req: McpConnectionRequest, opts?: IdempotentOpts): Observable<McpConnectionWithTools> {
    return this.http.request("POST", "/api/v1/mcp/connections", req, { headers: idemHeaders(opts) });
  }

  refresh(name: string): Observable<McpRefreshResult> {
    return this.http.request("POST", `/api/v1/mcp/connections/${encodeURIComponent(name)}/refresh`, {});
  }

  delete(name: string, opts?: IdempotentOpts): Observable<void> {
    return this.http.request("DELETE", `/api/v1/mcp/connections/${encodeURIComponent(name)}`, undefined, {
      headers: idemHeaders(opts),
    });
  }
}
