import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import {
  McpConnection,
  McpConnectionWithTools,
  McpTransport,
  Page,
} from '../../core/api.types';

/** `GET /api/v1/mcp/connections`'s envelope: the standard `Page` plus
 * `stdio_enabled` (RM-MCX-P3-302) — whether the operator has set
 * `APEX_ENABLE_MCP_STDIO=1`, so the panel knows to hide the `stdio` transport
 * option before the operator fills out a form, not after a rejected submit. */
interface McpConnectionsPage extends Page<McpConnection> {
  stdio_enabled: boolean;
}

/** `POST /api/v1/mcp/connections/{name}/refresh`'s response. */
export interface McpRefreshResult {
  name: string;
  tools: { name: string; description: string }[];
}

/**
 * Client for the MCP connection-management routes (PRD-006, RM-MCX-P1-102):
 * persisted, tenant-scoped external MCP server connections. Every call goes
 * through the dashboard's `tenantInterceptor` like any other `/api/*`
 * request, so the operator's own session is what actually gets RBAC-checked
 * server-side (`mcp:read`/`mcp:write`, plus `mcp:admin` for a `stdio`
 * connection — ADR-0012).
 */
@Injectable({ providedIn: 'root' })
export class McpServersService {
  private http = inject(HttpClient);

  /** `GET /api/v1/mcp/connections` — the caller's tenant's configured
   * connections, plus whether `stdio` is operator-enabled. */
  list(): Observable<{ connections: McpConnection[]; stdioEnabled: boolean }> {
    return this.http.get<McpConnectionsPage>('/api/v1/mcp/connections').pipe(
      map((p) => ({ connections: p.data ?? [], stdioEnabled: !!p.stdio_enabled })),
    );
  }

  /** `POST /api/v1/mcp/connections` — verifies the connection actually dials
   * (and resolves any `secret_ref`) before persisting. */
  create(req: {
    name: string;
    transport: McpTransport;
    secret_ref?: string;
    secret_env_var?: string;
  }): Observable<McpConnectionWithTools> {
    return this.http.post<McpConnectionWithTools>('/api/v1/mcp/connections', req);
  }

  /** `DELETE /api/v1/mcp/connections/{name}` — takes effect immediately. */
  delete(name: string): Observable<void> {
    return this.http.delete<void>(`/api/v1/mcp/connections/${encodeURIComponent(name)}`);
  }

  /** `POST /api/v1/mcp/connections/{name}/refresh` (MCX-203) — force an
   * immediate re-dial and re-discovery, bypassing the client cache. */
  refresh(name: string): Observable<McpRefreshResult> {
    return this.http.post<McpRefreshResult>(
      `/api/v1/mcp/connections/${encodeURIComponent(name)}/refresh`,
      {},
    );
  }
}
