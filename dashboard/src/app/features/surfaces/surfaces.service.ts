import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';

/** A validated generative-UI frame document (PRD-005 UIP-101). `root` is left
 * `unknown` here — this service only needs to pass it through to `POST
 * /api/v1/ui/present`; `<apex-ui-frame>` (`@apex/ui-react/web-component`) owns
 * the typed vocabulary and rendering. */
export interface UiFrame {
  schema_version: string;
  title?: string;
  root: unknown;
}

/** `POST /api/v1/ui/present` / `GET /api/v1/ui/frames[/{id}]` envelope
 * (RM-GUI-P3 EMB-701). `execution_id`/`activity_id` are `null` for a
 * standalone-presented frame — there is no workflow behind it at all. */
export interface PendingUiFrame {
  frame_id: string;
  execution_id: string | null;
  activity_id: string | null;
  frame: UiFrame;
  frame_hash: string;
  policy_ref: string;
  created_at_ms: number;
}

export interface UiDecisionResult {
  frame_id: string;
  execution_id: string | null;
  activity_id: string | null;
  status: 'decided';
}

/** `GET /api/v1/ui/decisions/{frame_id}` — a standalone frame's recorded
 * decision, retrievable after the pending record is gone. */
export interface UiDecisionOutcome {
  frame_id: string;
  action: string;
  values: Record<string, unknown>;
  decided_by: string;
  decided_at_ms: number;
  frame_hash: string;
}

/** Client for the generative-UI *standalone middleware* routes (PRD-005
 * RM-GUI-P3 EMB-701) — present, decide, and retrieve a trust-layer-governed
 * frame with no workflow or agent adoption at all. Every call goes through
 * the dashboard's `tenantInterceptor` like any other `/api/*` request, so the
 * operator's own session (`X-Apex-Tenant`/`X-Apex-Principal`/bearer
 * credential) is what actually gets RBAC-checked server-side — this panel
 * exercises the exact same `ui:read`/`ui:write` scopes a partner's own
 * integration would need. */
@Injectable({ providedIn: 'root' })
export class SurfacesService {
  private http = inject(HttpClient);

  /** `POST /api/v1/ui/present` — a `403` means the trust layer blocked the
   * frame (`crates/apex-server/src/ui.rs`'s `present_handler`); the error
   * body's `error.message` names which rule fired — surface it, don't treat
   * it as a transport failure. */
  present(frame: UiFrame): Observable<PendingUiFrame> {
    return this.http.post<PendingUiFrame>('/api/v1/ui/present', { frame });
  }

  /** `POST /api/v1/ui/decisions/{frame_id}` — validated fail-closed against
   * the frame's declared actions/inputs before anything is recorded. */
  decide(
    frameId: string,
    action: string,
    values: Record<string, unknown> = {},
  ): Observable<UiDecisionResult> {
    return this.http.post<UiDecisionResult>(
      `/api/v1/ui/decisions/${encodeURIComponent(frameId)}`,
      { action, values },
    );
  }

  /** `GET /api/v1/ui/decisions/{frame_id}` — the recorded outcome, once decided. */
  getDecision(frameId: string): Observable<UiDecisionOutcome> {
    return this.http.get<UiDecisionOutcome>(
      `/api/v1/ui/decisions/${encodeURIComponent(frameId)}`,
    );
  }
}
