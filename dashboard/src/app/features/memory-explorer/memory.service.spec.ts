import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { MemoryService } from './memory.service';

/**
 * Pins the response envelopes this service reads. Added 2026-08-04 after
 * `query()` was found parsing `{results}` while `POST /api/v1/memory:query`
 * answers `{data, count}` — API-701 renamed that field and this service was
 * missed, so Memory Explorer's search silently reported "0 matches" for every
 * query while browsing records kept working. Nothing failed loudly, because a
 * `?? []` fallback turned the shape mismatch into an empty array.
 *
 * These are shape assertions, not behaviour assertions: a rename on the server
 * cannot produce a TypeScript error in a client that declares its own response
 * interface inline, so a test that flushes the *real* wire shape is the only
 * thing that catches it.
 */
describe('MemoryService', () => {
  let service: MemoryService;
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(MemoryService);
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => http.verify());

  const record = {
    id: 'mem-games-2',
    namespace: 'games',
    content: 'Elden Ring was developed by FromSoftware and released on 25 February 2022.',
    type: 'semantic',
    importance: 0.5,
    tags: ['catalog'],
    score: 0.825,
    breakdown: { relevance: 1.0, recency: 1.0, importance: 0.5, total: 0.825 },
  };

  it('query() reads the `data` field of the {data, count} envelope', () => {
    let got: unknown[] | undefined;
    service.query({ text: 'who developed Elden Ring', strategy: 'keyword' }).subscribe((r) => (got = r));

    const req = http.expectOne('/api/v1/memory:query');
    expect(req.request.method).toBe('POST');
    // The exact envelope `memory:query` returns — the one documented exception to
    // the cursor-pagination shape, since a ranked top-K set has no stable order.
    req.flush({ data: [record], count: 1 });

    expect(got?.length).toBe(1);
    expect((got?.[0] as { id: string }).id).toBe('mem-games-2');
  });

  it('query() sends strategy and namespace through untouched', () => {
    service.query({ text: 'refund', namespace: 'games', strategy: 'keyword', limit: 3 }).subscribe();
    const req = http.expectOne('/api/v1/memory:query');
    expect(req.request.body).toEqual({
      text: 'refund',
      namespace: 'games',
      strategy: 'keyword',
      limit: 3,
    });
    req.flush({ data: [], count: 0 });
  });

  it('records() reads the cursor-paginated `data` field', () => {
    let got: unknown[] | undefined;
    service.records('games', 50).subscribe((r) => (got = r));

    const req = http.expectOne('/api/v1/memory/records?limit=50&namespace=games');
    expect(req.request.method).toBe('GET');
    req.flush({ data: [record], has_more: false, next_cursor: null, total_estimate: 1 });

    expect(got?.length).toBe(1);
  });

  it('namespaces() reads the `namespaces` field', () => {
    let got: unknown[] | undefined;
    service.namespaces().subscribe((r) => (got = r));

    const req = http.expectOne('/api/v1/memory/namespaces');
    req.flush({ namespaces: [{ namespace: 'games', count: 6 }], total: 6 });

    expect(got?.length).toBe(1);
  });
});
