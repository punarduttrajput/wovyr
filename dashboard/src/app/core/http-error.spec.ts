import { TestBed } from '@angular/core/testing';
import { HttpClient, provideHttpClient, withInterceptors } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import {
  errText,
  httpErrorInterceptor,
  resetHttpErrorDedupe,
  silentErrors,
} from './http-error';
import { ToastService } from './toast.service';

/**
 * UI-302: the central HTTP error layer. The interceptor is the guarantee that a
 * failed request — including a background poll whose subscriber ignores errors —
 * always surfaces to the user; these specs pin that guarantee.
 */
describe('errText', () => {
  it('unwraps the server error envelope', () => {
    expect(errText({ error: { error: { message: 'tenant mismatch' } } })).toBe('tenant mismatch');
  });

  it('falls back to a plain-text body, then the transport message', () => {
    expect(errText({ error: 'quota exceeded' })).toBe('quota exceeded');
    expect(errText({ message: 'Http failure response' })).toBe('Http failure response');
    expect(errText({})).toBe('request failed');
  });

  it('never shows an HTML error page as the message', () => {
    expect(errText({ error: '<html><body>502</body></html>', message: 'Http failure' })).toBe(
      'Http failure',
    );
  });
});

describe('httpErrorInterceptor', () => {
  let http: HttpClient;
  let ctl: HttpTestingController;
  let toast: ToastService;

  beforeEach(() => {
    resetHttpErrorDedupe();
    TestBed.configureTestingModule({
      providers: [
        provideHttpClient(withInterceptors([httpErrorInterceptor])),
        provideHttpClientTesting(),
      ],
    });
    http = TestBed.inject(HttpClient);
    ctl = TestBed.inject(HttpTestingController);
    toast = TestBed.inject(ToastService);
  });

  afterEach(() => ctl.verify());

  function failOnce(url = '/api/v1/things'): void {
    // The subscriber swallows the error — exactly the `error: () => {}` pattern.
    http.get(url).subscribe({ next: () => {}, error: () => {} });
    ctl
      .expectOne(url)
      .flush({ error: { message: 'store unavailable' } }, { status: 502, statusText: 'Bad Gateway' });
  }

  it('toasts a failed request even when the caller swallows the error', () => {
    failOnce();
    const toasts = toast.toasts();
    expect(toasts.length).toBe(1);
    expect(toasts[0].kind).toBe('err');
    expect(toasts[0].message).toContain('store unavailable');
    expect(toasts[0].message).toContain('/api/v1/things');
  });

  it('dedupes an identical repeating failure (a 5s poll complains once, not forever)', () => {
    failOnce();
    failOnce();
    failOnce();
    expect(toast.toasts().length).toBe(1);
  });

  it('stays quiet for requests marked silentErrors()', () => {
    http
      .get('/metrics', { context: silentErrors() })
      .subscribe({ next: () => {}, error: () => {} });
    ctl.expectOne('/metrics').flush('down', { status: 503, statusText: 'Unavailable' });
    expect(toast.toasts().length).toBe(0);
  });

  it('rethrows so component-level handlers still run', () => {
    let seen: unknown = null;
    http.get('/api/v1/things').subscribe({ next: () => {}, error: (e) => (seen = e) });
    ctl
      .expectOne('/api/v1/things')
      .flush({ error: { message: 'nope' } }, { status: 400, statusText: 'Bad Request' });
    expect(seen).not.toBeNull();
  });
});
