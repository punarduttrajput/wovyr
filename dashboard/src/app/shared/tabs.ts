import { Component, input, output } from '@angular/core';

export interface TabSpec {
  id: string;
  label: string;
  /** Optional count badge (e.g. installed plugins). Omit to hide. */
  count?: number;
}

/**
 * UI-301: the tab strip previously re-implemented per feature (Marketplace,
 * Settings). Renders an accessible `role="tablist"` with roving tabindex —
 * ←/→/Home/End move and select (UI-305).
 */
@Component({
  selector: 'app-tabs',
  template: `
    <div class="tabs" role="tablist">
      @for (t of tabs(); track t.id) {
        <button
          type="button"
          role="tab"
          class="tab"
          [class.on]="t.id === active()"
          [attr.aria-selected]="t.id === active()"
          [tabindex]="t.id === active() ? 0 : -1"
          (click)="select(t.id)"
          (keydown)="onKey($event, t.id)"
        >
          {{ t.label }}
          @if (t.count !== undefined && t.count > 0) {
            <span class="count tnum">{{ t.count }}</span>
          }
        </button>
      }
    </div>
  `,
  styles: `
    .tabs { display:flex; gap:4px; margin-bottom:18px; border-bottom:1px solid var(--line); }
    .tab {
      appearance:none; background:none; border:none; cursor:pointer;
      padding:8px 14px; font-size:13px; font-weight:500; color:var(--ink-3);
      border-bottom:2px solid transparent; margin-bottom:-1px; font-family:inherit;
    }
    .tab:hover { color:var(--ink-2); }
    .tab.on { color:var(--accent); border-bottom-color:var(--accent); }
    .count {
      font-family:var(--mono); font-size:10.5px; padding:1px 6px; border-radius:10px;
      background:var(--surface-2); border:1px solid var(--line); color:var(--ink-3); margin-left:2px;
    }
  `,
})
export class Tabs {
  readonly tabs = input.required<TabSpec[]>();
  readonly active = input.required<string>();
  readonly activeChange = output<string>();

  protected select(id: string): void {
    if (id !== this.active()) this.activeChange.emit(id);
  }

  /** Roving-tabindex keyboard nav: arrows wrap, Home/End jump. Selection follows
   * focus (the WAI-ARIA "automatic activation" tabs pattern). */
  protected onKey(e: KeyboardEvent, id: string): void {
    const ids = this.tabs().map((t) => t.id);
    const i = ids.indexOf(id);
    let next: number | null = null;
    if (e.key === 'ArrowRight') next = (i + 1) % ids.length;
    else if (e.key === 'ArrowLeft') next = (i - 1 + ids.length) % ids.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = ids.length - 1;
    if (next === null) return;
    e.preventDefault();
    this.select(ids[next]);
    const buttons = (e.currentTarget as HTMLElement).parentElement?.querySelectorAll('button');
    (buttons?.[next] as HTMLElement | undefined)?.focus();
  }
}
