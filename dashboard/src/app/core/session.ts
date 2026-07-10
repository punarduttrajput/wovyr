import { Injectable, signal } from '@angular/core';

/**
 * The identity the dashboard acts as when calling the tenancy-scoped platform API
 * (RM-GA-P4 OBS-805). An operator sets these from the **Sign in** page; the
 * non-secret tenant/principal persist in `localStorage`, so switching them (or
 * adding a real API key) never requires rebuilding the app.
 *
 * **Credential storage (RM-AIM-P1 UI-101):** the bearer credential (API key or JWT)
 * deliberately does NOT live in `localStorage` — anything there is readable by any
 * script that ever runs in this origin, making an XSS one `localStorage.getItem`
 * away from a long-lived credential theft. It lives in `sessionStorage` instead:
 * still survives page reloads within the tab (the chosen auth-persistence model),
 * but scoped to the tab's lifetime and gone when it closes. A legacy
 * pre-UI-101 `localStorage` blob that still carries an `apiKey` is migrated on
 * first load — the key is adopted into `sessionStorage` and **scrubbed** from the
 * persisted blob, so upgraded users don't keep a residual copy in the weaker store.
 * (An httpOnly-cookie BFF remains the preferred end state, but the dashboard talks
 * directly to apex-server today — BFF deferred, per the dashboard's own docs.)
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
/** sessionStorage key for the bearer credential (UI-101). */
const CREDENTIAL_KEY = 'apex.credential.v1';

interface StoredSession {
  tenant: string;
  principal: string;
  /** Only ever present in a legacy (pre-UI-101) blob — migrated + scrubbed on load. */
  apiKey?: string;
}

const DEFAULTS: StoredSession = {
  tenant: 'acme',
  principal: 'admin@apex.local',
};

/** Load tenant/principal from localStorage, migrating a legacy embedded apiKey. */
function load(): { tenant: string; principal: string; apiKey: string } {
  let tenant = DEFAULTS.tenant;
  let principal = DEFAULTS.principal;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<StoredSession>;
      tenant = parsed.tenant ?? tenant;
      principal = parsed.principal ?? principal;
      if (parsed.apiKey) {
        // Legacy blob: adopt the key into sessionStorage (unless a newer one is
        // already there), then rewrite the persisted blob WITHOUT it (UI-101).
        try {
          if (!sessionStorage.getItem(CREDENTIAL_KEY)) {
            sessionStorage.setItem(CREDENTIAL_KEY, parsed.apiKey);
          }
        } catch {
          // sessionStorage unavailable — the scrub below still removes the copy.
        }
        localStorage.setItem(STORAGE_KEY, JSON.stringify({ tenant, principal }));
      }
    }
  } catch {
    // Storage unavailable/corrupt — fall back to defaults.
  }
  let apiKey = '';
  try {
    apiKey = sessionStorage.getItem(CREDENTIAL_KEY) ?? '';
  } catch {
    // sessionStorage unavailable — in-memory only for this session.
  }
  return { tenant, principal, apiKey };
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
    // Non-secret identity → localStorage (survives browser restarts). The
    // credential is deliberately never written here (UI-101).
    const value: StoredSession = {
      tenant: this.tenant(),
      principal: this.principal(),
    };
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
    } catch {
      // Storage unavailable (private browsing, quota) — the in-memory signals above
      // still hold the value for the rest of this session.
    }
    // Credential → sessionStorage only: survives reloads in this tab, dies with it.
    try {
      if (this.apiKey()) {
        sessionStorage.setItem(CREDENTIAL_KEY, this.apiKey());
      } else {
        sessionStorage.removeItem(CREDENTIAL_KEY);
      }
    } catch {
      // Same fallback: in-memory signal still carries it for this session.
    }
  }
}
