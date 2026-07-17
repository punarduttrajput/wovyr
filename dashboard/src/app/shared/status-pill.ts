import { Component, computed, input } from '@angular/core';

/**
 * UI-301: the workflow/activity status → pill-class mapping, extracted from the
 * three components that used to carry it verbatim (Monitoring, Execution detail,
 * Workflow Builder). One place to learn about a new engine status.
 *
 * Case-insensitive (DX-301 follow-up): the engine serializes snake_case
 * (`completed`, RM-GA-P4 API-702) but this mapping predated that and matched
 * the old PascalCase only — every real status was silently falling through to
 * the neutral pill.
 */
export function statusClass(s: string): string {
  switch (s.toLowerCase()) {
    case 'completed':
      return 'ok';
    case 'failed':
      return 'crit';
    case 'compensating':
      return 'warn';
    case 'running':
    case 'waiting':
    case 'resumed':
    case 'scheduled':
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
