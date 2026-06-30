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
  memoryEnabled: boolean;
  namespace: string;
}

/** A registered tool from `GET /api/v1/tools` (built-in or enabled plugin). */
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
  tool_executions_per_minute?: number | null;
  memory_records?: number | null;
  concurrent_agent_runs?: number | null;
}

export interface Webhook {
  id: string;
  url: string;
  events: string[];
  active?: boolean;
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
  versions: string[];
  channels: Record<string, string>;
  rating: number | null;
  reviews: number;
  installs: number;
  verified: boolean;
}

/**
 * A normalized run-stream event. The server emits anonymous `data:` frames carrying a
 * `type` discriminator (start/memory/delta/tool_call/tool_result/done), then a terminal
 * named `result` or `error` event.
 */
export type StreamEvent =
  | { kind: 'start'; model?: string; provider?: string }
  | { kind: 'memory'; source?: string; score?: number }
  | { kind: 'delta'; text: string }
  | { kind: 'tool_call'; name: string; arguments?: unknown }
  | { kind: 'tool_result'; name: string; ok: boolean }
  | { kind: 'done'; usage?: { total_tokens?: number; cost_usd?: number } }
  | { kind: 'result'; status: string; output?: { message?: string }; steps?: number }
  | { kind: 'error'; message: string };
