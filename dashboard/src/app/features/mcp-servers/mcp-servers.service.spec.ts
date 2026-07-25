import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { McpServersService } from './mcp-servers.service';

/** PRD-006: pins the exact request shape this service sends against the MCP
 * connection-management routes — a drifted method/URL/body here would
 * silently 404/400 against a real `wovyr-server` without any type error. */
describe('McpServersService', () => {
  let service: McpServersService;
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(McpServersService);
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => http.verify());

  it('list() GETs /api/v1/mcp/connections and unwraps the Page + stdio_enabled envelope', () => {
    let result: { connections: unknown[]; stdioEnabled: boolean } | undefined;
    service.list().subscribe((r) => (result = r));

    const req = http.expectOne('/api/v1/mcp/connections');
    expect(req.request.method).toBe('GET');
    req.flush({
      data: [{ name: 'docs', transport: { kind: 'http', url: 'https://x' }, created_ms: 1, updated_ms: 1 }],
      has_more: false,
      next_cursor: null,
      total_estimate: 1,
      stdio_enabled: true,
    });

    expect(result?.connections.length).toBe(1);
    expect(result?.stdioEnabled).toBeTrue();
  });

  it('create() POSTs the connection body', () => {
    service
      .create({ name: 'docs', transport: { kind: 'http', url: 'https://x' } })
      .subscribe();
    const req = http.expectOne('/api/v1/mcp/connections');
    expect(req.request.method).toBe('POST');
    expect(req.request.body).toEqual({
      name: 'docs',
      transport: { kind: 'http', url: 'https://x' },
    });
    req.flush({
      name: 'docs',
      transport: { kind: 'http', url: 'https://x' },
      created_ms: 1,
      updated_ms: 1,
      tools: [],
    });
  });

  it('delete() DELETEs the named connection, URL-encoded', () => {
    service.delete('weird name').subscribe();
    const req = http.expectOne('/api/v1/mcp/connections/weird%20name');
    expect(req.request.method).toBe('DELETE');
    req.flush(null);
  });

  it('refresh() POSTs to the refresh sub-route with an empty body', () => {
    service.refresh('docs').subscribe();
    const req = http.expectOne('/api/v1/mcp/connections/docs/refresh');
    expect(req.request.method).toBe('POST');
    expect(req.request.body).toEqual({});
    req.flush({ name: 'docs', tools: [] });
  });
});
