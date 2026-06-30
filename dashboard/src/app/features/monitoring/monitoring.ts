import { Component, computed, inject, signal } from '@angular/core';
import { DecimalPipe } from '@angular/common';
import { RouterLink } from '@angular/router';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { forkJoin, of, timer } from 'rxjs';
import { catchError, switchMap } from 'rxjs/operators';
import { MonitoringService, MetricsSnapshot } from './monitoring.service';
import { Health, WorkflowSummary } from '../../core/api.types';

const POLL_MS = 5000;
const HISTORY = 30;

@Component({
  selector: 'app-monitoring',
  imports: [DecimalPipe, RouterLink],
  templateUrl: './monitoring.html',
  styleUrl: './monitoring.scss',
})
export class Monitoring {
  private svc = inject(MonitoringService);

  readonly metrics = signal<MetricsSnapshot | null>(null);
  readonly health = signal<Health | null>(null);
  readonly workflows = signal<WorkflowSummary[]>([]);
  readonly reachable = signal(true);
  readonly ticks = signal(0);

  /** Requests observed in each poll window (deltas of the cumulative counter). */
  private prevRequests: number | null = null;
  readonly perWindow = signal<number[]>([]);

  readonly errorPct = computed(() => ((this.metrics()?.errorRate ?? 0) * 100).toFixed(2));
  readonly sparkline = computed(() => this.buildSpark(this.perWindow()));

  constructor() {
    timer(0, POLL_MS)
      .pipe(
        switchMap(() =>
          forkJoin({
            metrics: this.svc.metrics().pipe(catchError(() => of(null))),
            health: this.svc.health().pipe(catchError(() => of(null))),
            wf: this.svc.workflows().pipe(catchError(() => of(null))),
          }),
        ),
        takeUntilDestroyed(),
      )
      .subscribe(({ metrics, health, wf }) => {
        this.reachable.set(!!(metrics || health));
        if (metrics) {
          this.metrics.set(metrics);
          if (this.prevRequests !== null) {
            const delta = Math.max(0, metrics.requestsTotal - this.prevRequests);
            this.perWindow.update((h) => [...h, delta].slice(-HISTORY));
          }
          this.prevRequests = metrics.requestsTotal;
        }
        if (health) this.health.set(health);
        if (wf) this.workflows.set(wf.data ?? []);
        this.ticks.update((t) => t + 1);
      });
  }

  statusClass(status: string): string {
    switch (status) {
      case 'Completed':
        return 'ok';
      case 'Failed':
        return 'crit';
      case 'Compensating':
        return 'warn';
      case 'Running':
      case 'Waiting':
      case 'Resumed':
      case 'Scheduled':
        return 'info';
      default:
        return 'mut';
    }
  }

  activityCount(s: WorkflowSummary): number {
    return Object.keys(s.activities ?? {}).length;
  }

  /** Build SVG line + area path strings for the per-window sparkline. */
  private buildSpark(values: number[]): { line: string; area: string; ok: boolean } {
    if (values.length < 2) return { line: '', area: '', ok: false };
    const w = 180;
    const h = 34;
    const max = Math.max(1, ...values);
    const step = w / (values.length - 1);
    const pts = values.map((v, i) => [i * step, h - (v / max) * (h - 4) + 2]);
    const line = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(' ');
    const area = `${line} L ${w} ${h + 2} L 0 ${h + 2} Z`;
    return { line, area, ok: true };
  }
}
