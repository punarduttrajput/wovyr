import { Component, OnInit, inject, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { errText } from '../../core/http-error';
import { AuditEntry } from '../../core/api.types';
import { EmptyState } from '../../shared/empty-state';
import { AuditQuery, AuditService } from './audit.service';

const PAGE_SIZE = 50;

/**
 * UI-303: read-only viewer for the tenant's tamper-evident audit trail
 * (`GET /api/v1/audit`, SEC-301) — most-recent first, cursor-paginated, with
 * principal/action/time-range filters. RBAC-gated server-side (`audit:read`);
 * a 403 renders as an explanation, not an error.
 */
@Component({
  selector: 'app-audit',
  imports: [DatePipe, FormsModule, EmptyState],
  templateUrl: './audit.html',
  styleUrl: './audit.scss',
})
export class Audit implements OnInit {
  private svc = inject(AuditService);

  readonly entries = signal<AuditEntry[]>([]);
  readonly hasMore = signal(false);
  readonly loading = signal(false);
  /** The caller's role lacks `audit:read` (server 403) — expected for viewers. */
  readonly forbidden = signal(false);
  readonly error = signal('');
  private cursor: string | null = null;

  // Filter form fields (applied on demand, not per keystroke).
  principal = '';
  action = '';
  from = '';
  to = '';

  ngOnInit(): void {
    this.apply();
  }

  /** Run the current filters from the top (drops any pagination cursor). */
  apply(): void {
    this.cursor = null;
    this.entries.set([]);
    this.hasMore.set(false);
    this.load();
  }

  clearFilters(): void {
    this.principal = '';
    this.action = '';
    this.from = '';
    this.to = '';
    this.apply();
  }

  /** Fetch the next page (append) — or the first, right after `apply()`. */
  load(): void {
    const q: AuditQuery = {
      principal: this.principal.trim() || undefined,
      action: this.action.trim() || undefined,
      after_ms: this.toEpochMs(this.from),
      before_ms: this.toEpochMs(this.to),
      limit: PAGE_SIZE,
      cursor: this.cursor ?? undefined,
    };
    this.loading.set(true);
    this.error.set('');
    this.forbidden.set(false);
    this.svc.query(q).subscribe({
      next: (page) => {
        this.entries.update((cur) => [...cur, ...(page.data ?? [])]);
        this.hasMore.set(page.has_more);
        this.cursor = page.next_cursor;
        this.loading.set(false);
      },
      error: (e: unknown) => {
        this.loading.set(false);
        const status = (e as { status?: number })?.status;
        if (status === 403) this.forbidden.set(true);
        else this.error.set(errText(e));
      },
    });
  }

  /** `datetime-local` input value → epoch ms (undefined when blank/invalid). */
  private toEpochMs(v: string): number | undefined {
    if (!v.trim()) return undefined;
    const ms = new Date(v).getTime();
    return Number.isNaN(ms) ? undefined : ms;
  }

  outcomeClass(outcome: 'allowed' | 'denied' | 'error'): string {
    switch (outcome) {
      case 'allowed':
        return 'ok';
      case 'denied':
        return 'warn';
      case 'error':
        return 'crit';
      default:
        return 'mut';
    }
  }

  /** Short hash prefix for the chain chip. */
  short(hash: string): string {
    return hash.slice(0, 10);
  }
}
