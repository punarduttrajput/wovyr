import { HttpInterceptorFn } from '@angular/common/http';
import { PRINCIPAL, TENANT } from './tenant.config';

/**
 * Attaches the tenant + principal headers to platform API calls so the server can
 * authorize them (default-deny RBAC). Scoped to `/api/v1` requests; `/metrics` and
 * `/healthz` are unauthenticated and left untouched.
 */
export const tenantInterceptor: HttpInterceptorFn = (req, next) => {
  if (!req.url.startsWith('/api/')) return next(req);
  return next(
    req.clone({
      setHeaders: { 'X-Apex-Tenant': TENANT, 'X-Apex-Principal': PRINCIPAL },
    }),
  );
};
