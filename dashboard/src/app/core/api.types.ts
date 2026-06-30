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
