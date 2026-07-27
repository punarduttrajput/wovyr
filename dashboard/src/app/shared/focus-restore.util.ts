import { Signal, effect } from '@angular/core';

/**
 * DASH-408: the one focus-restore mechanism shared by every overlay-like
 * primitive (`Modal`, `CommandPalette`) — trapping itself is delegated to
 * `cdkTrapFocus`/`cdkTrapFocusAutoCapture` (`@angular/cdk/a11y`), which moves
 * focus in but has no opinion on restoring it. Call once from a component's
 * constructor (an injection context) with the signal that flips true on open.
 */
export function restoreFocusOnClose(open: Signal<boolean>): void {
  let opener: HTMLElement | null = null;
  effect(() => {
    if (open()) {
      opener = document.activeElement as HTMLElement | null;
    } else if (opener) {
      opener.focus();
      opener = null;
    }
  });
}
