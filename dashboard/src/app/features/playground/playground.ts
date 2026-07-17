import { Component, OnDestroy, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Subscription } from 'rxjs';
import { AgentService } from '../agent-studio/agent.service';
import { StreamEvent } from '../../core/api.types';

/**
 * UI-306: a lightweight prompt playground — try a system prompt + message
 * against the gateway without authoring a full agent (prompt iteration used to
 * be bolted onto Agent Studio's run console). Builds a minimal inline manifest
 * (no tools, no memory) and streams the answer, reasoning, and usage back.
 */
@Component({
  selector: 'app-playground',
  imports: [FormsModule],
  templateUrl: './playground.html',
  styleUrl: './playground.scss',
})
export class Playground implements OnDestroy {
  private agents = inject(AgentService);

  system = 'You are a concise assistant.';
  message = 'In two sentences: what is Apex?';
  /** Model class for the selector — or a pinned model that overrides it. */
  modelClass = 'balanced';
  pinnedModel = '';

  readonly answer = signal('');
  readonly reasoning = signal('');
  readonly running = signal(false);
  readonly status = signal('');
  readonly usage = signal<{ total_tokens?: number; cost_usd?: number } | undefined>(undefined);
  readonly showReasoning = signal(false);

  private sub?: Subscription;

  ngOnDestroy(): void {
    this.stop();
  }

  manifest(): string {
    const draft = this.agents.blankDraft();
    draft.name = 'playground';
    draft.instructions = this.system.trim() || 'You are a helpful assistant.';
    draft.class = this.modelClass;
    draft.pinnedModel = this.pinnedModel;
    draft.memoryEnabled = false;
    draft.tools = [];
    return this.agents.toManifest(draft);
  }

  run(): void {
    this.stop();
    this.answer.set('');
    this.reasoning.set('');
    this.usage.set(undefined);
    this.status.set('');
    this.running.set(true);
    this.sub = this.agents.runStream(this.manifest(), this.message).subscribe({
      next: (e: StreamEvent) => {
        if (e.kind === 'delta') this.answer.update((a) => a + e.text);
        else if (e.kind === 'reasoning') this.reasoning.update((r) => r + e.text);
        else if (e.kind === 'done') this.usage.set(e.usage);
        else if (e.kind === 'error') this.status.set('Error: ' + e.message);
      },
      complete: () => this.running.set(false),
    });
  }

  stop(): void {
    this.sub?.unsubscribe();
    this.sub = undefined;
    this.running.set(false);
  }
}
