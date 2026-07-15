import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { McpServers } from './mcp-servers';

/** Regression coverage for the args tokenizer: a naive `split(/\s+/)` would
 * shatter a single quoted argument (a `node -e "<script>"` payload, or any
 * path containing a space — this repo's own checkout path is a real example)
 * into multiple bogus args. */
describe('McpServers', () => {
  let component: McpServers;
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      imports: [McpServers],
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    const fixture = TestBed.createComponent(McpServers);
    component = fixture.componentInstance;
    http = TestBed.inject(HttpTestingController);
    fixture.detectChanges(); // triggers ngOnInit's initial list() call
    // Drain the initial list() call from ngOnInit.
    http.expectOne('/api/v1/mcp/connections').flush({
      data: [],
      has_more: false,
      next_cursor: null,
      total_estimate: 0,
      stdio_enabled: true,
    });
  });

  afterEach(() => http.verify());

  it('splits unquoted args on whitespace', () => {
    component.kind = 'stdio';
    component.name = 'fs';
    component.command = 'npx';
    component.args = '-y @modelcontextprotocol/server-filesystem /data/docs';
    component.create();

    const req = http.expectOne('/api/v1/mcp/connections');
    expect(req.request.body.transport).toEqual({
      kind: 'stdio',
      command: 'npx',
      args: ['-y', '@modelcontextprotocol/server-filesystem', '/data/docs'],
    });
    req.flush({ name: 'fs', transport: req.request.body.transport, created_ms: 1, updated_ms: 1, tools: [] });
    http.expectOne('/api/v1/mcp/connections').flush({
      data: [],
      has_more: false,
      next_cursor: null,
      total_estimate: 0,
      stdio_enabled: true,
    });
  });

  it('keeps a quoted argument containing spaces as one element', () => {
    component.kind = 'stdio';
    component.name = 'inline';
    component.command = 'node';
    component.args = `-e "console.log('hi there')"`;
    component.create();

    const req = http.expectOne('/api/v1/mcp/connections');
    expect(req.request.body.transport).toEqual({
      kind: 'stdio',
      command: 'node',
      args: ['-e', "console.log('hi there')"],
    });
    req.flush({ name: 'inline', transport: req.request.body.transport, created_ms: 1, updated_ms: 1, tools: [] });
    http.expectOne('/api/v1/mcp/connections').flush({
      data: [],
      has_more: false,
      next_cursor: null,
      total_estimate: 0,
      stdio_enabled: true,
    });
  });
});
