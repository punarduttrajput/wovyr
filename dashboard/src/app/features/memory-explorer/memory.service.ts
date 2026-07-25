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

  query(req: QueryRequest): Observable<MemoryResult[]> {
    return this.http
      .post<{ results: MemoryResult[] }>('/api/v1/memory:query', req)
      .pipe(map((r) => r.results ?? []));
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
