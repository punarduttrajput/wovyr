import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { WorkflowDraft, WorkflowService } from './workflow.service';

/**
 * RM-AIM-P1 UI-102: the visual-builder → workflow-DSL serializer is hand-rolled
 * YAML emission; a quoting/indentation slip produces a manifest the engine rejects
 * (or, worse, silently mis-parses). These specs pin the emitted shape.
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

  it('serializes every activity type with its field mapping', () => {
    const yaml = service.toWorkflowManifest(
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

    expect(yaml).toContain('  name: research-team');
    expect(yaml).toContain('  version: "1.0.0"');
    // function → tool id + compacted inputs
    expect(yaml).toContain('    - id: fetch\n      type: function\n      name: "http_get"');
    expect(yaml).toContain('      inputs: {"url":"https://x"}');
    // ai → instructions in name
    expect(yaml).toContain('      name: "Summarize the page."');
    // human → bare
    expect(yaml).toContain('    - id: gate\n      type: human');
    // wait → inputs: { event: "<name>" }
    expect(yaml).toContain('    - id: hold\n      type: wait\n      inputs: { event: "resume-signal" }');
    // transitions, with the when guard quoted only when present
    expect(yaml).toContain('    - from: fetch\n      to: summarize');
    expect(yaml).toContain('    - from: summarize\n      to: review\n      when: "input.deep == true"');
  });

  it('quotes and escapes names containing quotes/backslashes', () => {
    const yaml = service.toWorkflowManifest(
      draft({
        activities: [
          { id: 'a', type: 'ai', name: 'Say "hi" c:\\path', inputs: '', x: 0, y: 0 },
        ],
      }),
    );
    expect(yaml).toContain('name: "Say \\"hi\\" c:\\\\path"');
  });

  it('drops empty/`{}`/invalid inputs instead of emitting broken YAML', () => {
    const yaml = service.toWorkflowManifest(
      draft({
        activities: [
          { id: 'a', type: 'function', name: 'echo', inputs: '', x: 0, y: 0 },
          { id: 'b', type: 'function', name: 'echo', inputs: '{}', x: 0, y: 0 },
          { id: 'c', type: 'function', name: 'echo', inputs: '{not json', x: 0, y: 0 },
        ],
      }),
    );
    expect(yaml).not.toContain('inputs:');
  });

  it('skips transitions with a blank endpoint and omits the section when none remain', () => {
    const yaml = service.toWorkflowManifest(
      draft({
        activities: [{ id: 'a', type: 'human', name: '', inputs: '', x: 0, y: 0 }],
        transitions: [{ from: '', to: 'a', when: '' }],
      }),
    );
    expect(yaml).not.toContain('transitions:');
  });

  it('falls back to defaults for a blank name/version and a wait without an event name', () => {
    const yaml = service.toWorkflowManifest(
      draft({
        name: '  ',
        version: '',
        activities: [{ id: 'hold', type: 'wait', name: '', inputs: '', x: 0, y: 0 }],
      }),
    );
    expect(yaml).toContain('name: untitled-workflow');
    expect(yaml).toContain('version: "1.0.0"');
    // A wait with no event name suspends on its own id.
    expect(yaml).toContain('inputs: { event: "hold" }');
  });
});
