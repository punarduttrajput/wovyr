import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';
import { map } from 'rxjs/operators';
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

  /** The registered tool catalog (built-ins + enabled plugin tools), for the picker. */
  tools(): Observable<ToolInfo[]> {
    return this.http
      .get<{ tools: ToolInfo[] }>('/api/v1/tools')
      .pipe(map((r) => r.tools ?? []));
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
      memoryEnabled: false,
      namespace: 'product-kb',
      maxSteps: null,
    };
  }

  /**
   * Parse a manifest back into an [`AgentDraft`] — the inverse of [`toManifest`]. It reads
   * the fixed shape this studio emits (and the same shape the example agents use): name,
   * `model` or `model_selector`, the `instructions: |` block, `tools: [...]`, `max_steps`,
   * and a `memory` block. Unknown/extra fields are ignored.
   */
  fromManifest(yaml: string): AgentDraft {
    const d = this.blankDraft();
    const lines = yaml.split('\n');
    const firstMatch = (re: RegExp): string | undefined =>
      lines.find((l) => re.test(l))?.match(re)?.[1]?.trim();

    d.name = firstMatch(/^\s{2}name:\s*(.+)$/) ?? '';
    const pinned = firstMatch(/^\s{2}model:\s*(.+)$/);
    if (pinned) d.pinnedModel = pinned;
    const selector = lines.find((l) => /^\s{2}model_selector:/.test(l));
    if (selector) {
      d.capability = selector.match(/capability:\s*([\w-]+)/)?.[1] ?? d.capability;
      d.class = selector.match(/class:\s*([\w-]+)/)?.[1] ?? d.class;
    }

    const toolsLine = lines.find((l) => /^\s{2}tools:\s*\[/.test(l));
    if (toolsLine) {
      const inner = toolsLine.slice(toolsLine.indexOf('[') + 1, toolsLine.lastIndexOf(']'));
      d.tools = inner
        .split(',')
        .map((t) => t.trim())
        .filter(Boolean);
    }

    if (lines.some((l) => /^\s{2}memory:/.test(l))) {
      d.memoryEnabled = true;
      d.namespace = firstMatch(/^\s{4}namespace:\s*(.+)$/) ?? d.namespace;
    }

    const maxSteps = firstMatch(/^\s{2}max_steps:\s*(\d+)$/);
    d.maxSteps = maxSteps ? Number(maxSteps) : null;

    // The `instructions: |` block scalar — collect the 4-space-indented body lines.
    const instrIdx = lines.findIndex((l) => /^\s{2}instructions:\s*\|/.test(l));
    if (instrIdx >= 0) {
      const body: string[] = [];
      for (let i = instrIdx + 1; i < lines.length; i++) {
        const l = lines[i];
        if (l.trim() === '') {
          body.push('');
        } else if (/^\s{4}/.test(l)) {
          body.push(l.slice(4));
        } else {
          break; // dedent → block ended
        }
      }
      d.instructions = body.join('\n').trimEnd();
    }
    return d;
  }

  /** Serialize a draft to the k8s-style agent manifest the engine expects. */
  toManifest(d: AgentDraft): string {
    const indent = (s: string, n: number) =>
      s
        .split('\n')
        .map((l) => ' '.repeat(n) + l)
        .join('\n');
    const model = d.pinnedModel.trim()
      ? `  model: ${d.pinnedModel.trim()}`
      : `  model_selector: { capability: ${d.capability}, class: ${d.class} }`;
    const lines = [
      'apiVersion: agent.apex.io/v1',
      'kind: Agent',
      'metadata:',
      `  name: ${d.name.trim() || 'untitled'}`,
      'spec:',
      model,
      '  instructions: |',
      indent(d.instructions.trimEnd() || 'You are a helpful assistant.', 4),
    ];
    const tools = d.tools.filter((t) => t.trim());
    if (tools.length) lines.push(`  tools: [${tools.join(', ')}]`);
    if (d.maxSteps != null && d.maxSteps > 0) lines.push(`  max_steps: ${Math.floor(d.maxSteps)}`);
    if (d.memoryEnabled) {
      lines.push(
        '  memory:',
        '    enabled: true',
        `    namespace: ${d.namespace.trim() || 'default'}`,
        '    retrieval: { strategy: hybrid, limit: 4 }',
      );
    }
    return lines.join('\n') + '\n';
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
