import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import * as YAML from 'yaml';
import { Page, ToolInfo, WorkflowSummary } from '../../core/api.types';

/** Activity type the visual builder supports. */
export type WfActivityType = 'function' | 'ai' | 'agent' | 'human' | 'wait';

/** One step in the visual workflow draft (a node on the canvas). */
export interface WfActivity {
  id: string;
  type: WfActivityType;
  /** function → tool id · ai → instructions · agent → stored agent id · wait → event name · human → (unused). */
  name: string;
  /** Inputs as a JSON object string (function/ai/agent). */
  inputs: string;
  /** Canvas position (px). Cosmetic — not serialized to the manifest. */
  x: number;
  y: number;
}

/** A `from → to` edge with an optional `when` guard. */
export interface WfTransition {
  from: string;
  to: string;
  when: string;
}

/** The visual workflow definition the form edits (serialized to YAML on run). */
export interface WorkflowDraft {
  name: string;
  version: string;
  activities: WfActivity[];
  transitions: WfTransition[];
}

export interface WorkflowValidation {
  valid: boolean;
  name: string;
  version: string;
  activity_count: number;
  activities: { id: string; type: string; name?: string }[];
  edges: { from: string; to: string; when?: string }[];
}

export interface SubmitResult {
  execution_id: string;
  status: string;
}

export interface ExecutionDetail {
  execution: WorkflowSummary;
  events: unknown[];
}

/** Client for the workflow-builder routes on wovyr-server. */
@Injectable({ providedIn: 'root' })
export class WorkflowService {
  private http = inject(HttpClient);

  /** Parse the YAML definition and return a DAG summary, or throw on validation error. */
  validate(manifest: string): Observable<WorkflowValidation> {
    return this.http.post<WorkflowValidation>('/api/v1/workflows/validate', { manifest });
  }

  /** Submit a workflow run; returns immediately with an execution_id. */
  submit(manifest: string, input: Record<string, unknown>, executionId?: string): Observable<SubmitResult> {
    return this.http.post<SubmitResult>('/api/v1/workflows', {
      manifest,
      input,
      ...(executionId ? { execution_id: executionId } : {}),
    });
  }

  /** Cursor-paginated list of executions. */
  list(limit = 25): Observable<Page<WorkflowSummary>> {
    return this.http.get<Page<WorkflowSummary>>(`/api/v1/workflows?limit=${limit}`);
  }

  /** Status + event timeline for one execution. */
  execution(id: string): Observable<ExecutionDetail> {
    return this.http.get<ExecutionDetail>(`/api/v1/workflows/${encodeURIComponent(id)}`);
  }

  /** Deliver a named event to a waiting execution. */
  signal(id: string, manifest: string, event: string, payload: unknown): Observable<unknown> {
    return this.http.post(`/api/v1/workflows/${encodeURIComponent(id)}/signal`, {
      manifest,
      event,
      payload,
    });
  }

  /** Approve a suspended human activity. */
  approve(id: string, manifest: string, activityId: string, decision: unknown): Observable<unknown> {
    return this.http.post(`/api/v1/workflows/${encodeURIComponent(id)}/approve`, {
      manifest,
      activity_id: activityId,
      decision,
    });
  }

  /** Cancel an execution (advisory). */
  cancel(id: string): Observable<void> {
    return this.http.delete<void>(`/api/v1/workflows/${encodeURIComponent(id)}`);
  }

  /** Summaries of all executions (unwrapped data array). */
  listAll(limit = 25): Observable<WorkflowSummary[]> {
    return this.list(limit).pipe(map((p) => p.data ?? []));
  }

  /** The registered tool catalog (for `function` activities' tool picker).
   * `GET /api/v1/tools` returns the standard cursor-pagination envelope
   * (`{data, has_more, ...}`, RM-GA-P4 API-701) — this used to read a bare
   * `{tools: [...]}` shape (the same bug already fixed in `AgentService.tools()`),
   * so the live catalog silently never replaced the canvas's hardcoded seeds. */
  tools(): Observable<ToolInfo[]> {
    return this.http.get<Page<ToolInfo>>('/api/v1/tools').pipe(map((r) => r.data ?? []));
  }

  /** The caller's stored agent ids (for `agent` activities' agent picker). */
  agents(): Observable<string[]> {
    return this.http.get<Page<string>>('/api/v1/agents?limit=100').pipe(map((p) => p.data ?? []));
  }

  /**
   * Serialize a visual [`WorkflowDraft`] to the workflow-engine YAML DSL, so a user never
   * has to write YAML. Field mapping per activity `type`:
   * - `function` → `name` is the tool id, `inputs` its params.
   * - `ai`       → `name` is the system prompt, emitted as `inputs.prompt` (where the
   *   runtime reads it); `inputs.message`/`inputs.text` the user turn. An explicit
   *   `prompt` in the inputs JSON wins over the form field.
   * - `agent`    → `name` is a *stored* agent id (registered via `POST /api/v1/agents`),
   *   `inputs.message` its run input — runs the full model/tool loop, not a bare chat call.
   * - `wait`     → suspends on an event named by `name` (`inputs: { event: <name> }`).
   * - `human`    → suspends for approval (no fields).
   */
  toWorkflowManifest(d: WorkflowDraft): string {
    // UI-302: emit through the `yaml` library from a plain object — quoting and
    // escaping are the library's problem, not a hand-rolled `q()` helper's.
    const parseInputs = (json: string): Record<string, unknown> | null => {
      const t = (json || '').trim();
      if (!t || t === '{}') return null;
      try {
        const v = JSON.parse(t);
        return v && typeof v === 'object' && !Array.isArray(v) ? v : null;
      } catch {
        return null;
      }
    };

    const activities = d.activities.map((a) => {
      const id = a.id.trim() || 'step';
      const out: Record<string, unknown> = { id, type: a.type };
      if (a.type === 'wait') {
        out['inputs'] = { event: a.name.trim() || id };
      } else if (a.type === 'ai') {
        // The instructions belong in `inputs.prompt`, which is where the runtime
        // reads the system message (crates/wovyr-runtime/src/lib.rs's `ai` branch).
        // They were emitted as the activity's `name` until 2026-08-05 — the
        // pre-HLTH-901 server convention, which that ticket replaced with the CLI's
        // `inputs.prompt`. Nothing read `name` for an `ai` step any more, so
        // everything typed into the instructions box was silently discarded and the
        // step ran with the runtime's default "You are a helpful assistant."
        // An explicit `prompt` in the inputs JSON wins, so the raw editor stays
        // authoritative over the form field.
        const inputs = parseInputs(a.inputs) ?? {};
        const instructions = a.name.trim();
        if (instructions && (inputs as Record<string, unknown>)['prompt'] === undefined) {
          (inputs as Record<string, unknown>)['prompt'] = instructions;
        }
        if (Object.keys(inputs as Record<string, unknown>).length) out['inputs'] = inputs;
      } else {
        if (a.name.trim()) out['name'] = a.name.trim();
        const inputs = parseInputs(a.inputs);
        if (inputs) out['inputs'] = inputs;
      }
      return out;
    });

    const spec: Record<string, unknown> = { activities };
    const edges = d.transitions
      .filter((t) => t.from.trim() && t.to.trim())
      .map((t) => ({
        from: t.from.trim(),
        to: t.to.trim(),
        ...(t.when.trim() ? { when: t.when.trim() } : {}),
      }));
    if (edges.length) spec['transitions'] = edges;

    return YAML.stringify(
      {
        metadata: {
          name: d.name.trim() || 'untitled-workflow',
          version: d.version.trim() || '1.0.0',
        },
        spec,
      },
      { lineWidth: 0 },
    );
  }
}
