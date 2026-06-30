import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import { Health, MetricSample, Page, WorkflowSummary } from '../../core/api.types';

/** Aggregated view of the platform metrics the Monitoring surface renders. */
export interface MetricsSnapshot {
  samples: MetricSample[];
  requestsTotal: number;
  errorsTotal: number;
  errorRate: number; // 0..1
  avgLatencyMs: number;
  llmCostUsd: number;
  llmTokens: number;
  cacheSavingsUsd: number;
  /** Per-route request counts, sorted desc. */
  routes: { route: string; count: number; errors: number }[];
}

@Injectable({ providedIn: 'root' })
export class MonitoringService {
  private http = inject(HttpClient);

  health(): Observable<Health> {
    return this.http.get<Health>('/healthz');
  }

  workflows(): Observable<Page<WorkflowSummary>> {
    return this.http.get<Page<WorkflowSummary>>('/api/v1/workflows');
  }

  metrics(): Observable<MetricsSnapshot> {
    return this.http
      .get('/metrics', { responseType: 'text' })
      .pipe(map((text) => this.aggregate(this.parse(text))));
  }

  /** Parse Prometheus/OpenMetrics text into samples (ignores HELP/TYPE/comment lines). */
  private parse(text: string): MetricSample[] {
    const out: MetricSample[] = [];
    for (const raw of text.split('\n')) {
      const line = raw.trim();
      if (!line || line.startsWith('#')) continue;
      const brace = line.indexOf('{');
      let name: string;
      let labels: Record<string, string> = {};
      let rest: string;
      if (brace >= 0) {
        name = line.slice(0, brace);
        const close = line.lastIndexOf('}');
        labels = this.parseLabels(line.slice(brace + 1, close));
        rest = line.slice(close + 1).trim();
      } else {
        const sp = line.indexOf(' ');
        name = line.slice(0, sp);
        rest = line.slice(sp + 1).trim();
      }
      const value = parseFloat(rest.split(/\s+/)[0]);
      if (!Number.isNaN(value)) out.push({ name, labels, value });
    }
    return out;
  }

  private parseLabels(s: string): Record<string, string> {
    const labels: Record<string, string> = {};
    // key="value" pairs; values may contain commas/escapes
    const re = /(\w+)="((?:[^"\\]|\\.)*)"/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(s))) labels[m[1]] = m[2].replace(/\\"/g, '"');
    return labels;
  }

  private aggregate(samples: MetricSample[]): MetricsSnapshot {
    const sum = (name: string, pred?: (s: MetricSample) => boolean) =>
      samples
        .filter((s) => s.name === name && (!pred || pred(s)))
        .reduce((a, s) => a + s.value, 0);

    const isErr = (s: MetricSample) => {
      const code = +(s.labels['status'] ?? s.labels['code'] ?? '0');
      return code >= 400;
    };

    const requestsTotal = sum('apex_api_requests_total');
    const errorsTotal = sum('apex_api_requests_total', isErr);
    const durSum = sum('apex_api_request_duration_seconds_sum');
    const durCount = sum('apex_api_request_duration_seconds_count');

    // Per-route rollup.
    const byRoute = new Map<string, { count: number; errors: number }>();
    for (const s of samples) {
      if (s.name !== 'apex_api_requests_total') continue;
      const route = s.labels['route'] ?? s.labels['path'] ?? '—';
      const cur = byRoute.get(route) ?? { count: 0, errors: 0 };
      cur.count += s.value;
      if (isErr(s)) cur.errors += s.value;
      byRoute.set(route, cur);
    }

    return {
      samples,
      requestsTotal,
      errorsTotal,
      errorRate: requestsTotal ? errorsTotal / requestsTotal : 0,
      avgLatencyMs: durCount ? (durSum / durCount) * 1000 : 0,
      llmCostUsd: sum('apex_llm_cost_usd_total'),
      llmTokens: sum('apex_llm_tokens_total'),
      cacheSavingsUsd: sum('apex_cache_savings_usd_total'),
      routes: [...byRoute.entries()]
        .map(([route, v]) => ({ route, ...v }))
        .sort((a, b) => b.count - a.count),
    };
  }
}
