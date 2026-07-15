import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { lastValueFrom } from 'rxjs';
import { toArray } from 'rxjs/operators';
import { AgentService } from './agent.service';
import { AgentDraft, StreamEvent } from '../../core/api.types';

/**
 * RM-AIM-P1 UI-102: specs for the two riskiest pieces of hand-rolled logic in the
 * studio — the SSE stream parser (a manual wire-format implementation over `fetch`)
 * and the YAML manifest round-trip (a hand-rolled parser/serializer pair that must
 * stay inverses of each other or the editor silently corrupts agents on open).
 */
describe('AgentService', () => {
  let service: AgentService;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(AgentService);
  });

  // ---- manifest round-trip ---------------------------------------------------

  describe('manifest round-trip (toManifest ∘ fromManifest)', () => {
    it('round-trips a fully-populated draft', () => {
      const draft: AgentDraft = {
        name: 'support-bot',
        pinnedModel: '',
        capability: 'chat',
        class: 'fast',
        instructions: 'You are a support agent.\nAnswer briefly.',
        tools: ['echo', 'http_get'],
        mcpServers: ['docs'],
        memoryEnabled: true,
        namespace: 'support-kb',
        maxSteps: 12,
      };

      const back = service.fromManifest(service.toManifest(draft));

      expect(back.name).toBe('support-bot');
      expect(back.capability).toBe('chat');
      expect(back.class).toBe('fast');
      expect(back.instructions).toBe('You are a support agent.\nAnswer briefly.');
      expect(back.tools).toEqual(['echo', 'http_get']);
      expect(back.mcpServers).toEqual(['docs']);
      expect(back.memoryEnabled).toBeTrue();
      expect(back.namespace).toBe('support-kb');
      expect(back.maxSteps).toBe(12);
    });

    it('round-trips with no mcp_servers (the common case)', () => {
      const draft = service.blankDraft();
      draft.name = 'no-mcp';
      draft.instructions = 'Hi.';

      const yaml = service.toManifest(draft);
      expect(yaml).not.toContain('mcp_servers');
      expect(service.fromManifest(yaml).mcpServers).toEqual([]);
    });

    it('round-trips a pinned model instead of a selector', () => {
      const draft = service.blankDraft();
      draft.name = 'pinned';
      draft.pinnedModel = 'gpt-4o-mini';
      draft.instructions = 'Hi.';

      const yaml = service.toManifest(draft);
      expect(yaml).toContain('model: gpt-4o-mini');
      expect(yaml).not.toContain('model_selector');

      const back = service.fromManifest(yaml);
      expect(back.pinnedModel).toBe('gpt-4o-mini');
    });

    it('round-trips a minimal draft (no tools, no memory, no max_steps)', () => {
      const draft = service.blankDraft();
      draft.name = 'minimal';
      draft.instructions = 'Do the thing.';

      const yaml = service.toManifest(draft);
      const back = service.fromManifest(yaml);

      expect(back.name).toBe('minimal');
      expect(back.tools).toEqual([]);
      expect(back.memoryEnabled).toBeFalse();
      expect(back.maxSteps).toBeNull();
      expect(back.instructions).toBe('Do the thing.');
    });

    it('parses the real hello-agent example manifest shape', () => {
      const yaml = [
        'apiVersion: agent.apex.io/v1',
        'kind: Agent',
        'metadata:',
        '  name: hello',
        'spec:',
        '  model_selector: { capability: chat, class: fast }',
        '  instructions: |',
        '    You are a friendly assistant. Greet the user and answer briefly.',
        '',
      ].join('\n');

      const draft = service.fromManifest(yaml);
      expect(draft.name).toBe('hello');
      expect(draft.class).toBe('fast');
      expect(draft.instructions).toContain('friendly assistant');
    });
  });

  // ---- tool catalog -----------------------------------------------------------

  /** Regression test: `tools()` used to read a bare `{tools: [...]}` shape, but
   * `GET /api/v1/tools` actually returns the standard cursor-pagination
   * envelope (RM-GA-P4 API-701) — so the live catalog (including MCX-202's
   * `mcp__<server>__<tool>` entries) silently never replaced the picker's
   * hardcoded built-in fallback. */
  describe('tools()', () => {
    let http: HttpTestingController;

    beforeEach(() => {
      http = TestBed.inject(HttpTestingController);
    });

    afterEach(() => http.verify());

    it('parses the Page envelope, not a bare {tools: [...]} shape', async () => {
      const promise = lastValueFrom(service.tools());
      const req = http.expectOne('/api/v1/tools');
      expect(req.request.method).toBe('GET');
      req.flush({
        data: [
          { id: 'echo', description: 'Echo tool.', category: 'builtin', permissions: [] },
          { id: 'mcp__docs__search_docs', description: 'Search docs.', category: 'mcp', permissions: ['mcp:docs'] },
        ],
        has_more: false,
        next_cursor: null,
        total_estimate: 2,
      });
      const tools = await promise;
      expect(tools.map((t) => t.id)).toEqual(['echo', 'mcp__docs__search_docs']);
    });
  });

  // ---- SSE stream parser -------------------------------------------------------

  /** A streaming Response whose body arrives in the given chunks. */
  function sseResponse(chunks: string[], status = 200): Response {
    const enc = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const c of chunks) controller.enqueue(enc.encode(c));
        controller.close();
      },
    });
    return new Response(stream, {
      status,
      headers: { 'Content-Type': 'text/event-stream' },
    });
  }

  function collect(): Promise<StreamEvent[]> {
    return lastValueFrom(service.runStream('manifest', 'hi').pipe(toArray()));
  }

  describe('runStream SSE parsing', () => {
    it('parses run events and the terminal result frame', async () => {
      spyOn(window, 'fetch').and.resolveTo(
        sseResponse([
          'data: {"type":"start","model":"mock-model","provider":"mock"}\n\n',
          'data: {"type":"delta","text":"Hel"}\n\n',
          'data: {"type":"delta","text":"lo"}\n\n',
          'data: {"type":"reasoning","text":"let me echo"}\n\n',
          'data: {"type":"tool_call_delta","index":0,"name":"echo","arguments":"{\\"x\\""}\n\n',
          'data: {"type":"tool_call","name":"echo","arguments":"{}"}\n\n',
          'data: {"type":"tool_result","name":"echo","ok":true}\n\n',
          'data: {"type":"done","usage":{"total_tokens":7}}\n\n',
          'event: result\ndata: {"status":"succeeded","output":"Hello","steps":1}\n\n',
        ]),
      );

      const events = await collect();
      expect(events.map((e) => e.kind)).toEqual([
        'start',
        'delta',
        'delta',
        'reasoning',
        'tool_call_delta',
        'tool_call',
        'tool_result',
        'done',
        'result',
      ]);
      expect(events[1]).toEqual(jasmine.objectContaining({ kind: 'delta', text: 'Hel' }));
      expect(events[3]).toEqual(
        jasmine.objectContaining({ kind: 'reasoning', text: 'let me echo' }),
      );
      expect(events[4]).toEqual(
        jasmine.objectContaining({
          kind: 'tool_call_delta',
          index: 0,
          name: 'echo',
          arguments: '{"x"',
        }),
      );
      expect(events[5]).toEqual(jasmine.objectContaining({ kind: 'tool_call', name: 'echo' }));
      expect(events[8]).toEqual(
        jasmine.objectContaining({ kind: 'result', status: 'succeeded', steps: 1 }),
      );
    });

    it('reassembles frames split across arbitrary chunk boundaries', async () => {
      // One delta frame delivered byte-dribbled across reads, then a second frame
      // whose separator spans a chunk boundary — the classic streaming pitfalls.
      spyOn(window, 'fetch').and.resolveTo(
        sseResponse([
          'data: {"type":"del',
          'ta","text":"chunked"}\n',
          '\ndata: {"type":"done","usage":null}\n\n',
        ]),
      );

      const events = await collect();
      expect(events.map((e) => e.kind)).toEqual(['delta', 'done']);
      expect(events[0]).toEqual(jasmine.objectContaining({ text: 'chunked' }));
    });

    it('ignores SSE comments and unknown event types without erroring', async () => {
      spyOn(window, 'fetch').and.resolveTo(
        sseResponse([
          ': keep-alive comment\n\n',
          'data: {"type":"someday-a-new-event"}\n\n',
          'data: {"type":"delta","text":"ok"}\n\n',
        ]),
      );

      const events = await collect();
      expect(events.map((e) => e.kind)).toEqual(['delta']);
    });

    it('surfaces an HTTP error as a terminal error event', async () => {
      spyOn(window, 'fetch').and.resolveTo(
        new Response('quota exceeded', { status: 429, statusText: 'Too Many Requests' }),
      );

      const events = await collect();
      expect(events.length).toBe(1);
      expect(events[0].kind).toBe('error');
      expect((events[0] as { message: string }).message).toContain('429');
    });

    it('parses the error event frame', async () => {
      spyOn(window, 'fetch').and.resolveTo(
        sseResponse(['event: error\ndata: model exploded\n\n']),
      );

      const events = await collect();
      expect(events).toEqual([
        jasmine.objectContaining({ kind: 'error', message: 'model exploded' }),
      ]);
    });
  });
});
