import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import { Page, WorkflowSummary } from '../../core/api.types';

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
}
