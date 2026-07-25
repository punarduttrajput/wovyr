import { Injectable, inject } from '@angular/core';
import { HttpClient, HttpParams } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import { MarketplaceListing, PluginAttestation, PluginInfo } from '../../core/api.types';

/**
 * Client for the plugin + marketplace routes on wovyr-server: the installed catalog and
 * full lifecycle (`/api/v1/plugins*`), plus the hosted registry discovery/install
 * surface (`/api/v1/marketplace*`).
 */
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

  /** Install a plugin from a base64-encoded `.wovyrpkg` file. */
  install(wovyrpkg: string, grants: string[] = []): Observable<PluginInfo> {
    return this.http.post<PluginInfo>('/api/v1/plugins:install', { wovyrpkg, grants });
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

  // ── marketplace registry (discovery + install) ───────────────────────────────

  /** Search/browse published listings (full-text + category/capability filters). */
  searchListings(text = '', category = '', capability = ''): Observable<MarketplaceListing[]> {
    let params = new HttpParams();
    if (text.trim()) params = params.set('q', text.trim());
    if (category.trim()) params = params.set('category', category.trim());
    if (capability.trim()) params = params.set('capability', capability.trim());
    return this.http
      .get<{ listings: MarketplaceListing[] }>('/api/v1/marketplace/listings', { params })
      .pipe(map((r) => r.listings ?? []));
  }

  /** One listing's detail (`id` is `publisher/name`). */
  getListing(id: string): Observable<MarketplaceListing> {
    return this.http.get<MarketplaceListing>(
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}`,
    );
  }

  /** A version's supply-chain attestation (risk + SBOM + provenance + signature status). */
  attestation(id: string, version = ''): Observable<PluginAttestation> {
    let params = new HttpParams();
    if (version.trim()) params = params.set('version', version.trim());
    return this.http.get<PluginAttestation>(
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/attestation`,
      { params },
    );
  }

  /** Download a listed package and install it locally (disabled). */
  installFromMarketplace(
    id: string,
    version: string | undefined,
    grants: string[],
  ): Observable<{ id: string; status: string; plugin: PluginInfo }> {
    return this.http.post<{ id: string; status: string; plugin: PluginInfo }>(
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/install`,
      { version, grants },
    );
  }

  /** Submit a 1–5 star review for a listing. */
  review(
    id: string,
    author: string,
    rating: number,
    body: string,
  ): Observable<{ id: string; status: string }> {
    return this.http.post<{ id: string; status: string }>(
      `/api/v1/marketplace/listings/${encodeURIComponent(id)}/reviews`,
      { author, rating, body },
    );
  }
}
