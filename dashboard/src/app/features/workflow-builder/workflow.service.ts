import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
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

/** Client for the workflow-builder routes on apex-server. */
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

  /** The registered tool catalog (for `function` activities' tool picker). */
  tools(): Observable<ToolInfo[]> {
    return this.http
      .get<{ tools: ToolInfo[] }>('/api/v1/tools')
      .pipe(map((r) => r.tools ?? []));
  }

  /** The caller's stored agent ids (for `agent` activities' agent picker). */
  agents(): Observable<string[]> {
    return this.http.get<Page<string>>('/api/v1/agents?limit=100').pipe(map((p) => p.data ?? []));
  }

  /**
   * Serialize a visual [`WorkflowDraft`] to the workflow-engine YAML DSL, so a user never
   * has to write YAML. Field mapping per activity `type`:
   * - `function` → `name` is the tool id, `inputs` its params.
   * - `ai`       → `name` is the system prompt (instructions), `inputs.message` the input.
   * - `agent`    → `name` is a *stored* agent id (registered via `POST /api/v1/agents`),
   *   `inputs.message` its run input — runs the full model/tool loop, not a bare chat call.
   * - `wait`     → suspends on an event named by `name` (`inputs: { event: <name> }`).
   * - `human`    → suspends for approval (no fields).
   */
  toWorkflowManifest(d: WorkflowDraft): string {
    const q = (s: string) => `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
    // Compact a JSON-object string to a single-line YAML flow map (JSON ⊂ YAML).
    const compact = (json: string): string | null => {
      const t = (json || '').trim();
      if (!t || t === '{}') return null;
      try {
        return JSON.stringify(JSON.parse(t));
      } catch {
        return null;
      }
    };

    const lines = [
      'metadata:',
      `  name: ${d.name.trim() || 'untitled-workflow'}`,
      `  version: ${q(d.version.trim() || '1.0.0')}`,
      'spec:',
      '  activities:',
    ];
    for (const a of d.activities) {
      const id = a.id.trim() || 'step';
      lines.push(`    - id: ${id}`, `      type: ${a.type}`);
      if (a.type === 'wait') {
        lines.push(`      inputs: { event: ${q(a.name.trim() || id)} }`);
      } else {
        if (a.name.trim()) lines.push(`      name: ${q(a.name.trim())}`);
        const inputs = compact(a.inputs);
        if (inputs) lines.push(`      inputs: ${inputs}`);
      }
    }

    const edges = d.transitions.filter((t) => t.from.trim() && t.to.trim());
    if (edges.length) {
      lines.push('  transitions:');
      for (const t of edges) {
        lines.push(`    - from: ${t.from.trim()}`, `      to: ${t.to.trim()}`);
        if (t.when.trim()) lines.push(`      when: ${q(t.when.trim())}`);
      }
    }
    return lines.join('\n') + '\n';
  }
}
