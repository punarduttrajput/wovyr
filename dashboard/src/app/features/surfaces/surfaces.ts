import {
  ChangeDetectorRef,
  Component,
  CUSTOM_ELEMENTS_SCHEMA,
  ElementRef,
  ViewChild,
  effect,
  inject,
  signal,
} from '@angular/core';
import { JsonPipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { firstValueFrom } from 'rxjs';
import type { WovyrUiFrameDecideDetail } from '@wovyr/ui-react/web-component';
// Side-effecting: registers the `<wovyr-ui-frame>` custom element (RDR-402).
import '@wovyr/ui-react/web-component';
import { errText } from '../../core/http-error';
import { ThemeService } from '../../core/theme.service';
import { PendingUiFrame, SurfacesService, UiDecisionOutcome } from './surfaces.service';

/** The runtime shape of an `<wovyr-ui-frame>` DOM node (its `frame`/
 * `expectedHash`/`theme` are JS properties, not HTML attributes — see
 * `sdks/ui-react/src/webComponent.tsx`'s doc comment — so they're set
 * imperatively here rather than via template binding). */
interface WovyrUiFrameElement extends HTMLElement {
  frame: unknown;
  expectedHash: string | undefined;
  disabled: boolean;
  theme: 'light' | 'dark' | undefined;
}

/**
 * Dashboard Surfaces panel (ITS-601/602): real dogfooding of the generative-UI
 * trust runtime on Wovyr's own ops surface. Composes a real `UiFrame`,
 * presents it via `POST /api/v1/ui/present` (standalone mode, no workflow
 * involved), renders it with `<wovyr-ui-frame>`, and records the operator's
 * own decision back through the same RBAC-scoped routes (`ui:read`/
 * `ui:write`) a design partner's integration would use.
 */
@Component({
  selector: 'app-surfaces',
  imports: [FormsModule, JsonPipe],
  templateUrl: './surfaces.html',
  styleUrl: './surfaces.scss',
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
})
export class Surfaces {
  private svc = inject(SurfacesService);
  private cdr = inject(ChangeDetectorRef);
  private themeSvc = inject(ThemeService);

  private frameElRef?: ElementRef<WovyrUiFrameElement>;
  @ViewChild('frameEl') set frameElSetter(ref: ElementRef<WovyrUiFrameElement> | undefined) {
    this.frameElRef = ref;
    this.applyFrameToElement();
  }

  constructor() {
    // DSY-105: keep the embedded `<wovyr-ui-frame>` in the dashboard's own
    // theme, including a live toggle while a frame is on screen — the
    // renderer's tokens must never silently disagree with the surrounding
    // chrome (previously nothing forwarded `data-theme` to it at all, so an
    // OS-dark/dashboard-light combination rendered a dark frame in a light
    // console). `frameElRef` is a plain property, not itself tracked, so this
    // effect re-applies whenever the theme signal changes and simply no-ops
    // if the element isn't mounted yet; `applyFrameToElement` below covers
    // the element-just-mounted case.
    effect(() => {
      const theme = this.themeSvc.theme();
      const el = this.frameElRef?.nativeElement;
      if (el) el.theme = theme;
    });
  }

  title = 'Confirm refund';
  message = 'Refund $42.00 to order #A1017?';
  includeDestructive = false;

  readonly busy = signal(false);
  readonly status = signal('');
  readonly pending = signal<PendingUiFrame | null>(null);
  readonly blockedReason = signal<string | null>(null);
  readonly outcome = signal<UiDecisionOutcome | null>(null);

  /** Build a real `UiFrame` from the form and present it — no canned/mocked
   * fixture, this is the exact JSON `POST /api/v1/ui/present` receives. */
  present(): void {
    const title = this.title.trim() || 'Confirm action';
    const text = this.message.trim() || 'Proceed?';
    const children: unknown[] = [
      { type: 'text', text },
      { type: 'button', action: 'approve', label: 'Approve', class: 'approve' },
      { type: 'button', action: 'cancel', label: 'Cancel', class: 'cancel' },
    ];
    if (this.includeDestructive) {
      // Deliberately included so an operator can see the trust layer's
      // deny-by-default destructive-action rule fire in their own dashboard,
      // not just read about it — the panel's own dogfooding of GRD-201.
      children.push({
        type: 'button',
        action: 'delete_all',
        label: 'Delete everything',
        class: 'destructive',
      });
    }

    this.busy.set(true);
    this.status.set('Presenting…');
    this.blockedReason.set(null);
    this.outcome.set(null);
    this.svc
      .present({ schema_version: '1.0.0', title, root: { type: 'column', children } })
      .subscribe({
        next: (pf) => this.showFrame(pf),
        error: (e) => this.failPresent(e),
      });
  }

  /** Handles the `decide` CustomEvent `<wovyr-ui-frame>` dispatches (RDR-402).
   * Typed as a plain `Event` because Angular's template type-checker doesn't
   * know `<wovyr-ui-frame>`'s event map under `CUSTOM_ELEMENTS_SCHEMA` — cast
   * once here rather than `$any()` in the template. Attaches the actual API
   * call's promise to `event.detail.result` so the element re-enables for
   * retry if it rejects, matching `UiFrameView`'s `onDecide` contract exactly. */
  onDecide(event: Event): void {
    const frameId = this.pending()?.frame_id;
    if (!frameId) return;
    const { detail } = event as CustomEvent<WovyrUiFrameDecideDetail>;
    const { decision } = detail;
    detail.result = firstValueFrom(
      this.svc.decide(frameId, decision.action, decision.values ?? {}),
    )
      .then(() => {
        this.status.set(`Decided: ${decision.action}`);
        return firstValueFrom(this.svc.getDecision(frameId));
      })
      .then((decided) => this.outcome.set(decided))
      .catch((e) => {
        this.status.set('Error: ' + errText(e));
        throw e;
      });
  }

  reset(): void {
    this.pending.set(null);
    this.blockedReason.set(null);
    this.outcome.set(null);
    this.status.set('');
  }

  private showFrame(pf: PendingUiFrame): void {
    this.pending.set(pf);
    this.busy.set(false);
    this.status.set('Frame presented — awaiting your decision.');
    // Force the `@if (pending())` block to render before touching the
    // element it creates (or reuses, across a second present()).
    this.cdr.detectChanges();
    this.applyFrameToElement();
  }

  private applyFrameToElement(): void {
    const el = this.frameElRef?.nativeElement;
    const pf = this.pending();
    if (!el || !pf) return;
    el.frame = pf.frame;
    el.expectedHash = pf.frame_hash;
    el.theme = this.themeSvc.theme();
  }

  private failPresent(e: unknown): void {
    this.busy.set(false);
    this.pending.set(null);
    const err = e as { status?: number };
    if (err?.status === 403) {
      this.blockedReason.set(errText(e));
      this.status.set('Blocked by the trust layer.');
    } else {
      this.status.set('Error: ' + errText(e));
    }
  }

}
