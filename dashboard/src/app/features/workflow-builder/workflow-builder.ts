import {
  Component,
  ElementRef,
  HostListener,
  OnDestroy,
  OnInit,
  ViewChild,
  inject,
  signal,
} from '@angular/core';
import { JsonPipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { switchMap } from 'rxjs/operators';
import {
  WfActivity,
  WfActivityType,
  WorkflowDraft,
  WorkflowService,
  WorkflowValidation,
} from './workflow.service';
import { errText } from '../../core/http-error';
import { StatusPill } from '../../shared/status-pill';
import { ToolInfo, WorkflowSummary } from '../../core/api.types';

const EXAMPLE_YAML = `metadata:
  name: hello-workflow
  version: "1.0.0"
spec:
  activities:
    - id: greet
      type: function
      name: echo
      inputs:
        message: "Hello from the Workflow Builder!"
    - id: done
      type: function
      name: echo
      inputs:
        message: "Workflow complete."
  transitions:
    - from: greet
      to: done
`;

/** Node box width + port vertical offset, in px — used for edge geometry. */
const NODE_W = 190;
const PORT_Y = 30;

@Component({
  selector: 'app-workflow-builder',
  imports: [JsonPipe, FormsModule, RouterLink, StatusPill],
  templateUrl: './workflow-builder.html',
  styleUrl: './workflow-builder.scss',
})
export class WorkflowBuilder implements OnInit, OnDestroy {
  private svc = inject(WorkflowService);

  /** The untransformed viewport — the reference frame for screen→content math. */
  @ViewChild('viewport') viewportRef?: ElementRef<HTMLElement>;

  // Pan/zoom of the canvas content layer.
  readonly panX = signal(0);
  readonly panY = signal(0);
  readonly zoom = signal(1);

  // ── editor state ────────────────────────────────────────────────────────────
  /** 'design' = the visual node canvas; 'yaml' = raw editor (advanced). */
  readonly mode = signal<'design' | 'yaml'>('design');
  manifest = EXAMPLE_YAML;
  execInput = '{}';
  customExecId = '';

  /** The visual definition the canvas edits — starts as the hello-workflow example. */
  draft: WorkflowDraft = {
    name: 'hello-workflow',
    version: '1.0.0',
    activities: [
      { id: 'greet', type: 'function', name: 'echo', inputs: '{"message":"Hello!"}', x: 60, y: 60 },
      { id: 'done', type: 'function', name: 'echo', inputs: '{"message":"Done."}', x: 340, y: 180 },
    ],
    transitions: [{ from: 'greet', to: 'done', when: '' }],
  };

  readonly toolCatalog = signal<ToolInfo[]>([
    { id: 'echo', description: 'Echo the input back unchanged.', category: 'builtin', permissions: [] },
    { id: 'fs_read', description: 'Read the contents of a text file at a given path.', category: 'builtin', permissions: [] },
    { id: 'http_get', description: 'Fetch a URL and return its status and body.', category: 'builtin', permissions: [] },
    { id: 'shell', description: 'Run a shell command.', category: 'builtin', permissions: [] },
  ]);

  /** Stored agent ids (`GET /api/v1/agents`), for the `agent` node's picker. */
  readonly agentCatalog = signal<string[]>([]);

  readonly activityTypes: { value: WfActivityType; label: string }[] = [
    { value: 'function', label: 'Run a tool' },
    { value: 'ai', label: 'Ask the model (AI)' },
    { value: 'agent', label: 'Run an agent' },
    { value: 'human', label: 'Wait for human approval' },
    { value: 'wait', label: 'Wait for an event' },
  ];

  /** Selected node index (opens the config panel), or null. */
  readonly selected = signal<number | null>(null);

  // Canvas interaction state (plain fields — mutated during pointer drags).
  private dragIdx: number | null = null;
  private dragDX = 0;
  private dragDY = 0;
  // Panning the background.
  private panning = false;
  private panMoved = false;
  private panStartX = 0;
  private panStartY = 0;
  private panOrigX = 0;
  private panOrigY = 0;
  /** The activity id a connection is being dragged from (output port), or null. */
  connectFrom: string | null = null;
  /** Live cursor position (canvas coords) while connecting, for the temp wire. */
  readonly cursor = signal<{ x: number; y: number }>({ x: 0, y: 0 });

  // ── validation DAG ──────────────────────────────────────────────────────────
  readonly validation = signal<WorkflowValidation | null>(null);
  readonly validErr = signal('');

  // ── executions list ─────────────────────────────────────────────────────────
  readonly executions = signal<WorkflowSummary[]>([]);
  readonly selectedExec = signal<WorkflowSummary | null>(null);
  readonly events = signal<unknown[]>([]);

  // ── signal / approve form ───────────────────────────────────────────────────
  readonly showSignal = signal(false);
  signalEvent = '';
  signalPayload = '{}';
  approveActivity = '';
  approveDecision = '{"approved":true}';

  // ── shared UI state ─────────────────────────────────────────────────────────
  readonly status = signal('');
  readonly busy = signal(false);

  /** Names of workflows saved in this browser (localStorage). */
  readonly savedNames = signal<string[]>([]);
  private readonly WF_INDEX = 'wovyr.wf.index';
  private wfKey(name: string): string {
    return `wovyr.wf:${name}`;
  }

  private pollSub?: Subscription;

  ngOnInit(): void {
    this.loadList();
    this.loadSavedNames();
    // Catalog loads and the executions poll surface failures via the global
    // error-toast interceptor (UI-302) — the empty error handlers below only
    // stop the observable error from propagating, they don't swallow it.
    this.svc.tools().subscribe({
      next: (t) => {
        if (t.length) this.toolCatalog.set(t);
      },
      error: () => {},
    });
    this.svc.agents().subscribe({
      next: (a) => this.agentCatalog.set(a),
      error: () => {},
    });
    this.pollSub = interval(5000)
      .pipe(switchMap(() => this.svc.listAll()))
      .subscribe({ next: (e) => this.executions.set(e), error: () => {} });
  }

  ngOnDestroy(): void {
    this.pollSub?.unsubscribe();
  }

  // ── save / load (browser-local) ───────────────────────────────────────────────
  private loadSavedNames(): void {
    try {
      this.savedNames.set(JSON.parse(localStorage.getItem(this.WF_INDEX) ?? '[]'));
    } catch {
      this.savedNames.set([]);
    }
  }

  /** Persist the whole draft (steps, wiring, and canvas layout) under its name. */
  saveWorkflow(): void {
    const name = this.draft.name.trim();
    if (!name) {
      this.status.set('Name the workflow before saving.');
      return;
    }
    localStorage.setItem(this.wfKey(name), JSON.stringify(this.draft));
    const names = [...new Set([...this.savedNames(), name])].sort();
    localStorage.setItem(this.WF_INDEX, JSON.stringify(names));
    this.savedNames.set(names);
    this.status.set(`Saved "${name}"`);
  }

  /** Restore a saved workflow into the canvas. */
  loadWorkflow(name: string): void {
    const raw = localStorage.getItem(this.wfKey(name));
    if (!raw) return;
    try {
      this.draft = JSON.parse(raw) as WorkflowDraft;
      this.selected.set(null);
      this.resetView();
      this.mode.set('design');
      this.status.set(`Loaded "${name}"`);
    } catch {
      this.status.set('Error: saved workflow is corrupt');
    }
  }

  deleteWorkflow(name: string): void {
    localStorage.removeItem(this.wfKey(name));
    const names = this.savedNames().filter((n) => n !== name);
    localStorage.setItem(this.WF_INDEX, JSON.stringify(names));
    this.savedNames.set(names);
  }

  // ── manifest ────────────────────────────────────────────────────────────────
  currentManifest(): string {
    return this.mode() === 'design' ? this.svc.toWorkflowManifest(this.draft) : this.manifest;
  }

  editYaml(): void {
    this.manifest = this.currentManifest();
    this.mode.set('yaml');
  }

  // ── nodes ─────────────────────────────────────────────────────────────────────
  /**
   * Insert a step that is **runnable as inserted**, then wire it to the end of the
   * current chain.
   *
   * Both halves used to be missing: a new step arrived with `inputs: '{}'` and no
   * transition, so an `ai`/`agent` step had no prompt and no live inbound edge. The
   * engine marks such a step `Skipped`, `Validate` still reports `Valid ✓` (the DAG
   * is structurally legal), and the workflow completes green having never run it —
   * which made "no YAML required" true only for tool steps.
   *
   * The seeded inputs are deliberately valid-but-obvious placeholders: they run, and
   * they read as something to edit rather than as a finished step.
   */
  addNode(type: WfActivityType): void {
    const n = this.draft.activities.length + 1;
    const id = `step-${n}`;
    const defaultName =
      type === 'function'
        ? (this.toolCatalog()[0]?.id ?? 'echo')
        : type === 'agent'
          ? (this.agentCatalog()[0] ?? '')
          : type === 'ai'
            ? 'You are a helpful assistant. Answer concisely.'
            : type === 'wait'
              ? `${id}-signal`
              : '';
    // `${input.<field>}` references the run input, so these resolve against the
    // Input JSON box without further editing.
    const defaultInputs =
      type === 'ai' || type === 'agent'
        ? '{\n  "message": "${input.message}"\n}'
        : '{}';

    const previous = this.draft.activities.at(-1)?.id;
    this.draft.activities.push({
      id,
      type,
      name: defaultName,
      inputs: defaultInputs,
      x: 80 + ((n * 30) % 240),
      y: 60 + ((n * 40) % 200),
    });
    // Chain onto the end so the step is reachable. A user rewiring the graph just
    // deletes the edge — cheaper than discovering later that the step never ran.
    if (previous) this.draft.transitions.push({ from: previous, to: id, when: '' });
    this.selected.set(this.draft.activities.length - 1);
  }

  removeNode(i: number): void {
    const id = this.draft.activities[i]?.id;
    this.draft.activities.splice(i, 1);
    if (id) this.draft.transitions = this.draft.transitions.filter((t) => t.from !== id && t.to !== id);
    this.selected.set(null);
  }

  removeEdge(i: number): void {
    this.draft.transitions.splice(i, 1);
  }

  selectedNode(): WfActivity | null {
    const i = this.selected();
    return i === null ? null : (this.draft.activities[i] ?? null);
  }

  private act(id: string): WfActivity | undefined {
    return this.draft.activities.find((a) => a.id === id);
  }

  // ── pan / zoom ──────────────────────────────────────────────────────────────────
  /** The transform applied to the content layer. */
  transformStyle(): string {
    return `translate(${this.panX()}px, ${this.panY()}px) scale(${this.zoom()})`;
  }

  /** Start a background pan (or a click-to-deselect if it doesn't move). */
  startPan(e: PointerEvent): void {
    this.panning = true;
    this.panMoved = false;
    this.panStartX = e.clientX;
    this.panStartY = e.clientY;
    this.panOrigX = this.panX();
    this.panOrigY = this.panY();
  }

  /** Wheel-zoom toward the cursor (clamped). */
  onWheel(e: WheelEvent): void {
    e.preventDefault();
    const r = this.viewportRef?.nativeElement.getBoundingClientRect();
    const cx = e.clientX - (r?.left ?? 0);
    const cy = e.clientY - (r?.top ?? 0);
    const old = this.zoom();
    const next = Math.min(2, Math.max(0.4, old * (e.deltaY < 0 ? 1.1 : 1 / 1.1)));
    // Keep the content point under the cursor fixed while zooming.
    this.panX.set(cx - ((cx - this.panX()) / old) * next);
    this.panY.set(cy - ((cy - this.panY()) / old) * next);
    this.zoom.set(next);
  }

  zoomBy(factor: number): void {
    this.zoom.set(Math.min(2, Math.max(0.4, this.zoom() * factor)));
  }
  resetView(): void {
    this.panX.set(0);
    this.panY.set(0);
    this.zoom.set(1);
  }

  // ── canvas drag / connect ──────────────────────────────────────────────────────
  /** Convert a pointer event to canvas *content* coordinates (undo pan + zoom). */
  private toCanvas(e: PointerEvent): { x: number; y: number } {
    const r = this.viewportRef?.nativeElement.getBoundingClientRect();
    return {
      x: (e.clientX - (r?.left ?? 0) - this.panX()) / this.zoom(),
      y: (e.clientY - (r?.top ?? 0) - this.panY()) / this.zoom(),
    };
  }

  startNodeDrag(i: number, e: PointerEvent): void {
    this.selected.set(i);
    const a = this.draft.activities[i];
    const p = this.toCanvas(e);
    this.dragIdx = i;
    this.dragDX = p.x - a.x;
    this.dragDY = p.y - a.y;
    e.preventDefault();
  }

  startConnect(id: string, e: PointerEvent): void {
    this.connectFrom = id;
    this.cursor.set(this.toCanvas(e));
    e.preventDefault();
    e.stopPropagation();
  }

  endConnect(toId: string, e: PointerEvent): void {
    const from = this.connectFrom;
    this.connectFrom = null;
    if (from && from !== toId) {
      const dup = this.draft.transitions.some((t) => t.from === from && t.to === toId);
      if (!dup) this.draft.transitions.push({ from, to: toId, when: '' });
    }
    e.stopPropagation();
  }

  @HostListener('window:pointermove', ['$event'])
  onPointerMove(e: PointerEvent): void {
    if (this.dragIdx !== null) {
      const p = this.toCanvas(e);
      const a = this.draft.activities[this.dragIdx];
      a.x = Math.max(0, p.x - this.dragDX);
      a.y = Math.max(0, p.y - this.dragDY);
    } else if (this.connectFrom) {
      this.cursor.set(this.toCanvas(e));
    } else if (this.panning) {
      const dx = e.clientX - this.panStartX;
      const dy = e.clientY - this.panStartY;
      if (Math.abs(dx) > 3 || Math.abs(dy) > 3) this.panMoved = true;
      this.panX.set(this.panOrigX + dx);
      this.panY.set(this.panOrigY + dy);
    }
  }

  @HostListener('window:pointerup')
  onPointerUp(): void {
    // A background press that didn't pan is a click on empty canvas → deselect.
    if (this.panning && !this.panMoved) this.selected.set(null);
    this.dragIdx = null;
    this.connectFrom = null;
    this.panning = false;
  }

  // ── edge geometry (SVG bezier paths) ───────────────────────────────────────────
  private outPort(a: WfActivity): { x: number; y: number } {
    return { x: a.x + NODE_W, y: a.y + PORT_Y };
  }
  private inPort(a: WfActivity): { x: number; y: number } {
    return { x: a.x, y: a.y + PORT_Y };
  }
  private bezier(x1: number, y1: number, x2: number, y2: number): string {
    const dx = Math.max(40, Math.abs(x2 - x1) / 2);
    return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
  }
  edgePath(fromId: string, toId: string): string {
    const f = this.act(fromId);
    const t = this.act(toId);
    if (!f || !t) return '';
    const o = this.outPort(f);
    const i = this.inPort(t);
    return this.bezier(o.x, o.y, i.x, i.y);
  }
  tempPath(): string {
    if (!this.connectFrom) return '';
    const f = this.act(this.connectFrom);
    if (!f) return '';
    const o = this.outPort(f);
    const c = this.cursor();
    return this.bezier(o.x, o.y, c.x, c.y);
  }

  // ── config panel helpers ────────────────────────────────────────────────────────
  hasInputs(a: WfActivity): boolean {
    return a.type === 'function' || a.type === 'ai' || a.type === 'agent';
  }
  typeLabel(t: WfActivityType): string {
    return this.activityTypes.find((x) => x.value === t)?.label ?? t;
  }

  // ── validate / submit ────────────────────────────────────────────────────────────
  validate(): void {
    this.validErr.set('');
    this.validation.set(null);
    this.svc.validate(this.currentManifest()).subscribe({
      next: (v) => this.validation.set(v),
      error: (e) => this.validErr.set(errText(e)),
    });
  }

  submit(): void {
    let input: Record<string, unknown> = {};
    try {
      input = JSON.parse(this.execInput || '{}');
    } catch {
      this.status.set('Error: input is not valid JSON');
      return;
    }
    this.busy.set(true);
    this.status.set('Submitting…');
    this.svc.submit(this.currentManifest(), input, this.customExecId.trim() || undefined).subscribe({
      next: (r) => {
        this.status.set(`Submitted · ${r.execution_id}`);
        this.busy.set(false);
        this.loadList();
        this.customExecId = '';
      },
      error: (e) => {
        this.status.set('Error: ' + errText(e));
        this.busy.set(false);
      },
    });
  }

  // ── list & select ────────────────────────────────────────────────────────────────
  loadList(): void {
    this.svc.listAll().subscribe({ next: (e) => this.executions.set(e), error: () => {} });
  }

  select(ex: WorkflowSummary): void {
    this.selectedExec.set(ex);
    this.events.set([]);
    this.showSignal.set(false);
    this.svc.execution(ex.execution_id).subscribe({
      next: (r) => {
        this.selectedExec.set(r.execution);
        this.events.set(r.events ?? []);
      },
      error: () => {},
    });
  }

  refresh(): void {
    const ex = this.selectedExec();
    if (ex) this.select(ex);
    this.loadList();
  }

  // ── signal / approve ───────────────────────────────────────────────────────────
  sendSignal(): void {
    const ex = this.selectedExec();
    if (!ex || !this.signalEvent.trim()) return;
    let payload: unknown = {};
    try {
      payload = JSON.parse(this.signalPayload || '{}');
    } catch {
      /* ignore */
    }
    this.busy.set(true);
    this.svc.signal(ex.execution_id, this.currentManifest(), this.signalEvent.trim(), payload).subscribe({
      next: () => {
        this.status.set(`Signal "${this.signalEvent}" sent`);
        this.busy.set(false);
        this.signalEvent = '';
        setTimeout(() => this.refresh(), 800);
      },
      error: (e) => {
        this.status.set('Error: ' + errText(e));
        this.busy.set(false);
      },
    });
  }

  approve(): void {
    const ex = this.selectedExec();
    if (!ex || !this.approveActivity.trim()) return;
    let decision: unknown = { approved: true };
    try {
      decision = JSON.parse(this.approveDecision || '{"approved":true}');
    } catch {
      /* ignore */
    }
    this.busy.set(true);
    this.svc
      .approve(ex.execution_id, this.currentManifest(), this.approveActivity.trim(), decision)
      .subscribe({
        next: () => {
          this.status.set(`Approved "${this.approveActivity}"`);
          this.busy.set(false);
          this.approveActivity = '';
          setTimeout(() => this.refresh(), 800);
        },
        error: (e) => {
          this.status.set('Error: ' + errText(e));
          this.busy.set(false);
        },
      });
  }

  // ── helpers ────────────────────────────────────────────────────────────────────
  isWaiting(ex: WorkflowSummary): boolean {
    // Case-insensitive: the engine serializes snake_case (`waiting`, API-702).
    const s = ex.status.toLowerCase();
    return s === 'waiting' || s === 'running';
  }

  activities(): { id: string; state: string }[] {
    const a = this.selectedExec()?.activities ?? {};
    return Object.entries(a).map(([id, state]) => ({ id, state: String(state) }));
  }

}
