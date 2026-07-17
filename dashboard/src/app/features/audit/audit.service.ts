import { Injectable, inject } from '@angular/core';
import { HttpClient, HttpParams } from '@angular/common/http';
import { Observable } from 'rxjs';
import { silentErrors } from '../../core/http-error';
import { AuditEntry, AuditPage } from '../../core/api.types';

/** Filters for `GET /api/v1/audit` (SEC-301). Timestamps are epoch ms, inclusive. */
export interface AuditQuery {
  principal?: string;
  action?: string;
  after_ms?: number;
  before_ms?: number;
  limit?: number;
  cursor?: string;
}

/**
 * UI-303: client for the tamper-evident, hash-chained audit trail — tenant-scoped,
 * most-recent first, cursor-paginated. `total_estimate` is always `null` on this
 * route (an exact count would need the full-log scan the paged query avoids).
 *
 * Errors are handled inline by the viewer (a 403 renders the RBAC explanation,
 * not a toast), so the global error interceptor is silenced here.
 */
@Injectable({ providedIn: 'root' })
export class AuditService {
  private http = inject(HttpClient);

  query(q: AuditQuery): Observable<AuditPage<AuditEntry>> {
    let params = new HttpParams();
    for (const [k, v] of Object.entries(q)) {
      if (v !== undefined && v !== null && `${v}` !== '') params = params.set(k, `${v}`);
    }
    return this.http.get<AuditPage<AuditEntry>>('/api/v1/audit', {
      params,
      context: silentErrors(),
    });
  }
}
