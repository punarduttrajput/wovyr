import { Component, input } from '@angular/core';

/**
 * Stand-in for dashboard surfaces whose build slice has not landed yet. The `surface`
 * and `eyebrow` inputs are bound from the route's `data` (withComponentInputBinding).
 */
@Component({
  selector: 'app-placeholder',
  template: `
    <section class="view">
      <div class="page-head">
        <div>
          <div class="eyebrow">{{ eyebrow() }}</div>
          <h1>{{ surface() }}</h1>
          <p>This surface is specified in <code>docs/10-dashboard</code> and is queued for an
            upcoming build slice. Agent Studio is the first surface wired to the live platform API.</p>
        </div>
      </div>
      <div class="card empty">
        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <rect x="3" y="4" width="18" height="16" rx="2.5" stroke="currentColor" stroke-width="1.6"/>
          <path d="M3 9h18M8 14h8M8 17h5" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
        </svg>
        <div>
          <b>{{ surface() }} — coming soon</b>
          <span>The design is approved; implementation follows the Agent Studio slice.</span>
        </div>
      </div>
    </section>
  `,
  styles: [
    `
    .view { padding:24px 26px 60px; }
    .empty {
      display:flex; align-items:center; gap:16px; padding:40px; color:var(--ink-3);
      border-style:dashed;
    }
    .empty svg { width:38px; height:38px; flex:none; color:var(--ink-3); opacity:.6; }
    .empty b { display:block; color:var(--ink); font-size:15px; font-weight:600; }
    .empty span { font-size:13px; }
    code { font-family:var(--mono); font-size:12px; background:var(--neutral-bg); padding:1px 5px; border-radius:4px; }
  `,
  ],
})
export class Placeholder {
  readonly surface = input('Surface');
  readonly eyebrow = input('Wovyr');
}
