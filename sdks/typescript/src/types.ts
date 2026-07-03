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
}

/** The cursor-pagination envelope every list endpoint returns. */
export interface Page<T> {
  data: T[];
  has_more: boolean;
  next_cursor: string | null;
  total_estimate: number;
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
  | { type: "tool_call"; [key: string]: unknown }
  | { type: "tool_result"; [key: string]: unknown }
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
