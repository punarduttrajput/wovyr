import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Subscription } from 'rxjs';
import { AgentService } from './agent.service';
import { errText } from '../../core/http-error';
import { AgentDraft, StreamEvent, ToolInfo } from '../../core/api.types';

/** The MCP connection name named by a tool id of the form
 * `mcp__<server>__<tool>` (the proxy id shape `wovyr-tools::mcp`'s
 * `McpClient::connect` produces) — `null` for a built-in/plugin tool id. */
function mcpServerOf(id: string): string | null {
  const m = /^mcp__([A-Za-z0-9_-]+)__/.exec(id);
  return m ? m[1] : null;
}

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
      'You are a documentation assistant for the Wovyr platform. Ground every answer in ' +
      'retrieved docs and cite the section. If unsure, say so rather than guessing.',
    tools: ['fs_read', 'http_get'],
    mcpServers: [],
    memoryEnabled: false,
    namespace: 'product-kb',
    maxSteps: null,
  };
  pickTool = '';
  message = 'How do durable timers work?';

  /** The tool catalog from the server (`GET /api/v1/tools` — built-ins + enabled plugin
   *  tools). Seeded with the built-ins so the picker works before the fetch resolves or
   *  if the server doesn't expose the endpoint. */
  readonly toolCatalog = signal<ToolInfo[]>([
    { id: 'fs_read', description: 'Read the contents of a text file at a given path.', category: 'builtin', permissions: [] },
    { id: 'http_get', description: 'Fetch a URL and return its status and body.', category: 'builtin', permissions: [] },
    { id: 'shell', description: 'Run a shell command; returns stdout, stderr, exit code.', category: 'builtin', permissions: [] },
    { id: 'echo', description: 'Echo the input back unchanged — handy for testing.', category: 'builtin', permissions: [] },
  ]);

  readonly agents = signal<string[]>([]);
  /** The saved agent currently loaded into the designer (for highlighting), if any. */
  readonly loadedId = signal<string | null>(null);
  readonly events = signal<StreamEvent[]>([]);
  readonly answer = signal('');
  /** Accumulated reasoning/thinking text, where the provider streams one (AIC-202). */
  readonly reasoning = signal('');
  readonly running = signal(false);
  readonly usage = signal<{ total_tokens?: number; cost_usd?: number } | undefined>(undefined);
  readonly status = signal('');
  readonly showDsl = signal(false);

  /** The console feed: every non-delta event, in arrival order. */
  readonly timeline = computed(() => this.events().filter((e) => e.kind !== 'delta'));

  /**
   * The run's real model/tool iteration count, taken from the terminal `result`
   * event — which is what the "Max steps" cap above the console actually bounds.
   *
   * This was previously rendered as `timeline().length` under a "Steps" label
   * (the signal was itself called `steps`), so the stat box counted *stream
   * frames*: a one-iteration run reported 3, because `start`/`done`/`result` are
   * three events. Misleading precisely where it matters, since an operator tuning
   * `max_steps` reads this number to decide whether the cap is close to binding.
   * Renamed the feed to `timeline` so the collision can't recur.
   */
  readonly stepCount = computed(
    () => this.timeline().find((e) => e.kind === 'result')?.steps ?? 0,
  );

  private sub?: Subscription;

  ngOnInit(): void {
    this.refresh();
    // Load the live tool catalog; keep the built-in fallback if the endpoint is
    // absent. A failure surfaces via the global error-toast interceptor (UI-302).
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
    this.reasoning.set('');
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
        this.reasoning.set('');
        this.usage.set(undefined);
        this.status.set('Loaded · ' + id);
      },
      error: (e) => this.status.set('Error: ' + errText(e)),
    });
  }

  /** Catalog tools not already on the draft — the dropdown's options. */
  availableTools(): ToolInfo[] {
    return this.toolCatalog().filter((t) => !this.draft.tools.includes(t.id));
  }

  /** Add the tool chosen from the dropdown, then reset it to the placeholder.
   * An `mcp__<server>__<tool>` pick (MCX-202/303) also adds `<server>` to
   * `spec.mcp_servers` — the actual allow-list a run resolves (MCX-201); the
   * literal tool id alone would be meaningless to the server without it. */
  addPicked(): void {
    const id = this.pickTool;
    if (id && !this.draft.tools.includes(id)) this.draft.tools.push(id);
    const server = mcpServerOf(id);
    if (server && !this.draft.mcpServers.includes(server)) this.draft.mcpServers.push(server);
    this.pickTool = '';
  }

  /** Whether `id` is an MCP-sourced tool (`mcp__<server>__<tool>`) — drives
   * the picker's "MCP" badge. */
  isMcpTool(id: string): boolean {
    return mcpServerOf(id) !== null;
  }

  removeMcpServer(name: string): void {
    this.draft.mcpServers = this.draft.mcpServers.filter((s) => s !== name);
    // Drop that connection's tools from spec.tools too — advertising them
    // without the connection named would be a dead reference at run time.
    this.draft.tools = this.draft.tools.filter((t) => mcpServerOf(t) !== name);
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
    this.reasoning.set('');
    this.usage.set(undefined);
    this.status.set('');
    this.running.set(true);
    this.sub = this.svc.runStream(this.manifest(), this.message).subscribe({
      next: (e) => {
        if (e.kind === 'delta') this.answer.update((a) => a + e.text);
        else if (e.kind === 'reasoning') this.reasoning.update((r) => r + e.text);
        else if (e.kind === 'tool_call_delta') this.coalesceToolDelta(e);
        else this.events.update((l) => [...l, e]);
        if (e.kind === 'done') this.usage.set(e.usage);
      },
      complete: () => this.running.set(false),
    });
  }

  /** Fold a tool-call-argument fragment (AIC-202) into the console feed: extend the
   * in-progress entry for the same call rather than adding a line per fragment. */
  private coalesceToolDelta(e: StreamEvent & { kind: 'tool_call_delta' }): void {
    this.events.update((l) => {
      const last = l[l.length - 1];
      if (last?.kind === 'tool_call_delta' && last.index === e.index) {
        return [...l.slice(0, -1), { ...last, name: e.name || last.name, arguments: last.arguments + e.arguments }];
      }
      return [...l, e];
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
      error: (e) => this.status.set('Error: ' + errText(e)),
    });
  }

  remove(id: string): void {
    if (this.loadedId() === id) this.loadedId.set(null);
    this.svc.deleteAgent(id).subscribe({ next: () => this.refresh(), error: () => this.refresh() });
  }

}
