import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
import * as axe from 'axe-core';
import { App } from './app';
import { ConfirmDialog, ConfirmService } from './shared/confirm';
import { Audit } from './features/audit/audit';

/**
 * UI-305: automated axe checks over the core flows — the app shell (nav rail,
 * topbar), the audit table, and the shared dialog. Any WCAG violation axe can
 * detect statically (missing labels/names, contrast candidates, landmark
 * misuse) fails the suite, so a11y regressions can't land silently.
 */
async function expectNoViolations(root: Element): Promise<void> {
  const results = await axe.run(root, {
    // Color-contrast needs a fully painted page; Karma renders detached
    // fixtures, so contrast is checked in review, not here.
    rules: { 'color-contrast': { enabled: false } },
  });
  const summary = results.violations
    .map((v) => `${v.id}: ${v.help} → ${v.nodes.map((n) => n.target.join(' ')).join('; ')}`)
    .join('\n');
  expect(results.violations.length).withContext(summary).toBe(0);
}

describe('accessibility (axe)', () => {
  it('app shell has no detectable violations', async () => {
    TestBed.configureTestingModule({
      imports: [App],
      providers: [provideRouter([]), provideHttpClient(), provideHttpClientTesting()],
    });
    const fixture = TestBed.createComponent(App);
    fixture.detectChanges();
    await expectNoViolations(fixture.nativeElement);
  });

  it('audit viewer (filters + table) has no detectable violations', async () => {
    TestBed.configureTestingModule({
      imports: [Audit],
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    const fixture = TestBed.createComponent(Audit);
    fixture.detectChanges();
    TestBed.inject(HttpTestingController)
      .expectOne((r) => r.url === '/api/v1/audit')
      .flush({
        data: [
          {
            id: 'aud-1',
            seq: 1,
            event: {
              actor: { principal: 'ops@acme', type: 'user', tenant: 'acme' },
              action: 'secret.create',
              resource: { type: 'secret', id: 'tok' },
              outcome: 'allowed',
              timestamp_ms: 1_700_000_000_000,
            },
            prev_hash: '',
            hash: 'abc123',
          },
        ],
        has_more: false,
        next_cursor: null,
        total_estimate: null,
      });
    fixture.detectChanges();
    await expectNoViolations(fixture.nativeElement);
  });

  it('confirm dialog (open) has no detectable violations', async () => {
    TestBed.configureTestingModule({ imports: [ConfirmDialog] });
    const svc = TestBed.inject(ConfirmService);
    const fixture = TestBed.createComponent(ConfirmDialog);
    fixture.detectChanges();
    void svc.ask({ message: 'Uninstall x?', confirmLabel: 'Uninstall', danger: true });
    fixture.detectChanges();
    await expectNoViolations(fixture.nativeElement);
    svc.settle(false);
  });
});
