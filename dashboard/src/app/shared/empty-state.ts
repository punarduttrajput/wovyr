import { Component, input } from '@angular/core';

/**
 * UI-301: the empty/loading/error card primitive — the dashed "nothing here"
 * card each feature used to hand-roll. Content is projected, so features keep
 * their own copy ("No executions yet. Start one with …").
 */
@Component({
  selector: 'app-empty-state',
  template: `
    <div class="empty-state card" [class.err]="kind() === 'error'" role="status">
      @if (kind() === 'loading') {
        <span class="spin" aria-hidden="true"></span>
      }
      <div class="body"><ng-content /></div>
    </div>
  `,
  styles: `
    .empty-state {
      display:flex; align-items:center; justify-content:center; gap:10px;
      padding:30px; border-style:dashed; color:var(--ink-3); font-size:13px;
    }
    .empty-state.err { color:var(--crit); border-color:color-mix(in srgb, var(--crit) 35%, var(--line)); }
    .body :where(b) { color:var(--ink); font-size:14px; }
    .spin {
      width:14px; height:14px; flex:none; border-radius:50%;
      border:2px solid var(--line); border-top-color:var(--accent);
      animation:spin .8s linear infinite;
    }
    @keyframes spin { to { transform:rotate(360deg); } }
  `,
})
export class EmptyState {
  readonly kind = input<'empty' | 'loading' | 'error'>('empty');
}
