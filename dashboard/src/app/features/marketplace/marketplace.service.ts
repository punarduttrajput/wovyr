import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import { PluginInfo } from '../../core/api.types';

/** Client for the plugin routes on apex-server (installed catalog + enable/disable). */
@Injectable({ providedIn: 'root' })
export class MarketplaceService {
  private http = inject(HttpClient);

  list(): Observable<PluginInfo[]> {
    return this.http
      .get<{ plugins: PluginInfo[] }>('/api/v1/plugins')
      .pipe(map((r) => r.plugins ?? []));
  }

  enable(id: string): Observable<{ id: string; state: string }> {
    return this.http.post<{ id: string; state: string }>('/api/v1/plugins:enable', { id });
  }

  disable(id: string): Observable<{ id: string; state: string }> {
    return this.http.post<{ id: string; state: string }>('/api/v1/plugins:disable', { id });
  }
}
