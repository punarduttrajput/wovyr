import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import {
  Membership,
  Organization,
  Page,
  Project,
  QuotaLimits,
  Role,
  Webhook,
} from '../../core/api.types';

/** Client for the tenancy + webhooks routes on wovyr-server. */
@Injectable({ providedIn: 'root' })
export class SettingsService {
  private http = inject(HttpClient);

  // --- organizations ---
  listOrgs(): Observable<Organization[]> {
    return this.http
      .get<Page<Organization>>('/api/v1/organizations')
      .pipe(map((p) => p.data ?? []));
  }
  createOrg(name: string): Observable<Organization> {
    return this.http.post<Organization>('/api/v1/organizations', { name });
  }

  // --- projects ---
  listProjects(): Observable<Project[]> {
    return this.http.get<Page<Project>>('/api/v1/projects').pipe(map((p) => p.data ?? []));
  }
  createProject(name: string, organization: string): Observable<Project> {
    return this.http.post<Project>('/api/v1/projects', { name, organization });
  }
  deleteProject(id: string): Observable<void> {
    return this.http.delete<void>(`/api/v1/projects/${encodeURIComponent(id)}`);
  }

  // --- members ---
  listMembers(projectId: string): Observable<Membership[]> {
    return this.http
      .get<{ members: Membership[] }>(`/api/v1/projects/${encodeURIComponent(projectId)}/members`)
      .pipe(map((r) => r.members ?? []));
  }
  addMember(projectId: string, user: string, role: Role): Observable<Membership> {
    return this.http.post<Membership>(
      `/api/v1/projects/${encodeURIComponent(projectId)}/members`,
      { user, role },
    );
  }
  removeMember(projectId: string, uid: string): Observable<void> {
    return this.http.delete<void>(
      `/api/v1/projects/${encodeURIComponent(projectId)}/members/${encodeURIComponent(uid)}`,
    );
  }

  // --- quotas ---
  getQuota(projectId: string): Observable<QuotaLimits> {
    return this.http
      .get<{ limits: QuotaLimits }>(`/api/v1/projects/${encodeURIComponent(projectId)}/quota`)
      .pipe(map((r) => r.limits ?? {}));
  }
  setQuota(projectId: string, limits: QuotaLimits): Observable<QuotaLimits> {
    return this.http
      .patch<{ limits: QuotaLimits }>(
        `/api/v1/projects/${encodeURIComponent(projectId)}/quota`,
        limits,
      )
      .pipe(map((r) => r.limits ?? {}));
  }

  // --- webhooks ---
  listWebhooks(): Observable<Webhook[]> {
    return this.http.get<Page<Webhook>>('/api/v1/webhooks').pipe(map((p) => p.data ?? []));
  }
  createWebhook(url: string, events: string[], secret: string): Observable<Webhook> {
    return this.http.post<Webhook>('/api/v1/webhooks', { url, events, secret });
  }
  deleteWebhook(id: string): Observable<void> {
    return this.http.delete<void>(`/api/v1/webhooks/${encodeURIComponent(id)}`);
  }
}
