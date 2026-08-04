import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import { MemoryNamespace, MemoryResult, Page } from '../../core/api.types';

export interface QueryRequest {
  text: string;
  namespace?: string;
  strategy?: 'hybrid' | 'vector' | 'keyword';
  limit?: number;
  diversity?: number;
  relevance?: number;
  recency?: number;
  importance?: number;
  grants?: string[];
}

/** Client for the memory-explorer routes on wovyr-server. */
@Injectable({ providedIn: 'root' })
export class MemoryService {
  private http = inject(HttpClient);

  namespaces(): Observable<MemoryNamespace[]> {
    return this.http
      .get<{ namespaces: MemoryNamespace[] }>('/api/v1/memory/namespaces')
      .pipe(map((r) => r.namespaces ?? []));
  }

  records(namespace?: string, limit = 50): Observable<MemoryResult[]> {
    const ns = namespace ? `&namespace=${encodeURIComponent(namespace)}` : '';
    return this.http
      .get<Page<MemoryResult>>(`/api/v1/memory/records?limit=${limit}${ns}`)
      .pipe(map((p) => p.data ?? []));
  }

  /**
   * `POST /api/v1/memory:query` answers `{data, count}` — not `{results, count}`,
   * which is what this parsed until 2026-08-04. API-701 renamed the field when it
   * standardised every list route's envelope; the SDKs and openapi.yaml moved, this
   * did not, so `r.results` was always `undefined` and `?? []` turned it into an
   * empty array. Memory Explorer's search therefore reported "0 matches" for every
   * query since that rename, no matter what the server returned — while browsing
   * records kept working (see `records()` above, which reads `data` correctly),
   * which is exactly why it looked healthy.
   *
   * Deliberately *not* `?? []` here: a shape mismatch should surface as an error
   * rather than an empty result set. That fallback is what hid this for so long.
   * The route is the one documented exception to the cursor envelope — a ranked
   * top-K set has no stable order to page through — so there is no `has_more`
   * or `next_cursor` to read, only `data`.
   */
  query(req: QueryRequest): Observable<MemoryResult[]> {
    return this.http
      .post<{ data: MemoryResult[]; count: number }>('/api/v1/memory:query', req)
      .pipe(map((r) => r.data));
  }

  put(body: {
    namespace: string;
    content: string;
    type?: string;
    importance?: number;
    tags?: string[];
  }): Observable<{ id: string }> {
    return this.http.post<{ id: string }>('/api/v1/memory/records', body);
  }
}
