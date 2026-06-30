import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import { PluginInfo } from '../../core/api.types';

/** Client for the plugin routes on apex-server (installed catalog + full lifecycle). */
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

  /** Install a plugin from a base64-encoded `.apexpkg` file. */
  install(apexpkg: string, grants: string[] = []): Observable<PluginInfo> {
    return this.http.post<PluginInfo>('/api/v1/plugins:install', { apexpkg, grants });
  }

  /** Uninstall a plugin by its qualified id (`publisher/name`). */
  uninstall(id: string): Observable<{ id: string; status: string }> {
    return this.http.delete<{ id: string; status: string }>(
      `/api/v1/plugins/${encodeURIComponent(id)}`,
    );
  }

  /** Register a trusted publisher's ed25519 public key (hex-encoded). */
  trustPublisher(publisher: string, publicKeyHex: string): Observable<{ publisher: string; status: string }> {
    return this.http.post<{ publisher: string; status: string }>('/api/v1/plugins:trust', {
      publisher,
      public_key_hex: publicKeyHex,
    });
  }
}
