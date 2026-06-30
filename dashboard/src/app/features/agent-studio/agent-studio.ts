import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Subscription } from 'rxjs';
import { AgentService } from './agent.service';
import { AgentDraft, StreamEvent, ToolInfo } from '../../core/api.types';

@Component({
  selector: 'app-agent-studio',
  imports: [FormsModule],
  templateUrl: './agent-studio.html',
  styleUrl: './agent-studio.scss',
})
export class AgentStudio implements OnInit, OnDestroy {
  private svc = inject(AgentService);

  /** The agent under design — the source of truth the manifest is serialized from. */
  draft: AgentDraft = {
    name: 'docs-bot',
    pinnedModel: '',
    capability: 'chat',
    class: 'balanced',
    instructions:
      'You are a documentation assistant for the Apex platform. Ground every answer in ' +
      'retrieved docs and cite the section. If unsure, say so rather than guessing.',
    tools: ['fs_read', 'http_get'],
    memoryEnabled: false,
    namespace: 'product-kb',
  };
  pickTool = '';
  message = 'How do durable timers work?';

  /** The tool catalog from the server (`GET /api/v1/tools` — built-ins + enabled plugin
   *  tools). Seeded with the built-ins so the picker works before the fetch resolves or
   *  if the server doesn't expose the endpoint. */
  readonly toolCatalog = signal<ToolInfo[]>([
    { id: 'fs_read', description: 'Read the contents of a text file at a given path.' },
    { id: 'http_get', description: 'Fetch a URL and return its status and body.' },
    { id: 'shell', description: 'Run a shell command; returns stdout, stderr, exit code.' },
    { id: 'echo', description: 'Echo the input back unchanged — handy for testing.' },
  ]);

  readonly agents = signal<string[]>([]);
  /** The saved agent currently loaded into the designer (for highlighting), if any. */
  readonly loadedId = signal<string | null>(null);
  readonly events = signal<StreamEvent[]>([]);
  readonly answer = signal('');
  readonly running = signal(false);
  readonly usage = signal<{ total_tokens?: number; cost_usd?: number } | undefined>(undefined);
  readonly status = signal('');
  readonly showDsl = signal(false);

  /** Steps shown in the console feed = every non-delta event. */
  readonly steps = computed(() => this.events().filter((e) => e.kind !== 'delta'));

  private sub?: Subscription;

  ngOnInit(): void {
    this.refresh();
    // Load the live tool catalog; keep the built-in fallback if the endpoint is absent.
    this.svc.tools().subscribe({
      next: (t) => {
        if (t.length) this.toolCatalog.set(t);
      },
      error: () => {},
    });
  }

  ngOnDestroy(): void {
    this.stop();
  }

  manifest(): string {
    return this.svc.toManifest(this.draft);
  }

  refresh(): void {
    this.svc.listAgents().subscribe({
      next: (p) => this.agents.set(p.data ?? []),
      error: () => this.agents.set([]),
    });
  }

  /** Reset the designer to a blank draft to author a new agent. */
  newAgent(): void {
    this.draft = this.svc.blankDraft();
    this.loadedId.set(null);
    this.events.set([]);
    this.answer.set('');
    this.usage.set(undefined);
    this.status.set('New draft — edit and Save.');
  }

  /** Load a saved agent's manifest back into the designer for editing. */
  load(id: string): void {
    this.svc.getAgent(id).subscribe({
      next: (r) => {
        this.draft = this.svc.fromManifest(r.manifest);
        this.loadedId.set(id);
        this.events.set([]);
        this.answer.set('');
        this.usage.set(undefined);
        this.status.set('Loaded · ' + id);
      },
      error: (e) => this.status.set('Error: ' + this.errText(e)),
    });
  }

  /** Catalog tools not already on the draft — the dropdown's options. */
  availableTools(): ToolInfo[] {
    return this.toolCatalog().filter((t) => !this.draft.tools.includes(t.id));
  }

  /** Add the tool chosen from the dropdown, then reset it to the placeholder. */
  addPicked(): void {
    const id = this.pickTool;
    if (id && !this.draft.tools.includes(id)) this.draft.tools.push(id);
    this.pickTool = '';
  }

  /** A tool's description for chip/option tooltips. */
  toolDesc(id: string): string {
    return this.toolCatalog().find((t) => t.id === id)?.description ?? 'Plugin tool.';
  }

  removeTool(t: string): void {
    this.draft.tools = this.draft.tools.filter((x) => x !== t);
  }

  run(): void {
    this.stop();
    this.events.set([]);
    this.answer.set('');
    this.usage.set(undefined);
    this.status.set('');
    this.running.set(true);
    this.sub = this.svc.runStream(this.manifest(), this.message).subscribe({
      next: (e) => {
        if (e.kind === 'delta') this.answer.update((a) => a + e.text);
        else this.events.update((l) => [...l, e]);
        if (e.kind === 'done') this.usage.set(e.usage);
      },
      complete: () => this.running.set(false),
    });
  }

  stop(): void {
    this.sub?.unsubscribe();
    this.sub = undefined;
    this.running.set(false);
  }

  save(): void {
    this.status.set('Saving…');
    this.svc.createAgent(this.manifest()).subscribe({
      next: (r) => {
        this.status.set('Saved · ' + r.id);
        this.loadedId.set(r.id);
        this.refresh();
      },
      error: (e) => this.status.set('Error: ' + this.errText(e)),
    });
  }

  remove(id: string): void {
    if (this.loadedId() === id) this.loadedId.set(null);
    this.svc.deleteAgent(id).subscribe({ next: () => this.refresh(), error: () => this.refresh() });
  }

  private errText(e: unknown): string {
    const err = e as { error?: { error?: { message?: string } }; message?: string };
    return err?.error?.error?.message ?? err?.message ?? 'request failed';
  }
}
