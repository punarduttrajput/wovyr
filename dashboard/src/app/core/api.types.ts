/**
 * Shapes mirroring the Wovyr platform API (wovyr-server).
 *
 * UI-302: every wire shape is re-exported from the TypeScript SDK's `types.ts`
 * (via the `@wovyr/sdk-types` tsconfig path alias) — one source of truth, so
 * the dashboard cannot drift from the published client. Only UI-local shapes
 * (drafts, normalized stream events, parsed metric samples) are defined here.
 */

export type {
  AuditEntry,
  AuditPage,
  Health,
  MarketplaceListing,
  McpConnection,
  McpConnectionWithTools,
  McpToolInfo,
  McpTransport,
  Membership,
  Organization,
  Page,
  PermissionRisk,
  Project,
  Provenance,
  QuotaLimits,
  Role,
  SbomComponent,
  ScanFinding,
  Webhook,
  WorkflowSummary,
} from '@wovyr/sdk-types';

import type {
  Attestation,
  CapabilityDescriptor,
  MemoryQueryResult,
  PluginSummary,
  ScanFinding,
  ToolSummary,
} from '@wovyr/sdk-types';

/** SDK names differ from this app's originals in a few places — keep the local
 * names as aliases so feature code reads naturally. */
export type ToolInfo = ToolSummary;
export type PluginInfo = PluginSummary;
export type PluginCapability = CapabilityDescriptor;
export type PluginAttestation = Attestation;
export type MemoryResult = MemoryQueryResult;
export type ScoreBreakdown = MemoryQueryResult['breakdown'];
export type ScanSeverity = ScanFinding['severity'];

// ---- UI-local shapes (not wire types) ----

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

/** One parsed Prometheus sample line: `name{labels} value`. */
export interface MetricSample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

/** A memory namespace aggregate the explorer's sidebar lists. */
export interface MemoryNamespace {
  namespace: string;
  count: number;
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
