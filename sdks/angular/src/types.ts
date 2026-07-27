/** Types mirroring the Wovyr AI Platform API. Kept byte-for-byte in sync with
 * `@wovyr/sdk`'s `types.ts` so the two SDKs are interchangeable on the wire.
 * Source of truth: `docs/09-api/openapi.yaml`. */

export interface WovyrClientOptions {
  /** A configured Angular `HttpClient` (from `provideHttpClient`). */
  http: import("@angular/common/http").HttpClient;
  /** Base URL of a running `wovyr-server` (e.g. `http://127.0.0.1:8080`). */
  baseUrl: string;
  /** Sent as `X-Wovyr-Tenant` on every request. Defaults to `"default"`. */
  tenant?: string;
  /** Sent as `X-Wovyr-Principal` on every request, if set. */
  principal?: string;
}

export interface Page<T> {
  data: T[];
  has_more: boolean;
  next_cursor: string | null;
  total_estimate: number;
}

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

export type AgentStreamEvent =
  | { type: "start"; [key: string]: unknown }
  | { type: "memory"; [key: string]: unknown }
  | { type: "delta"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "tool_call_delta"; index: number; name: string; arguments: string }
  | { type: "tool_call"; [key: string]: unknown }
  | { type: "tool_result"; [key: string]: unknown }
  | { type: "ui_frame"; frame_id: string; frame: unknown }
  | { type: "done"; usage: Usage }
  | ({ type: "result" } & RunResult)
  | { type: "error"; message: string };

export interface AgentSummary {
  id: string;
  manifest: string;
}

export interface Health {
  status: string;
  version: string;
}

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

export type PermissionRisk = "low" | "medium" | "high";

export interface MarketplaceListing {
  id: string;
  publisher: string;
  name: string;
  description: string;
  categories: string[];
  capabilities: string[];
  permissions: string[];
  risk: PermissionRisk;
  scan_severity?: ScanFinding["severity"] | null;
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

export interface SbomComponent {
  name: string;
  version: string;
  license?: string;
}

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
  llm_tokens_per_day?: number | null;
  concurrent_agent_runs?: number | null;
  max_mcp_connections?: number | null;
  day_reset_offset_minutes?: number | null;
}

export interface Webhook {
  id: string;
  url: string;
  events: string[];
  active?: boolean;
}

export interface AuditEvent {
  timestamp_ms: number;
  actor: { principal: string; type: "user" | "service" | "api_key" | "system"; tenant: string };
  action: string;
  resource: { type: string; id: string };
  outcome: "allowed" | "denied" | "error";
  reason?: string;
  request_id?: string;
}

export interface AuditEntry {
  id: string;
  seq: number;
  event: AuditEvent;
  prev_hash: string;
  hash: string;
}

export interface ToolSummary {
  id: string;
  description: string;
  category: string;
  permissions: string[] | null;
}

export type McpTransport =
  | { kind: "stdio"; command: string; args: string[] }
  | { kind: "http"; url: string };

export interface McpConnection {
  name: string;
  transport: McpTransport;
  secret_ref?: string | null;
  secret_env_var?: string | null;
  tool_permissions?: string[] | null;
  created_ms: number;
  updated_ms: number;
}

export interface McpToolInfo {
  name: string;
  description: string;
}

export interface McpConnectionRequest {
  name: string;
  transport: McpTransport;
  secret_ref?: string;
  secret_env_var?: string;
  tool_permissions?: string[];
}

export interface McpConnectionWithTools extends McpConnection {
  tools: McpToolInfo[];
}

export interface McpRefreshResult {
  name: string;
  tools: McpToolInfo[];
}

export interface McpConnectionsPage extends Page<McpConnection> {
  stdio_enabled: boolean;
}

export interface UiFrame {
  schema_version: string;
  title?: string;
  root: unknown;
}

export interface PendingUiFrame {
  frame_id: string;
  execution_id: string | null;
  activity_id: string | null;
  frame: unknown;
  frame_hash: string;
  policy_ref: string;
  created_at_ms: number;
}

export interface UiDecisionRequest {
  action: string;
  values?: Record<string, unknown>;
}

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
