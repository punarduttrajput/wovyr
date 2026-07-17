import {
  Component,
  ElementRef,
  HostListener,
  effect,
  input,
  output,
  viewChild,
} from '@angular/core';

/**
 * UI-301: the in-app dialog primitive (there was none — destructive flows fell
 * back to the native `confirm()`). Renders over a scrim; Escape or a scrim
 * click closes. UI-305: focus moves into the dialog on open, Tab is trapped
 * inside it, and focus returns to the opener on close.
 */
@Component({
  selector: 'app-modal',
  template: `
    @if (open()) {
      <div class="scrim" (click)="close.emit()">
        <div
          #panel
          class="modal card"
          role="dialog"
          aria-modal="true"
          [attr.aria-label]="title()"
          (click)="$event.stopPropagation()"
          (keydown)="trapTab($event)"
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

  private readonly panel = viewChild<ElementRef<HTMLElement>>('panel');
  private opener: HTMLElement | null = null;

  constructor() {
    // On open: remember the opener and move focus into the dialog; on close,
    // give focus back so keyboard users aren't dropped at the document root.
    effect(() => {
      if (this.open()) {
        this.opener = document.activeElement as HTMLElement | null;
        queueMicrotask(() => this.focusables()[0]?.focus());
      } else if (this.opener) {
        this.opener.focus();
        this.opener = null;
      }
    });
  }

  @HostListener('document:keydown.escape')
  onEscape(): void {
    if (this.open()) this.close.emit();
  }

  /** Keep Tab/Shift-Tab cycling within the dialog. */
  protected trapTab(e: KeyboardEvent): void {
    if (e.key !== 'Tab') return;
    const items = this.focusables();
    if (!items.length) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  private focusables(): HTMLElement[] {
    const root = this.panel()?.nativeElement;
    if (!root) return [];
    return Array.from(
      root.querySelectorAll<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => !el.hasAttribute('disabled'));
  }
}
