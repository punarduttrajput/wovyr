import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
import * as YAML from 'yaml';
import { AgentDraft, Page, StreamEvent, ToolInfo } from '../../core/api.types';

/**
 * Client for the Agents API on apex-server. CRUD goes over HttpClient; the run stream
 * uses the fetch streaming API because `agents:stream` is a POST (EventSource is GET-only).
 * Requests target `/api/v1/...` — the dev server proxies that to apex-server (see
 * proxy.conf.json); in production it is same-origin behind the gateway.
 */
@Injectable({ providedIn: 'root' })
export class AgentService {
  private http = inject(HttpClient);

  /** List stored agent ids (cursor-paginated). */
  listAgents(): Observable<Page<string>> {
    return this.http.get<Page<string>>('/api/v1/agents');
  }

  /** The registered tool catalog for the picker: built-ins, enabled plugin
   * tools, and (MCX-202) the caller's tenant's configured MCP connections'
   * currently-discovered tools. `GET /api/v1/tools` returns the standard
   * cursor-pagination envelope (`{data, has_more, ...}`, RM-GA-P4 API-701) —
   * not a bare `{tools: [...]}` — mirroring `listAgents()`'s own `Page<T>`
   * parsing below. */
  tools(): Observable<ToolInfo[]> {
    return this.http.get<Page<ToolInfo>>('/api/v1/tools').pipe(map((r) => r.data ?? []));
  }

  /** Register an agent from its YAML manifest; returns its id. */
  createAgent(manifest: string): Observable<{ id: string }> {
    return this.http.post<{ id: string }>('/api/v1/agents', { manifest });
  }

  /** Fetch a stored agent's manifest by id. */
  getAgent(id: string): Observable<{ id: string; manifest: string }> {
    return this.http.get<{ id: string; manifest: string }>(
      `/api/v1/agents/${encodeURIComponent(id)}`,
    );
  }

  /** Remove a stored agent. */
  deleteAgent(id: string): Observable<void> {
    return this.http.delete<void>(`/api/v1/agents/${encodeURIComponent(id)}`);
  }

  /** A blank draft (the "+ New" starting point). */
  blankDraft(): AgentDraft {
    return {
      name: '',
      pinnedModel: '',
      capability: 'chat',
      class: 'balanced',
      instructions: '',
      tools: [],
      mcpServers: [],
      memoryEnabled: false,
      namespace: 'product-kb',
      maxSteps: null,
    };
  }

  /**
   * Parse a manifest back into an [`AgentDraft`] — the inverse of [`toManifest`].
   * UI-302: a real YAML parse (the `yaml` library) replaced the old line-regex
   * walker, so any valid manifest loads — not just the byte shape this studio
   * happens to emit. Unknown/extra fields are ignored.
   */
  fromManifest(yaml: string): AgentDraft {
    const d = this.blankDraft();
    let doc: unknown;
    try {
      doc = YAML.parse(yaml);
    } catch {
      return d; // unparseable → blank draft (the editor shows the raw text anyway)
    }
    const m = doc as {
      metadata?: { name?: unknown };
      spec?: {
        model?: unknown;
        model_selector?: { capability?: unknown; class?: unknown };
        instructions?: unknown;
        tools?: unknown;
        mcp_servers?: unknown;
        max_steps?: unknown;
        memory?: { namespace?: unknown };
      };
    } | null;
    const strings = (v: unknown): string[] =>
      Array.isArray(v) ? v.map((x) => String(x).trim()).filter(Boolean) : [];

    d.name = typeof m?.metadata?.name === 'string' ? m.metadata.name : '';
    const spec = m?.spec;
    if (typeof spec?.model === 'string') d.pinnedModel = spec.model;
    if (spec?.model_selector && typeof spec.model_selector === 'object') {
      const sel = spec.model_selector;
      if (typeof sel.capability === 'string') d.capability = sel.capability;
      if (typeof sel.class === 'string') d.class = sel.class;
    }
    if (typeof spec?.instructions === 'string') d.instructions = spec.instructions.trimEnd();
    d.tools = strings(spec?.tools);
    // `spec.mcp_servers` (PRD-006 MCX-201) — the allow-list of MCP connection
    // names this agent may draw tools from.
    d.mcpServers = strings(spec?.mcp_servers);
    if (spec?.memory && typeof spec.memory === 'object') {
      d.memoryEnabled = true;
      if (typeof spec.memory.namespace === 'string') d.namespace = spec.memory.namespace;
    }
    d.maxSteps = typeof spec?.max_steps === 'number' ? spec.max_steps : null;
    return d;
  }

  /** Serialize a draft to the k8s-style agent manifest the engine expects. */
  toManifest(d: AgentDraft): string {
    const spec: Record<string, unknown> = {};
    if (d.pinnedModel.trim()) {
      spec['model'] = d.pinnedModel.trim();
    } else {
      spec['model_selector'] = { capability: d.capability, class: d.class };
    }
    spec['instructions'] = (d.instructions.trimEnd() || 'You are a helpful assistant.') + '\n';
    const tools = d.tools.filter((t) => t.trim());
    if (tools.length) spec['tools'] = tools;
    const mcpServers = d.mcpServers.filter((t) => t.trim());
    if (mcpServers.length) spec['mcp_servers'] = mcpServers;
    if (d.maxSteps != null && d.maxSteps > 0) spec['max_steps'] = Math.floor(d.maxSteps);
    if (d.memoryEnabled) {
      spec['memory'] = {
        enabled: true,
        namespace: d.namespace.trim() || 'default',
        retrieval: { strategy: 'hybrid', limit: 4 },
      };
    }
    return YAML.stringify(
      {
        apiVersion: 'agent.apex.io/v1',
        kind: 'Agent',
        metadata: { name: d.name.trim() || 'untitled' },
        spec,
      },
      // Multi-line instructions render as a `|` block scalar, like the examples.
      { blockQuote: 'literal', lineWidth: 0 },
    );
  }

  /**
   * Run an inline-manifest agent, streaming normalized events. Parses the SSE wire
   * format manually: frames are separated by a blank line; `event:` names the terminal
   * `result`/`error` frame, while run events arrive as anonymous `data:` JSON carrying a
   * `type`. The returned Observable completes when the stream closes; unsubscribing
   * aborts the request.
   */
  runStream(manifest: string, message: string): Observable<StreamEvent> {
    return new Observable<StreamEvent>((sub) => {
      const ctrl = new AbortController();
      (async () => {
        try {
          const res = await fetch('/api/v1/agents:stream', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
            body: JSON.stringify({ manifest, input: { message } }),
            signal: ctrl.signal,
          });
          if (!res.ok || !res.body) {
            const detail = await res.text().catch(() => res.statusText);
            sub.next({ kind: 'error', message: `${res.status} — ${detail || 'request failed'}` });
            sub.complete();
            return;
          }
          const reader = res.body.getReader();
          const decoder = new TextDecoder();
          let buf = '';
          for (;;) {
            const { value, done } = await reader.read();
            if (done) break;
            buf += decoder.decode(value, { stream: true });
            let sep: number;
            while ((sep = buf.indexOf('\n\n')) >= 0) {
              const frame = buf.slice(0, sep);
              buf = buf.slice(sep + 2);
              const evt = this.parseFrame(frame);
              if (evt) sub.next(evt);
            }
          }
          sub.complete();
        } catch (e: unknown) {
          if (!ctrl.signal.aborted) {
            sub.next({ kind: 'error', message: e instanceof Error ? e.message : String(e) });
          }
          sub.complete();
        }
      })();
      return () => ctrl.abort();
    });
  }

  private parseFrame(frame: string): StreamEvent | null {
    let event = '';
    const dataLines: string[] = [];
    for (const line of frame.split('\n')) {
      if (line.startsWith('event:')) event = line.slice(6).trim();
      else if (line.startsWith('data:')) dataLines.push(line.slice(5).replace(/^ /, ''));
      // ignore comments (':') and other SSE fields
    }
    const data = dataLines.join('\n');
    if (!data && !event) return null;

    if (event === 'result') {
      try {
        const p = JSON.parse(data);
        return { kind: 'result', status: p.status, output: p.output, steps: p.steps };
      } catch {
        return { kind: 'result', status: 'succeeded' };
      }
    }
    if (event === 'error') return { kind: 'error', message: data || 'run failed' };

    try {
      const p = JSON.parse(data);
      switch (p.type) {
        case 'start':
          return { kind: 'start', model: p.model, provider: p.provider };
        case 'memory':
          return { kind: 'memory', source: p.source, score: p.score };
        case 'delta':
          return { kind: 'delta', text: p.text ?? '' };
        case 'reasoning':
          return { kind: 'reasoning', text: p.text ?? '' };
        case 'tool_call_delta':
          return {
            kind: 'tool_call_delta',
            index: p.index ?? 0,
            name: p.name ?? '',
            arguments: p.arguments ?? '',
          };
        case 'tool_call':
          return { kind: 'tool_call', name: p.name, arguments: p.arguments };
        case 'tool_result':
          return { kind: 'tool_result', name: p.name, ok: !!p.ok };
        case 'done':
          return { kind: 'done', usage: p.usage };
        default:
          return null;
      }
    } catch {
      return null;
    }
  }
}
