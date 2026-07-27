import { Component, HostListener, input, output } from '@angular/core';
import { A11yModule } from '@angular/cdk/a11y';
import { restoreFocusOnClose } from './focus-restore.util';

/**
 * UI-301: the in-app dialog primitive (there was none — destructive flows fell
 * back to the native `confirm()`). Renders over a scrim; Escape or a scrim
 * click closes. UI-305/DASH-408: focus trapping is `cdkTrapFocus` +
 * `cdkTrapFocusAutoCapture` (`@angular/cdk/a11y`) — the same mechanism the
 * command palette uses (A11Y-206) — rather than a hand-rolled Tab handler;
 * focus restore on close is the shared `restoreFocusOnClose` helper.
 */
@Component({
  selector: 'app-modal',
  imports: [A11yModule],
  template: `
    @if (open()) {
      <div class="scrim" (click)="close.emit()">
        <div
          class="modal card"
          role="dialog"
          aria-modal="true"
          [attr.aria-label]="title()"
          cdkTrapFocus
          [cdkTrapFocusAutoCapture]="open()"
          (click)="$event.stopPropagation()"
        >
          <div class="card-h">
            <h3>{{ title() }}</h3>
            <div class="right">
              <button class="icon-btn" type="button" aria-label="Close dialog" (click)="close.emit()">
                <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path d="M6 6l12 12M18 6L6 18" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
                </svg>
              </button>
            </div>
          </div>
          <div class="modal-body">
            <ng-content />
          </div>
        </div>
      </div>
    }
  `,
  styles: `
    .scrim {
      position:fixed; inset:0; z-index:60; background:rgba(13,20,36,.45);
      display:grid; place-items:center; padding:20px;
    }
    .modal { width:min(480px, 100%); box-shadow:var(--shadow-lg); }
    .modal-body { padding:16px; }
  `,
})
export class Modal {
  readonly open = input.required<boolean>();
  readonly title = input('');
  readonly close = output<void>();

  constructor() {
    restoreFocusOnClose(this.open);
  }

  @HostListener('document:keydown.escape')
  onEscape(): void {
    if (this.open()) this.close.emit();
  }
}
