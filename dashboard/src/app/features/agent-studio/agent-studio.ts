import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Subscription } from 'rxjs';
import { AgentService } from './agent.service';
import { AgentDraft, StreamEvent } from '../../core/api.types';

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
  newTool = '';
  message = 'How do durable timers work?';

  readonly agents = signal<string[]>([]);
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

  addTool(): void {
    const t = this.newTool.trim();
    if (t && !this.draft.tools.includes(t)) this.draft.tools.push(t);
    this.newTool = '';
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
        this.refresh();
      },
      error: (e) => this.status.set('Error: ' + this.errText(e)),
    });
  }

  remove(id: string): void {
    this.svc.deleteAgent(id).subscribe({ next: () => this.refresh(), error: () => this.refresh() });
  }

  private errText(e: unknown): string {
    const err = e as { error?: { error?: { message?: string } }; message?: string };
    return err?.error?.error?.message ?? err?.message ?? 'request failed';
  }
}
