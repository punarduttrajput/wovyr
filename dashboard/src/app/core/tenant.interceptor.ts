import { HttpInterceptorFn } from '@angular/common/http';
import { inject } from '@angular/core';
import { Session } from './session';

/**
 * Attaches the tenant + principal headers (and, once one is set, a real
 * `Authorization: Bearer` credential — RM-GA-P4 OBS-805) to platform API calls so
 * the server can authorize them (default-deny RBAC). Scoped to `/api/v1` requests;
 * `/metrics` and `/healthz` are unauthenticated and left untouched.
 *
 * `X-Apex-Tenant`/`X-Apex-Principal` are always sent — the server's default
 * `disabled-loopback` auth mode trusts them verbatim, and even in `apikey`/`jwt`
 * mode `X-Apex-Tenant` is still how the tenant is asserted. When [`Session`] holds
 * an API key/JWT, `Authorization: Bearer <value>` rides along too; the server's
 * `authenticate` middleware then verifies it and overwrites `X-Apex-Principal` with
 * the verified identity, so a stale/spoofed principal header can't win.
 */
export const tenantInterceptor: HttpInterceptorFn = (req, next) => {
  if (!req.url.startsWith('/api/')) return next(req);
  const session = inject(Session);
  const headers: Record<string, string> = {
    'X-Apex-Tenant': session.tenant(),
    'X-Apex-Principal': session.principal(),
  };
  if (session.hasCredential()) {
    headers['Authorization'] = `Bearer ${session.apiKey()}`;
  }
  return next(req.clone({ setHeaders: headers }));
};
