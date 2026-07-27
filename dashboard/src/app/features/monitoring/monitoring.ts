import { Component, computed, inject, signal } from '@angular/core';
import { DecimalPipe } from '@angular/common';
import { RouterLink } from '@angular/router';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { forkJoin, of, timer } from 'rxjs';
import { catchError, switchMap } from 'rxjs/operators';
import { MonitoringService, MetricsSnapshot } from './monitoring.service';
import { StatusPill } from '../../shared/status-pill';
import { TimeSeriesChart, ChartData } from '../../shared/time-series-chart';
import { Health, MetricSample, WorkflowSummary } from '../../core/api.types';

const POLL_MS = 5000;
const HISTORY = 30;

interface Bucket {
  le: number;
  count: number;
}

/**
 * Cumulative-histogram `_bucket` samples for `name`, summed across every
 * label combination (the server emits one `_bucket` series per route/method
 * pair) so each `le` boundary reflects the *whole* histogram rather than
 * just whichever route's sample happened to sort last — every route shares
 * one platform-wide latency chart here, not a per-route breakdown.
 */
function parseBuckets(samples: MetricSample[], name: string): Bucket[] {
  const byLe = new Map<number, number>();
  for (const s of samples) {
    if (s.name !== name) continue;
    const le = s.labels['le'] === '+Inf' ? Infinity : parseFloat(s.labels['le']);
    if (Number.isNaN(le)) continue;
    byLe.set(le, (byLe.get(le) ?? 0) + s.value);
  }
  return [...byLe.entries()].map(([le, count]) => ({ le, count })).sort((a, b) => a.le - b.le);
}

/**
 * DASH-409: an approximate windowed percentile (in ms) from the delta
 * between two cumulative-histogram snapshots — subtracting the previous
 * poll's cumulative bucket counts from the current one yields the counts
 * for just this window, then walks buckets ascending until the running
 * count clears `p` of the window's total. Bucket-boundary granularity, not
 * linear interpolation within a bucket — good enough for a dashboard trend
 * line, not a billing-grade figure.
 */
function windowedPercentileMs(cur: Bucket[], prev: Bucket[] | null, p: number): number | null {
  if (!cur.length) return null;
  const prevByLe = new Map((prev ?? []).map((b) => [b.le, b.count]));
  const deltas = cur.map((b) => ({ le: b.le, count: Math.max(0, b.count - (prevByLe.get(b.le) ?? 0)) }));
  const total = deltas[deltas.length - 1]?.count ?? 0;
  if (!total) return null;
  const target = p * total;
  const hit = deltas.find((b) => b.count >= target);
  if (!hit || hit.le === Infinity) return null;
  return hit.le * 1000;
}

let sparkGradientSeq = 0;

@Component({
  selector: 'app-monitoring',
  imports: [DecimalPipe, RouterLink, StatusPill, TimeSeriesChart],
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

  /** DASH-409: latency (p50/p95) and LLM-spend time series over the polling
   * window — the stat tiles above only ever showed a single scalar. */
  private prevBuckets: Bucket[] | null = null;
  private prevCost: number | null = null;
  readonly xTimes = signal<number[]>([]);
  readonly p50History = signal<(number | null)[]>([]);
  readonly p95History = signal<(number | null)[]>([]);
  readonly spendWindow = signal<(number | null)[]>([]);

  readonly errorPct = computed(() => ((this.metrics()?.errorRate ?? 0) * 100).toFixed(2));
  readonly sparkline = computed(() => this.buildSpark(this.perWindow()));
  /** A11Y-208/DASH-409: unique per instance — the old hardcoded `id="sg"` on
   * the gradient `<defs>` would collide the moment a second sparkline (or a
   * second Monitoring instance) ever rendered on the same page. */
  readonly sparkGradientId = `sg-${sparkGradientSeq++}`;

  readonly latencyChart = computed<ChartData>(() => ({
    x: this.xTimes(),
    series: [
      { label: 'p50', values: this.p50History(), colorToken: '--accent' },
      { label: 'p95', values: this.p95History(), colorToken: '--accent-2' },
    ],
  }));

  readonly spendChart = computed<ChartData>(() => ({
    x: this.xTimes(),
    series: [{ label: 'LLM spend', values: this.spendWindow(), colorToken: '--accent' }],
  }));

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

          const buckets = parseBuckets(metrics.samples, 'wovyr_api_request_duration_seconds_bucket');
          const p50 = windowedPercentileMs(buckets, this.prevBuckets, 0.5);
          const p95 = windowedPercentileMs(buckets, this.prevBuckets, 0.95);
          this.prevBuckets = buckets.length ? buckets : this.prevBuckets;
          this.p50History.update((h) => [...h, p50].slice(-HISTORY));
          this.p95History.update((h) => [...h, p95].slice(-HISTORY));

          const costDelta =
            this.prevCost === null ? null : Math.max(0, metrics.llmCostUsd - this.prevCost);
          this.prevCost = metrics.llmCostUsd;
          this.spendWindow.update((h) => [...h, costDelta].slice(-HISTORY));

          this.xTimes.update((h) => [...h, Math.floor(Date.now() / 1000)].slice(-HISTORY));
        }
        if (health) this.health.set(health);
        if (wf) this.workflows.set(wf.data ?? []);
        this.ticks.update((t) => t + 1);
      });
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
