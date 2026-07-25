/** Options accepted by every {@link WovyrClient} constructor call. */
export interface WovyrClientOptions {
  /** Base URL of a running `wovyr-server` (e.g. `http://127.0.0.1:8080`). */
  baseUrl: string;
  /** Sent as `X-Wovyr-Tenant` on every request. Defaults to `"default"`. */
  tenant?: string;
  /** Sent as `X-Wovyr-Principal` on every request, if set. */
  principal?: string;
  /** `fetch` override, mainly for tests. Defaults to the global `fetch`. */
  fetchImpl?: typeof fetch;
  /** Retry policy for transient failures (network errors, `429`, `502`/`503`/
   * `504`). Defaults to 2 retries with a 250ms base delay, doubling each
   * attempt. Set `maxRetries: 0` to disable. Applied to every `GET`, and —
   * DX-301 — to mutating requests **only when they carry an
   * `Idempotency-Key`** (pass `idempotencyKey` on the call): the server's
   * replay middleware then makes the retry safe, whereas a keyless retry
   * could double-execute. */
  retry?: RetryOptions;
}

export interface RetryOptions {
  /** Number of retry attempts after the initial try. Default `2`. */
  maxRetries?: number;
  /** Base delay in ms before the first retry; doubles each subsequent
   * attempt (250 → 500 → 1000 …). Default `250`. */
  baseDelayMs?: number;
}

/** The cursor-pagination envelope every list endpoint returns. */
export interface Page<T> {
  data: T[];
  has_more: boolean;
  next_cursor: string | null;
  total_estimate: number;
}

/** `GET /api/v1/audit`'s envelope (SEC-301): identical to {@link Page} except
 * `total_estimate` is always `null` — computing an exact count would require
 * the full-log scan this route's bounded, time-ranged paging exists to avoid. */
export interface AuditPage<T> extends Omit<Page<T>, "total_estimate"> {
  total_estimate: null;
}

export interface PageParams {
  limit?: number;
  cursor?: string;
}

export interface Usage {
  total_tokens: number;
  cost_usd: number;
}

export interface RunRequest {
  manifest: string;
  input?: unknown;
  max_steps?: number;
}

export interface RunResult {
  run_id: string;
  status: "succeeded";
  output: { message: string };
  steps: number;
  usage: Usage;
}

/** One parsed SSE frame from `agents:stream`. The terminal frame is either
 * `{ type: "result", ... }` or `{ type: "error", message: string }` — mapped
 * from the raw `event: result` / `event: error` SSE events. */
export type AgentStreamEvent =
  | { type: "start"; [key: string]: unknown }
  | { type: "memory"; [key: string]: unknown }
  | { type: "delta"; text: string }
  /** The model's reasoning/thinking channel, where the provider exposes one
   * (AIC-202). Display-only — never part of the final answer. */
  | { type: "reasoning"; text: string }
  /** An incremental fragment of a tool call's JSON arguments as the model
   * composes it (AIC-202); `arguments` is this frame's fragment only. The
   * complete call still arrives as its own `tool_call` frame. */
  | { type: "tool_call_delta"; index: number; name: string; arguments: string }
  | { type: "tool_call"; [key: string]: unknown }
  | { type: "tool_result"; [key: string]: unknown }
  /** A validated generative-UI frame presented mid-run (PRD-005 UIP-104) —
   * only trust-layer-checked frames are ever emitted. `frame` is left
   * `unknown` here (a generic API client shouldn't parse the vocabulary);
   * `@wovyr/ui-react` owns the typed `UiFrame` shape and rendering. */
  | { type: "ui_frame"; frame_id: string; frame: unknown }
  | { type: "done"; usage: Usage }
  | ({ type: "result" } & RunResult)
  | { type: "error"; message: string };

export interface AgentSummary {
  id: string;
  manifest: string;
}

/** `GET /healthz`. */
export interface Health {
  status: string;
  version: string;
}

/** A workflow execution summary (wovyr-workflow `ExecutionSummary`) — the row
 * shape `GET /api/v1/workflows` pages and `GET /api/v1/workflows/{id}` returns
 * under `execution`. */
export interface WorkflowSummary {
  execution_id: string;
  workflow_name: string;
  workflow_version: string;
  status: string;
  activities: Record<string, string>;
  waiting_on: string[];
}

export interface WorkflowValidation {
  valid: boolean;
  name: string;
  version: string;
  activities: Array<{ id: string; type: string; name?: string }>;
  edges: Array<{ from: string; to: string; when?: string | null }>;
  activity_count: number;
}

export type WorkflowStatus =
  | "created"
  | "validated"
  | "scheduled"
  | "running"
  | "waiting"
  | "resumed"
  | "compensating"
  | "completed"
  | "failed"
  | "cancelled";

export interface WorkflowListParams extends PageParams {
  workflow?: string;
  status?: WorkflowStatus;
}

export interface SubmitWorkflowRequest {
  manifest: string;
  input?: unknown;
  execution_id?: string;
}

export type MemoryType = "semantic" | "conversation" | "workflow" | "episodic";

export interface PutMemoryRequest {
  namespace: string;
  content: string;
  type?: MemoryType;
  importance?: number;
  tags?: string[];
  required_scopes?: string[];
}

export interface QueryMemoryRequest {
  text: string;
  namespace?: string;
  strategy?: "vector" | "keyword" | "hybrid";
  limit?: number;
  diversity?: number;
  min_importance?: number;
  tags?: string[];
  grants?: string[];
  relevance?: number;
  recency?: number;
  importance?: number;
}

export interface MemoryQueryResult {
  id: string;
  namespace: string;
  content: string;
  type: string;
  importance: number;
  tags: string[];
  score: number;
  breakdown: { relevance: number; recency: number; importance: number; total: number };
}

export interface CapabilityDescriptor {
  kind: "tool" | "provider" | "memory_backend" | "policy" | "workflow_activity";
  id: string;
}

export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  publisher: string;
  description: string;
  state: "enabled" | "disabled";
  permissions: string[];
  granted: string[];
  capabilities: CapabilityDescriptor[];
  platform_api: string;
}

export interface ScanFinding {
  code: string;
  severity: "info" | "warning" | "critical";
  message: string;
}

export interface ScanReport {
  findings: ScanFinding[];
}

export interface MarketplaceSearchParams {
  q?: string;
  category?: string;
  capability?: CapabilityDescriptor["kind"];
}

/** Permission-risk classification of a listing's latest version. */
export type PermissionRisk = "low" | "medium" | "high";

/** A marketplace listing projection (wovyr-marketplace `Listing`) — the row
 * shape `GET /api/v1/marketplace/listings` pages. `capabilities` are the
 * latest version's capability kinds (snake_case strings, e.g. `tool`). */
export interface MarketplaceListing {
  id: string;
  publisher: string;
  name: string;
  description: string;
  categories: string[];
  capabilities: string[];
  permissions: string[];
  risk: PermissionRisk;
  /** Most severe scan finding on the latest version (absent/null ⇒ clean scan). */
  scan_severity?: ScanFinding["severity"] | null;
  /** Number of scan findings on the latest version. */
  scan_findings?: number;
  versions: string[];
  channels: Record<string, string>;
  rating: number | null;
  reviews: number;
  installs: number;
  verified: boolean;
}

export interface PublishResult {
  listing: string;
  reference: string;
  channel: string;
  status: "published";
  scan: ScanReport;
}

/** One SBOM component a package bundles (wovyr-plugin `SbomComponent`). */
export interface SbomComponent {
  name: string;
  version: string;
  license?: string;
}

/** Build provenance for a package (wovyr-plugin `Provenance`). */
export interface Provenance {
  builder: string;
  source: string;
  built_at: string;
}

export interface Attestation {
  id: string;
  version: string;
  publisher: string;
  verified: boolean;
  signature_verified: boolean;
  risk: PermissionRisk;
  permissions: string[];
  package_digest: string;
  sbom: { components: SbomComponent[] } | null;
  provenance: Provenance | null;
  scan: ScanReport;
}

export interface SecretMetadata {
  reference: string;
  name: string;
  version: number;
  [key: string]: unknown;
}

export type Role = "viewer" | "editor" | "project_admin" | "org_admin" | "platform_admin";

// ---- tenancy (wovyr-tenancy; organizations/projects routes) ----

export interface Organization {
  id: string;
  name: string;
  tenant: string;
}

export interface Project {
  id: string;
  name: string;
  organization: string;
  tenant: string;
  settings?: Record<string, unknown>;
  status?: string;
  version?: number;
}

export interface Membership {
  user: string;
  role: Role;
  scope: unknown;
}

export interface QuotaLimits {
  llm_cost_per_day_usd?: number | null;
  /** LLM tokens (prompt + completion) per rolling day (SRV-202). */
  llm_tokens_per_day?: number | null;
  concurrent_agent_runs?: number | null;
  max_mcp_connections?: number | null;
  /** Daily-reset boundary in minutes east of UTC (SRV-203); empty = UTC midnight. */
  day_reset_offset_minutes?: number | null;
}

/** A webhook subscription row (`GET /api/v1/webhooks` — secrets redacted). */
export interface Webhook {
  id: string;
  url: string;
  events: string[];
  active?: boolean;
}

/** A single audited action (wovyr-audit `AuditEvent`) — who did what, when,
 * with what outcome. `reason` is set for denials/errors (e.g. the missing
 * scope). */
export interface AuditEvent {
  timestamp_ms: number;
  actor: { principal: string; type: "user" | "service" | "api_key" | "system"; tenant: string };
  /** Dotted action, e.g. `secret.create`, `workflow.execution.cancel`. */
  action: string;
  /** The resource acted on, by type + id — never by value. */
  resource: { type: string; id: string };
  outcome: "allowed" | "denied" | "error";
  reason?: string;
  request_id?: string;
}

/** A persisted audit record (wovyr-audit `AuditEntry`): the event plus its
 * position and hash-chain links — the row shape `GET /api/v1/audit` pages.
 * (This type used to mirror the event fields flat, which is not what the
 * server sends — the event nests under `event`.) */
export interface AuditEntry {
  /** Derived id, `aud-<seq>`. */
  id: string;
  seq: number;
  event: AuditEvent;
  /** Hex sha256 of the previous entry (empty string for the genesis entry). */
  prev_hash: string;
  hash: string;
}

export interface ToolSummary {
  id: string;
  description: string;
  category: string;
  permissions: string[] | null;
}

/** How to reach an external MCP server (PRD-006 MCX-101) — `stdio` spawns an
 * arbitrary local command (materially higher privilege; gated behind
 * `mcp:admin` + the operator's `WOVYR_ENABLE_MCP_STDIO=1` opt-in, ADR-0012),
 * `http` POSTs JSON-RPC to a URL (SSRF-guarded the same way `http_get` is). */
export type McpTransport =
  | { kind: "stdio"; command: string; args: string[] }
  | { kind: "http"; url: string };

/** A tenant's persisted MCP connection (never a resolved credential value —
 * `secret_ref` is a `SecretRef` string into the secret vault, MCX-105). */
export interface McpConnection {
  name: string;
  transport: McpTransport;
  secret_ref?: string | null;
  secret_env_var?: string | null;
  tool_permissions?: string[] | null;
  created_ms: number;
  updated_ms: number;
}

/** One tool a connection's server currently reports via `tools/list`. */
export interface McpToolInfo {
  name: string;
  description: string;
}

/** `POST /api/v1/mcp/connections`'s request body. */
export interface McpConnectionRequest {
  name: string;
  transport: McpTransport;
  secret_ref?: string;
  secret_env_var?: string;
  tool_permissions?: string[];
}

/** `POST /api/v1/mcp/connections`'s response: the persisted connection plus
 * the tools discovered while verifying it dials successfully. */
export interface McpConnectionWithTools extends McpConnection {
  tools: McpToolInfo[];
}

/** `POST /api/v1/mcp/connections/{name}/refresh`'s response — a fresh
 * re-discovery, bypassing the connection's client cache (MCX-203). */
export interface McpRefreshResult {
  name: string;
  tools: McpToolInfo[];
}

/** `GET /api/v1/mcp/connections`'s response: the standard {@link Page}
 * envelope plus `stdio_enabled` (RM-MCX-P3-302) — whether the operator has
 * set `WOVYR_ENABLE_MCP_STDIO=1`, so a caller (the dashboard) knows to hide
 * the `stdio` transport option before the operator fills out a form, not
 * after a rejected submit. */
export interface McpConnectionsPage extends Page<McpConnection> {
  stdio_enabled: boolean;
}

/** A minimal shape for a UiFrame document — `root` is left `unknown` (a
 * generic API client shouldn't need to parse the component vocabulary);
 * `@wovyr/ui-react` owns the typed, full `UiFrame` shape and rendering. Enough
 * to *submit* a frame (`ui.present`); reach for `@wovyr/ui-react` to *build*
 * or *render* one. */
export interface UiFrame {
  schema_version: string;
  title?: string;
  root: unknown;
}

/** A pending, already-validated generative-UI frame awaiting a human decision
 * (PRD-005 RM-GUI-P1/P3). `frame` is left `unknown` for the same reason as
 * {@link UiFrame} — `@wovyr/ui-react` owns rendering, and can consume this
 * envelope directly (its `frame`/`frame_hash` fields are exactly what
 * `verifyFrame`/`UiFrameView` expect). `execution_id`/`activity_id` are
 * `null` for a **standalone** frame (RM-GUI-P3 EMB-701, `ui.present`) — one
 * presented with no workflow/agent involvement at all. */
export interface PendingUiFrame {
  frame_id: string;
  execution_id: string | null;
  activity_id: string | null;
  frame: unknown;
  frame_hash: string;
  /** Which policy judged the frame: `name@vN`, `hosted-floor`, or
   * `unrestricted` (GRD-206). */
  policy_ref: string;
  created_at_ms: number;
}

/** `POST /api/v1/ui/decisions/{frame_id}` request body (HIL-302/303): `action`
 * must be one of the frame's declared button actions, and `values` must match
 * declared inputs — the server rejects anything else at the boundary without
 * touching the workflow. */
export interface UiDecisionRequest {
  action: string;
  values?: Record<string, unknown>;
}

/** `GET /api/v1/ui/decisions/{frame_id}` response (RM-GUI-P3 EMB-701) — a
 * standalone frame's recorded decision, retrievable after the pending record
 * is gone. */
export interface UiDecisionOutcome {
  frame_id: string;
  action: string;
  values: Record<string, unknown>;
  decided_by: string;
  decided_at_ms: number;
  frame_hash: string;
}

export interface UiDecisionResult {
  frame_id: string;
  execution_id: string;
  activity_id: string;
  status: "decided";
}
