import { Component, computed, input } from '@angular/core';

/**
 * UI-301: the workflow/activity status → pill-class mapping, extracted from the
 * three components that used to carry it verbatim (Monitoring, Execution detail,
 * Workflow Builder). One place to learn about a new engine status.
 */
export function statusClass(s: string): string {
  switch (s) {
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

/** A status pill colored by [`statusClass`]. Pill visuals come from the global
 * `.pill` design-system styles. */
@Component({
  selector: 'app-status-pill',
  template: `<span class="pill {{ cls() }}"><span class="pd"></span>{{ status() }}</span>`,
})
export class StatusPill {
  readonly status = input.required<string>();
  protected readonly cls = computed(() => statusClass(this.status()));
}
