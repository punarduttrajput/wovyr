/** Shapes mirroring the Apex platform API (apex-server). */

/** Declarative model requirement — one of `model` (pinned) or `model_selector`. */
export interface ModelSelector {
  capability: string;
  class: string;
}

/** A draft agent in the studio, before it is serialized to a YAML manifest. */
export interface AgentDraft {
  name: string;
  pinnedModel: string;
  capability: string;
  class: string;
  instructions: string;
  tools: string[];
  /** Configured MCP connection names this agent may draw tools from
   * (`spec.mcp_servers`, PRD-006 MCX-201) — an agent that doesn't name a
   * connection can't reach its tools even if the tenant has it configured.
   * Populated automatically when the author picks an `mcp__<server>__<tool>`
   * entry from the tool picker (MCX-303). */
  mcpServers: string[];
  memoryEnabled: boolean;
  namespace: string;
  /** Default model/tool iteration cap for runs of this agent; `null` = runtime default (8). */
  maxSteps: number | null;
}

/** A registered tool from `GET /api/v1/tools` — built-ins, enabled plugin
 * tools, and (MCX-202) `mcp__<server>__<tool>` entries for the caller's
 * tenant's configured MCP connections. */
export interface ToolInfo {
  id: string;
  description: string;
  category?: string;
  permissions?: string[];
}

/** Cursor-paginated list envelope (overview §6). */
export interface Page<T> {
  data: T[];
  has_more: boolean;
  next_cursor: string | null;
  total_estimate?: number;
}

/** `GET /healthz`. */
export interface Health {
  status: string;
  version: string;
}

/** A workflow execution summary (apex-workflow `ExecutionSummary`). */
export interface WorkflowSummary {
  execution_id: string;
  workflow_name: string;
  workflow_version: string;
  status: string;
  activities: Record<string, string>;
  waiting_on: string[];
}

/** One parsed Prometheus sample line: `name{labels} value`. */
export interface MetricSample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

// ---- tenancy / settings ----

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

/** Built-in roles, snake_case on the wire. */
export type Role = 'viewer' | 'editor' | 'project_admin' | 'org_admin' | 'platform_admin';

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
  /** Daily-reset boundary in minutes east of UTC (SRV-203); empty = UTC midnight. */
  day_reset_offset_minutes?: number | null;
}

export interface Webhook {
  id: string;
  url: string;
  events: string[];
  active?: boolean;
}

// ---- MCP connections (PRD-006, apex-tools::mcp / apex-server mcp.rs) ----

/** How to reach an external MCP server. `stdio` spawns an arbitrary local
 * command (materially higher privilege — gated behind `mcp:admin` + the
 * operator's `APEX_ENABLE_MCP_STDIO=1` opt-in, ADR-0012); `http` POSTs
 * JSON-RPC to a URL (SSRF-guarded the same way `http_get` is, SEC-304). */
export type McpTransport =
  | { kind: 'stdio'; command: string; args: string[] }
  | { kind: 'http'; url: string };

/** A tenant's persisted MCP connection. `secret_ref` is a `SecretRef` string
 * into the secret vault — never a resolved credential value (MCX-105). */
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

/** `POST /api/v1/mcp/connections`'s response: the persisted connection plus
 * the tools discovered while verifying it dials successfully. */
export interface McpConnectionWithTools extends McpConnection {
  tools: McpToolInfo[];
}

// ---- memory ----

export interface MemoryNamespace {
  namespace: string;
  count: number;
}

export interface ScoreBreakdown {
  relevance: number;
  recency: number;
  importance: number;
  total: number;
}

export interface MemoryResult {
  id: string;
  namespace: string;
  content: string;
  type: string;
  importance: number;
  tags: string[];
  score: number;
  breakdown: ScoreBreakdown;
}

// ---- plugins ----

export interface PluginCapability {
  kind: string;
  id: string;
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  publisher: string;
  description: string;
  state: 'enabled' | 'disabled';
  permissions: string[];
  granted: string[];
  capabilities: PluginCapability[];
  platform_api: string;
}

// ---- marketplace (apex-marketplace registry) ----

/** Permission-risk classification of a listing's latest version. */
export type PermissionRisk = 'low' | 'medium' | 'high';

/**
 * A marketplace listing projection (apex-marketplace `Listing`). `capabilities` are the
 * latest version's capability kinds (snake_case strings, e.g. `tool`).
 */
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
  scan_severity?: ScanSeverity | null;
  /** Number of scan findings on the latest version. */
  scan_findings?: number;
  versions: string[];
  channels: Record<string, string>;
  rating: number | null;
  reviews: number;
  installs: number;
  verified: boolean;
}

/** Severity of a security-scan finding (apex-marketplace `Severity`). */
export type ScanSeverity = 'info' | 'warning' | 'critical';

/** One coded security-scan finding (apex-marketplace `Finding`). */
export interface ScanFinding {
  code: string;
  severity: ScanSeverity;
  message: string;
}

/** One SBOM component a package bundles (apex-plugin `SbomComponent`). */
export interface SbomComponent {
  name: string;
  version: string;
  license?: string;
}

/** Build provenance for a package (apex-plugin `Provenance`). */
export interface Provenance {
  builder: string;
  source: string;
  built_at: string;
}

/**
 * A version's supply-chain attestation (`/marketplace/listings/{id}/attestation`):
 * permission risk, SBOM, build provenance, content digest, the operator verified badge,
 * and whether the package signature verifies against the trust store.
 */
export interface PluginAttestation {
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
  /** Live security-scan report (re-run against the current operator deny-list). */
  scan: { findings: ScanFinding[] };
}

/**
 * A normalized run-stream event. The server emits anonymous `data:` frames carrying a
 * `type` discriminator (start/memory/delta/reasoning/tool_call_delta/tool_call/
 * tool_result/done), then a terminal named `result` or `error` event.
 */
export type StreamEvent =
  | { kind: 'start'; model?: string; provider?: string }
  | { kind: 'memory'; source?: string; score?: number }
  | { kind: 'delta'; text: string }
  /** The model's reasoning/thinking channel, where the provider exposes one (AIC-202). */
  | { kind: 'reasoning'; text: string }
  /** An incremental fragment of a tool call's JSON arguments as the model composes
   * it (AIC-202); `arguments` is this frame's fragment, not the whole. */
  | { kind: 'tool_call_delta'; index: number; name: string; arguments: string }
  | { kind: 'tool_call'; name: string; arguments?: unknown }
  | { kind: 'tool_result'; name: string; ok: boolean }
  | { kind: 'done'; usage?: { total_tokens?: number; cost_usd?: number } }
  | { kind: 'result'; status: string; output?: { message?: string }; steps?: number }
  | { kind: 'error'; message: string };
