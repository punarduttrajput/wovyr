import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { lastValueFrom } from 'rxjs';
import * as YAML from 'yaml';
import { WorkflowDraft, WorkflowService } from './workflow.service';

/**
 * RM-AIM-P1 UI-102 / UI-302: the visual-builder → workflow-DSL serializer now
 * emits through the `yaml` library, so these specs pin the *parsed meaning* of
 * the manifest (what the engine sees), not its byte shape — a quoting-style
 * change can no longer break them, but a field-mapping slip still does.
 */
describe('WorkflowService.toWorkflowManifest', () => {
  let service: WorkflowService;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(WorkflowService);
  });

  function draft(overrides: Partial<WorkflowDraft> = {}): WorkflowDraft {
    return {
      name: 'research-team',
      version: '1.0.0',
      activities: [],
      transitions: [],
      ...overrides,
    };
  }

  interface ParsedManifest {
    metadata: { name: string; version: string };
    spec: {
      activities: Array<Record<string, unknown>>;
      transitions?: Array<Record<string, unknown>>;
    };
  }

  const emit = (d: WorkflowDraft): ParsedManifest =>
    YAML.parse(service.toWorkflowManifest(d)) as ParsedManifest;

  it('serializes every activity type with its field mapping', () => {
    const m = emit(
      draft({
        activities: [
          { id: 'fetch', type: 'function', name: 'http_get', inputs: '{"url":"https://x"}', x: 0, y: 0 },
          { id: 'summarize', type: 'ai', name: 'Summarize the page.', inputs: '{}', x: 0, y: 0 },
          { id: 'review', type: 'agent', name: 'reviewer', inputs: '{"message":"go"}', x: 0, y: 0 },
          { id: 'gate', type: 'human', name: '', inputs: '', x: 0, y: 0 },
          { id: 'hold', type: 'wait', name: 'resume-signal', inputs: '', x: 0, y: 0 },
        ],
        transitions: [
          { from: 'fetch', to: 'summarize', when: '' },
          { from: 'summarize', to: 'review', when: 'input.deep == true' },
        ],
      }),
    );

    expect(m.metadata).toEqual({ name: 'research-team', version: '1.0.0' });
    // `version` must stay a YAML *string* — the engine rejects a numeric version.
    expect(typeof m.metadata.version).toBe('string');

    const [fetch, summarize, review, gate, hold] = m.spec.activities;
    // function → tool id + parsed inputs
    expect(fetch).toEqual({
      id: 'fetch',
      type: 'function',
      name: 'http_get',
      inputs: { url: 'https://x' },
    });
    // ai → instructions in name; empty `{}` inputs dropped
    expect(summarize).toEqual({ id: 'summarize', type: 'ai', name: 'Summarize the page.' });
    expect(review).toEqual({
      id: 'review',
      type: 'agent',
      name: 'reviewer',
      inputs: { message: 'go' },
    });
    // human → bare
    expect(gate).toEqual({ id: 'gate', type: 'human' });
    // wait → inputs: { event: <name> }
    expect(hold).toEqual({ id: 'hold', type: 'wait', inputs: { event: 'resume-signal' } });

    expect(m.spec.transitions).toEqual([
      { from: 'fetch', to: 'summarize' },
      { from: 'summarize', to: 'review', when: 'input.deep == true' },
    ]);
  });

  it('round-trips names containing quotes/backslashes intact', () => {
    const hostile = 'Say "hi" c:\\path';
    const m = emit(
      draft({
        activities: [{ id: 'a', type: 'ai', name: hostile, inputs: '', x: 0, y: 0 }],
      }),
    );
    expect(m.spec.activities[0]['name']).toBe(hostile);
  });

  it('drops empty/`{}`/invalid inputs instead of emitting broken YAML', () => {
    const m = emit(
      draft({
        activities: [
          { id: 'a', type: 'function', name: 'echo', inputs: '', x: 0, y: 0 },
          { id: 'b', type: 'function', name: 'echo', inputs: '{}', x: 0, y: 0 },
          { id: 'c', type: 'function', name: 'echo', inputs: '{not json', x: 0, y: 0 },
        ],
      }),
    );
    for (const a of m.spec.activities) expect(a['inputs']).toBeUndefined();
  });

  it('skips transitions with a blank endpoint and omits the section when none remain', () => {
    const m = emit(
      draft({
        activities: [{ id: 'a', type: 'human', name: '', inputs: '', x: 0, y: 0 }],
        transitions: [{ from: '', to: 'a', when: '' }],
      }),
    );
    expect(m.spec.transitions).toBeUndefined();
  });

  it('falls back to defaults for a blank name/version and a wait without an event name', () => {
    const m = emit(
      draft({
        name: '  ',
        version: '',
        activities: [{ id: 'hold', type: 'wait', name: '', inputs: '', x: 0, y: 0 }],
      }),
    );
    expect(m.metadata.name).toBe('untitled-workflow');
    expect(m.metadata.version).toBe('1.0.0');
    // A wait with no event name suspends on its own id.
    expect(m.spec.activities[0]['inputs']).toEqual({ event: 'hold' });
  });
});

/** UI-302 regression: `tools()` used to read a bare `{tools: [...]}` shape, but
 * `GET /api/v1/tools` returns the standard cursor-pagination envelope
 * (RM-GA-P4 API-701) — the same bug already fixed in `AgentService.tools()`;
 * the canvas's tool picker silently never saw the live catalog. */
describe('WorkflowService.tools', () => {
  let service: WorkflowService;
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(WorkflowService);
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => http.verify());

  it('parses the Page envelope, not a bare {tools: [...]} shape', async () => {
    const promise = lastValueFrom(service.tools());
    const req = http.expectOne('/api/v1/tools');
    expect(req.request.method).toBe('GET');
    req.flush({
      data: [{ id: 'echo', description: 'Echo tool.', category: 'builtin', permissions: [] }],
      has_more: false,
      next_cursor: null,
      total_estimate: 1,
    });
    expect((await promise).map((t) => t.id)).toEqual(['echo']);
  });
});
