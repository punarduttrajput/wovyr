import { Injectable, signal } from '@angular/core';

/**
 * The identity the dashboard acts as when calling the tenancy-scoped platform API
 * (RM-GA-P4 OBS-805). Replaces the previous hardcoded `TENANT`/`PRINCIPAL` build-time
 * constants — an operator now sets these from the **Sign in** page and they persist
 * in `localStorage`, so switching tenant/principal (or adding a real API key) no
 * longer requires rebuilding the app.
 *
 * Three credential shapes exist server-side (`crates/apex-server/src/auth.rs`):
 * `disabled-loopback` (the default dev mode — trusts `X-Apex-Tenant`/
 * `X-Apex-Principal` verbatim, no real verification), `apikey` (a bearer token minted
 * via `apex auth create-key`, verified by `authenticate` which then *overwrites*
 * `X-Apex-Principal` from the verified key), and `jwt` (a pre-issued bearer token).
 * There is no username/password login endpoint anywhere in the platform — an
 * operator who wants real auth pastes an already-minted API key or JWT here, rather
 * than the dashboard collecting a password it has nowhere to send. `apiKey` is sent
 * as `Authorization: Bearer <value>` by [`tenantInterceptor`](./tenant.interceptor.ts)
 * whichever credential type it is; the tenant is a separate field because a bearer
 * credential resolves a *principal*, not a tenant.
 */
const STORAGE_KEY = 'apex.session.v1';

interface StoredSession {
  tenant: string;
  principal: string;
  apiKey: string;
}

const DEFAULTS: StoredSession = {
  tenant: 'acme',
  principal: 'admin@apex.local',
  apiKey: '',
};

function load(): StoredSession {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<StoredSession>;
    return { ...DEFAULTS, ...parsed };
  } catch {
    return { ...DEFAULTS };
  }
}

@Injectable({ providedIn: 'root' })
export class Session {
  private readonly initial = load();

  readonly tenant = signal(this.initial.tenant);
  readonly principal = signal(this.initial.principal);
  readonly apiKey = signal(this.initial.apiKey);

  /** Whether a real bearer credential (API key or JWT) has been supplied. */
  readonly hasCredential = () => this.apiKey().trim().length > 0;

  save(tenant: string, principal: string, apiKey: string): void {
    this.tenant.set(tenant.trim());
    this.principal.set(principal.trim());
    this.apiKey.set(apiKey.trim());
    const value: StoredSession = {
      tenant: this.tenant(),
      principal: this.principal(),
      apiKey: this.apiKey(),
    };
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
    } catch {
      // Storage unavailable (private browsing, quota) — the in-memory signals above
      // still hold the value for the rest of this session.
    }
  }
}
