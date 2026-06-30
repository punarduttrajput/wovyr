/**
 * Identity the dashboard acts as when calling the tenancy-scoped API. The server
 * authorizes each request from these headers (`X-Apex-Tenant` / `X-Apex-Principal`)
 * and treats principals listed in its `APEX_PLATFORM_ADMINS` env var as platform
 * admins. For local dev, run the server with:
 *
 *   APEX_PLATFORM_ADMINS=admin@apex.local apex dev
 *
 * so this principal can administer orgs/projects/quotas. When the BFF lands, these are
 * replaced by the authenticated session's real identity.
 */
export const TENANT = 'acme';
export const PRINCIPAL = 'admin@apex.local';
