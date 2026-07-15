/** Options accepted by every {@link ApexClient} constructor call. */
export interface ApexClientOptions {
  /** Base URL of a running `apex-server` (e.g. `http://127.0.0.1:8080`). */
  baseUrl: string;
  /** Sent as `X-Apex-Tenant` on every request. Defaults to `"default"`. */
  tenant?: string;
  /** Sent as `X-Apex-Principal` on every request, if set. */
  principal?: string;
  /** `fetch` override, mainly for tests. Defaults to the global `fetch`. */
  fetchImpl?: typeof fetch;
  /** Retry policy for transient failures (network errors, `429`, `502`/`503`/
   * `504`). Defaults to 2 retries with a 250ms base delay, doubling each
   * attempt. Set `maxRetries: 0` to disable. Only applied to `GET` requests —
   * mutating requests are never auto-retried, since retrying one without an
   * `Idempotency-Key` could double-execute it. */
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
   * `@apex/ui-react` owns the typed `UiFrame` shape and rendering. */
  | { type: "ui_frame"; frame_id: string; frame: unknown }
  | { type: "done"; usage: Usage }
  | ({ type: "result" } & RunResult)
  | { type: "error"; message: string };

export interface AgentSummary {
  id: string;
  manifest: string;
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

export interface PublishResult {
  listing: string;
  reference: string;
  channel: string;
  status: "published";
  scan: ScanReport;
}

export interface Attestation {
  id: string;
  version: string;
  publisher: string;
  verified: boolean;
  signature_verified: boolean;
  risk: "low" | "medium" | "high";
  permissions: string[];
  package_digest: string;
  sbom: unknown[] | null;
  provenance: unknown | null;
  scan: ScanReport;
}

export interface SecretMetadata {
  reference: string;
  name: string;
  version: number;
  [key: string]: unknown;
}

export type Role = "viewer" | "editor" | "project_admin" | "org_admin" | "platform_admin";

export interface AuditEntry {
  actor: { principal: string; type?: string; tenant?: string };
  action: string;
  resource: { type?: string; id: string };
  outcome?: string;
  reason?: string;
  request_id?: string;
  timestamp_ms: number;
  prev_hash?: string;
  hash?: string;
}

export interface ToolSummary {
  id: string;
  description: string;
  category: string;
  permissions: string[] | null;
}

/** A pending, already-validated generative-UI frame awaiting a human decision
 * (PRD-005 RM-GUI-P1). `frame` is left `unknown` — a generic API client
 * shouldn't need to parse the component vocabulary; `@apex/ui-react` owns the
 * typed `UiFrame` shape and rendering, and can consume this envelope
 * directly (its `frame`/`frame_hash` fields are exactly what
 * `verifyFrame`/`UiFrameView` expect). */
export interface PendingUiFrame {
  frame_id: string;
  execution_id: string;
  activity_id: string;
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

export interface UiDecisionResult {
  frame_id: string;
  execution_id: string;
  activity_id: string;
  status: "decided";
}
