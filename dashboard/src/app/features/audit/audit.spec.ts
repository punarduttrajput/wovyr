import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { AuditEntry } from '../../core/api.types';
import { Audit } from './audit';

/**
 * UI-303: the audit viewer's contract with `GET /api/v1/audit` (SEC-301) —
 * filter/query-param encoding, cursor pagination that appends, and the
 * RBAC-gated 403 rendering as an explanation instead of an error.
 */
describe('Audit', () => {
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      imports: [Audit],
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => http.verify());

  function entry(action: string, ts: number): AuditEntry {
    return {
      id: `aud-${ts}`,
      seq: ts,
      event: {
        actor: { principal: 'ops@acme', type: 'user', tenant: 'acme' },
        action,
        resource: { type: 'secret', id: 'api-token' },
        outcome: 'allowed',
        request_id: 'req-1',
        timestamp_ms: ts,
      },
      prev_hash: '',
      hash: `h-${action}-${ts}`,
    };
  }

  it('loads the first page on init and appends older pages via the cursor', () => {
    const fixture = TestBed.createComponent(Audit);
    fixture.detectChanges();

    const first = http.expectOne((r) => r.url === '/api/v1/audit');
    expect(first.request.params.get('limit')).toBe('50');
    expect(first.request.params.has('cursor')).toBeFalse();
    first.flush({
      data: [entry('secrets.create', 2000), entry('secrets.rotate', 1500)],
      has_more: true,
      next_cursor: 'c1',
      total_estimate: null,
    });
    fixture.detectChanges();

    expect(fixture.componentInstance.entries().length).toBe(2);
    expect(fixture.componentInstance.hasMore()).toBeTrue();

    fixture.componentInstance.load();
    const second = http.expectOne((r) => r.url === '/api/v1/audit');
    expect(second.request.params.get('cursor')).toBe('c1');
    second.flush({
      data: [entry('kms.rotate', 1000)],
      has_more: false,
      next_cursor: null,
      total_estimate: null,
    });
    fixture.detectChanges();

    // Appended, not replaced — the older page extends the list.
    expect(fixture.componentInstance.entries().map((e) => e.event.action)).toEqual([
      'secrets.create',
      'secrets.rotate',
      'kms.rotate',
    ]);
    expect(fixture.componentInstance.hasMore()).toBeFalse();
  });

  it('encodes principal/action/time-range filters as query params', () => {
    const fixture = TestBed.createComponent(Audit);
    fixture.detectChanges();
    http
      .expectOne((r) => r.url === '/api/v1/audit')
      .flush({ data: [], has_more: false, next_cursor: null, total_estimate: null });

    const c = fixture.componentInstance;
    c.principal = 'ops@acme';
    c.action = 'secrets.create';
    c.from = '2026-07-01T00:00';
    c.apply();

    const req = http.expectOne((r) => r.url === '/api/v1/audit');
    expect(req.request.params.get('principal')).toBe('ops@acme');
    expect(req.request.params.get('action')).toBe('secrets.create');
    expect(req.request.params.get('after_ms')).toBe(
      String(new Date('2026-07-01T00:00').getTime()),
    );
    expect(req.request.params.has('before_ms')).toBeFalse();
    req.flush({ data: [], has_more: false, next_cursor: null, total_estimate: null });
  });

  it('renders a 403 as the RBAC explanation, not an error', () => {
    const fixture = TestBed.createComponent(Audit);
    fixture.detectChanges();
    http
      .expectOne((r) => r.url === '/api/v1/audit')
      .flush(
        { error: { message: 'missing scope audit:read' } },
        { status: 403, statusText: 'Forbidden' },
      );
    fixture.detectChanges();

    expect(fixture.componentInstance.forbidden()).toBeTrue();
    expect(fixture.componentInstance.error()).toBe('');
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('audit:read');
  });
});
