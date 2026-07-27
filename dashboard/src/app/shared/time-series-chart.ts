import {
  Component,
  ElementRef,
  OnDestroy,
  computed,
  effect,
  inject,
  input,
  viewChild,
} from '@angular/core';
import uPlot from 'uplot';
import { ThemeService } from '../core/theme.service';

export interface ChartSeries {
  label: string;
  values: (number | null)[];
  /** A DSY-101 CSS custom-property name (e.g. `--accent`), resolved at render
   * time so a chart repaints in the right colour on a live theme toggle. */
  colorToken: string;
}

export interface ChartData {
  /** Unix seconds, one per sample — same length as every series' `values`. */
  x: number[];
  series: ChartSeries[];
}

let chartSeq = 0;

/**
 * DASH-409: a uPlot wrapper for the two time series Monitoring needed
 * (latency p50/p95, LLM spend) that the single hand-rolled sparkline
 * (`monitoring.html`'s `.spark`, kept as-is — it's good and cheap for a
 * single-scalar trend) can't express. Canvas-based, ~47 kB, and restrained
 * enough to inherit the design system rather than fight it.
 *
 * Colours are read from the DSY-101 token file's resolved custom properties
 * at render time, never hardcoded — `colorToken` names the property,
 * `resolveColor` reads it off `documentElement` so a live theme toggle
 * repaints correctly. Paired with an accessible `<details>` table of the
 * exact same data — a canvas chart has nothing for a screen reader to read
 * on its own, and this stays reachable by keyboard without hiding it in an
 * `aria-hidden` region no one can get to.
 */
@Component({
  selector: 'app-time-series-chart',
  template: `
    <div #host class="ts-host" role="img" [attr.aria-label]="ariaLabel()"></div>
    <details class="ts-table">
      <summary>View data as table</summary>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th scope="col">Time</th>
              @for (s of data().series; track s.label) {
                <th scope="col">{{ s.label }}{{ unit() ? ' (' + unit() + ')' : '' }}</th>
              }
            </tr>
          </thead>
          <tbody>
            @for (row of tableRows(); track $index) {
              <tr>
                <td class="mono">{{ row.t }}</td>
                @for (v of row.values; track $index) {
                  <td class="num tnum">{{ v }}</td>
                }
              </tr>
            }
          </tbody>
        </table>
      </div>
    </details>
  `,
  styles: `
    .ts-host { width:100%; height:var(--ts-h, 160px); }
    .ts-table { margin-top:6px; }
    .ts-table summary { font-size:11.5px; color:var(--ink-3); cursor:pointer; }
    .ts-table .table-wrap { max-height:200px; overflow:auto; margin-top:6px; }
  `,
})
export class TimeSeriesChart implements OnDestroy {
  readonly data = input.required<ChartData>();
  readonly unit = input('');
  readonly height = input(160);
  readonly ariaLabel = input('Time series chart');

  private readonly theme = inject(ThemeService);
  private readonly hostEl = viewChild<ElementRef<HTMLDivElement>>('host');
  private plot?: uPlot;
  private resizeObserver?: ResizeObserver;
  private readonly instanceId = chartSeq++;
  /** Repaint (not just re-`setData`) when the theme changes, since a token
   * like `--accent` resolves to a different colour per theme and uPlot's
   * per-series `stroke` is fixed at construction time. */
  private lastTheme?: string;

  readonly tableRows = computed(() => {
    const d = this.data();
    return d.x.map((t, i) => ({
      t: new Date(t * 1000).toLocaleTimeString(),
      values: d.series.map((s) => (s.values[i] == null ? '—' : s.values[i]!.toFixed(2))),
    }));
  });

  constructor() {
    effect(() => {
      const d = this.data();
      const currentTheme = this.theme.theme();
      const host = this.hostEl()?.nativeElement;
      if (!host) return;
      if (this.plot && currentTheme !== this.lastTheme) {
        this.plot.destroy();
        this.plot = undefined;
      }
      this.lastTheme = currentTheme;
      this.render(host, d);
    });
  }

  ngOnDestroy(): void {
    this.resizeObserver?.disconnect();
    this.plot?.destroy();
  }

  private resolveColor(token: string): string {
    const v = getComputedStyle(document.documentElement).getPropertyValue(token).trim();
    return v || '#888';
  }

  private render(host: HTMLDivElement, d: ChartData): void {
    const plotData = [d.x, ...d.series.map((s) => s.values)] as uPlot.AlignedData;
    if (this.plot) {
      this.plot.setData(plotData);
      return;
    }

    const gridColor = this.resolveColor('--line');
    const tickColor = this.resolveColor('--ink-3');

    const opts: uPlot.Options = {
      width: host.clientWidth || 400,
      height: this.height(),
      id: `ts-chart-${this.instanceId}`,
      cursor: { show: true },
      legend: { show: true },
      series: [
        {},
        ...d.series.map((s) => ({
          label: s.label,
          stroke: this.resolveColor(s.colorToken),
          width: 1.75,
          points: { show: false },
        })),
      ],
      scales: { x: { time: true } },
      axes: [
        { stroke: tickColor, grid: { stroke: gridColor, width: 1 }, ticks: { stroke: gridColor } },
        { stroke: tickColor, grid: { stroke: gridColor, width: 1 }, ticks: { stroke: gridColor } },
      ],
    };

    this.plot = new uPlot(opts, plotData, host);
    this.resizeObserver = new ResizeObserver(() => {
      if (this.plot && host.clientWidth) this.plot.setSize({ width: host.clientWidth, height: this.height() });
    });
    this.resizeObserver.observe(host);
  }
}
