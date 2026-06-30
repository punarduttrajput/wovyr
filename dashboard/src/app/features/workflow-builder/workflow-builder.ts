import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { JsonPipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { switchMap } from 'rxjs/operators';
import { WorkflowService, WorkflowValidation } from './workflow.service';
import { WorkflowSummary } from '../../core/api.types';

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

@Component({
  selector: 'app-workflow-builder',
  imports: [JsonPipe, FormsModule, RouterLink],
  templateUrl: './workflow-builder.html',
  styleUrl: './workflow-builder.scss',
})
export class WorkflowBuilder implements OnInit, OnDestroy {
  private svc = inject(WorkflowService);

  // ── editor state ────────────────────────────────────────────────────────────
  manifest = EXAMPLE_YAML;
  execInput = '{}';
  customExecId = '';

  // ── validation DAG ──────────────────────────────────────────────────────────
  readonly validation = signal<WorkflowValidation | null>(null);
  readonly validErr = signal('');

  // ── executions list ─────────────────────────────────────────────────────────
  readonly executions = signal<WorkflowSummary[]>([]);
  readonly selected = signal<WorkflowSummary | null>(null);
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

  private pollSub?: Subscription;

  ngOnInit(): void {
    this.loadList();
    // Poll the list every 5 s so newly-submitted executions appear.
    this.pollSub = interval(5000)
      .pipe(switchMap(() => this.svc.listAll()))
      .subscribe({
        next: (e) => this.executions.set(e),
        error: () => {},
      });
  }

  ngOnDestroy(): void {
    this.pollSub?.unsubscribe();
  }

  // ── validate ─────────────────────────────────────────────────────────────────
  validate(): void {
    this.validErr.set('');
    this.validation.set(null);
    this.svc.validate(this.manifest).subscribe({
      next: (v) => this.validation.set(v),
      error: (e) => this.validErr.set(this.errText(e)),
    });
  }

  // ── submit ───────────────────────────────────────────────────────────────────
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
    this.svc
      .submit(this.manifest, input, this.customExecId.trim() || undefined)
      .subscribe({
        next: (r) => {
          this.status.set(`Submitted · ${r.execution_id}`);
          this.busy.set(false);
          this.loadList();
          this.customExecId = '';
        },
        error: (e) => {
          this.status.set('Error: ' + this.errText(e));
          this.busy.set(false);
        },
      });
  }

  // ── list & select ────────────────────────────────────────────────────────────
  loadList(): void {
    this.svc.listAll().subscribe({
      next: (e) => this.executions.set(e),
      error: () => {},
    });
  }

  select(ex: WorkflowSummary): void {
    this.selected.set(ex);
    this.events.set([]);
    this.showSignal.set(false);
    this.svc.execution(ex.execution_id).subscribe({
      next: (r) => {
        this.selected.set(r.execution);
        this.events.set(r.events ?? []);
      },
      error: () => {},
    });
  }

  refresh(): void {
    const ex = this.selected();
    if (ex) this.select(ex);
    this.loadList();
  }

  // ── signal ────────────────────────────────────────────────────────────────────
  sendSignal(): void {
    const ex = this.selected();
    if (!ex || !this.signalEvent.trim()) return;
    let payload: unknown = {};
    try { payload = JSON.parse(this.signalPayload || '{}'); } catch { /* ignore */ }
    this.busy.set(true);
    this.svc.signal(ex.execution_id, this.manifest, this.signalEvent.trim(), payload).subscribe({
      next: () => {
        this.status.set(`Signal "${this.signalEvent}" sent`);
        this.busy.set(false);
        this.signalEvent = '';
        setTimeout(() => this.refresh(), 800);
      },
      error: (e) => {
        this.status.set('Error: ' + this.errText(e));
        this.busy.set(false);
      },
    });
  }

  // ── approve ───────────────────────────────────────────────────────────────────
  approve(): void {
    const ex = this.selected();
    if (!ex || !this.approveActivity.trim()) return;
    let decision: unknown = { approved: true };
    try { decision = JSON.parse(this.approveDecision || '{"approved":true}'); } catch { /* ignore */ }
    this.busy.set(true);
    this.svc.approve(ex.execution_id, this.manifest, this.approveActivity.trim(), decision).subscribe({
      next: () => {
        this.status.set(`Approved "${this.approveActivity}"`);
        this.busy.set(false);
        this.approveActivity = '';
        setTimeout(() => this.refresh(), 800);
      },
      error: (e) => {
        this.status.set('Error: ' + this.errText(e));
        this.busy.set(false);
      },
    });
  }

  // ── helpers ───────────────────────────────────────────────────────────────────
  statusClass(s: string): string {
    switch (s) {
      case 'Completed': return 'ok';
      case 'Failed': return 'crit';
      case 'Compensating': return 'warn';
      case 'Running': case 'Waiting': case 'Resumed': case 'Scheduled': return 'info';
      default: return 'mut';
    }
  }

  isWaiting(ex: WorkflowSummary): boolean {
    return ex.status === 'Waiting' || ex.status === 'Running';
  }

  activities(): { id: string; state: string }[] {
    const a = this.selected()?.activities ?? {};
    return Object.entries(a).map(([id, state]) => ({ id, state: String(state) }));
  }

  private errText(e: unknown): string {
    const err = e as { error?: { error?: { message?: string } }; message?: string };
    return err?.error?.error?.message ?? err?.message ?? 'request failed';
  }
}
