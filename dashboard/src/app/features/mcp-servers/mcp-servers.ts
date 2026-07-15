import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { forkJoin, of } from 'rxjs';
import { catchError, map } from 'rxjs/operators';
import { McpServersService } from './mcp-servers.service';
import { McpConnection, McpTransport } from '../../core/api.types';

type TransportKind = 'stdio' | 'http';

/**
 * Dashboard "MCP Servers" panel (PRD-006 RM-MCX-P3-302): connect an external
 * MCP server once, grant it to agents by name (`spec.mcp_servers`, wired into
 * Agent Studio's tool picker — MCX-303), no Rust/terminal required. Mirrors
 * the Surfaces panel's compose → call → render pattern. Every call goes
 * through the dashboard's `tenantInterceptor` like any other `/api/*`
 * request, so the operator's own session is what gets RBAC-checked
 * server-side (`mcp:read`/`mcp:write`, plus `mcp:admin` for a `stdio`
 * connection — ADR-0012).
 */
@Component({
  selector: 'app-mcp-servers',
  imports: [FormsModule],
  templateUrl: './mcp-servers.html',
  styleUrl: './mcp-servers.scss',
})
export class McpServers implements OnInit {
  private svc = inject(McpServersService);

  readonly connections = signal<McpConnection[]>([]);
  /** Live-discovered tool count per connection name, populated on load (and
   * after create/refresh) — `undefined` while unknown, `null` if the last
   * dial attempt failed (a stale/unreachable connection). */
  readonly toolCounts = signal<Record<string, number | null>>({});
  /** Whether the operator has set `APEX_ENABLE_MCP_STDIO=1` (MCX-302) — the
   * `stdio` transport option is hidden, not silently offered-then-rejected,
   * when this is `false`. */
  readonly stdioEnabled = signal(false);

  readonly busy = signal(false);
  readonly status = signal('');
  readonly forbidden = signal(false);

  kind: TransportKind = 'http';
  name = '';
  command = '';
  args = '';
  url = '';
  secretRef = '';
  secretEnvVar = '';

  ngOnInit(): void {
    this.reload();
  }

  reload(): void {
    this.svc.list().subscribe({
      next: ({ connections, stdioEnabled }) => {
        this.forbidden.set(false);
        this.connections.set(connections);
        this.stdioEnabled.set(stdioEnabled);
        this.refreshAllToolCounts(connections);
      },
      error: (e) => this.fail(e),
    });
  }

  /** Populate each listed connection's live tool count by refreshing it — a
   * connection that fails to dial (a since-revoked credential, a stopped
   * local process) shows a "?" instead of failing the whole panel. */
  private refreshAllToolCounts(connections: McpConnection[]): void {
    if (!connections.length) return;
    forkJoin(
      connections.map((c) =>
        this.svc.refresh(c.name).pipe(
          map((r) => [c.name, r.tools.length] as const),
          catchError(() => of([c.name, null] as const)),
        ),
      ),
    ).subscribe((pairs) => {
      const next: Record<string, number | null> = { ...this.toolCounts() };
      for (const [name, count] of pairs) next[name] = count;
      this.toolCounts.set(next);
    });
  }

  create(): void {
    const name = this.name.trim();
    if (!name) return;
    let transport: McpTransport;
    if (this.kind === 'stdio') {
      const command = this.command.trim();
      if (!command) return;
      transport = { kind: 'stdio', command, args: this.parseArgs(this.args) };
    } else {
      const url = this.url.trim();
      if (!url) return;
      transport = { kind: 'http', url };
    }

    this.busy.set(true);
    this.status.set('Verifying connection…');
    this.svc
      .create({
        name,
        transport,
        secret_ref: this.secretRef.trim() || undefined,
        secret_env_var: this.secretEnvVar.trim() || undefined,
      })
      .subscribe({
        next: (created) => {
          this.busy.set(false);
          this.status.set(`Connected — ${created.tools.length} tool(s) discovered.`);
          this.toolCounts.set({ ...this.toolCounts(), [created.name]: created.tools.length });
          this.resetForm();
          this.reload();
        },
        error: (e) => {
          this.busy.set(false);
          this.fail(e);
        },
      });
  }

  refresh(c: McpConnection): void {
    this.status.set(`Refreshing ${c.name}…`);
    this.svc.refresh(c.name).subscribe({
      next: (r) => {
        this.toolCounts.set({ ...this.toolCounts(), [c.name]: r.tools.length });
        this.status.set(`${c.name}: ${r.tools.length} tool(s) discovered.`);
      },
      error: (e) => this.fail(e),
    });
  }

  remove(c: McpConnection): void {
    this.svc.delete(c.name).subscribe({
      next: () => {
        this.status.set(`Deleted ${c.name}.`);
        this.reload();
      },
      error: (e) => this.fail(e),
    });
  }

  transportSummary(t: McpTransport): string {
    return t.kind === 'stdio' ? `${t.command} ${t.args.join(' ')}`.trim() : t.url;
  }

  toolCountLabel(name: string): string {
    const count = this.toolCounts()[name];
    if (count === undefined) return '…';
    if (count === null) return 'unreachable';
    return `${count} tool${count === 1 ? '' : 's'}`;
  }

  /** Splits a space-separated args string into tokens, honoring `"..."`/`'...'`
   * quoting so a single argument may itself contain spaces (a script passed to
   * `node -e "..."`, or a path containing one — this repo's own checkout path
   * is a real example). Unquoted runs split on whitespace, same as a shell. */
  private parseArgs(raw: string): string[] {
    const args: string[] = [];
    const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(raw)) !== null) {
      args.push(m[1] ?? m[2] ?? m[3]);
    }
    return args;
  }

  private resetForm(): void {
    this.name = '';
    this.command = '';
    this.args = '';
    this.url = '';
    this.secretRef = '';
    this.secretEnvVar = '';
  }

  private fail(e: unknown): void {
    const err = e as { status?: number; error?: { error?: { message?: string } }; message?: string };
    if (err?.status === 403) this.forbidden.set(true);
    this.status.set('Error: ' + (err?.error?.error?.message ?? err?.message ?? 'request failed'));
  }
}
