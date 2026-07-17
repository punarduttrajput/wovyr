import { Component, Injectable, inject, signal } from '@angular/core';
import { Modal } from './modal';

export interface ConfirmRequest {
  message: string;
  title?: string;
  /** Label for the affirmative button (default "Confirm"). */
  confirmLabel?: string;
  /** Style the affirmative button as destructive. */
  danger?: boolean;
}

interface PendingConfirm extends ConfirmRequest {
  resolve: (confirmed: boolean) => void;
}

/**
 * UI-301: in-app confirmation, replacing the native `confirm()` (which is
 * unstylable, blocks the event loop, and some embedders suppress outright).
 * `await confirmService.ask({...})` resolves `true` only on explicit confirm.
 */
@Injectable({ providedIn: 'root' })
export class ConfirmService {
  readonly pending = signal<PendingConfirm | null>(null);

  ask(req: ConfirmRequest): Promise<boolean> {
    // One dialog at a time; a second request auto-cancels the first rather
    // than silently replacing its resolver.
    this.pending()?.resolve(false);
    return new Promise<boolean>((resolve) => this.pending.set({ ...req, resolve }));
  }

  /** The dialog's answer path — resolves and clears the pending request. */
  settle(confirmed: boolean): void {
    const p = this.pending();
    if (!p) return;
    this.pending.set(null);
    p.resolve(confirmed);
  }
}

/** Renders the pending [`ConfirmService`] request. Hosted once, in the app shell. */
@Component({
  selector: 'app-confirm-dialog',
  imports: [Modal],
  template: `
    <app-modal
      [open]="svc.pending() !== null"
      [title]="svc.pending()?.title || 'Are you sure?'"
      (close)="svc.settle(false)"
    >
      <p class="msg">{{ svc.pending()?.message }}</p>
      <div class="actions">
        <button class="btn" type="button" (click)="svc.settle(false)">Cancel</button>
        <button
          class="btn"
          type="button"
          [class.pri]="!svc.pending()?.danger"
          [class.danger]="svc.pending()?.danger"
          (click)="svc.settle(true)"
        >
          {{ svc.pending()?.confirmLabel || 'Confirm' }}
        </button>
      </div>
    </app-modal>
  `,
  styles: `
    .msg { margin:0 0 16px; font-size:13.5px; color:var(--ink-2); }
    .actions { display:flex; justify-content:flex-end; gap:9px; }
    .btn.danger {
      background:var(--crit); border-color:var(--crit); color:#fff;
    }
    .btn.danger:hover { filter:brightness(.92); }
  `,
})
export class ConfirmDialog {
  protected readonly svc = inject(ConfirmService);
}
