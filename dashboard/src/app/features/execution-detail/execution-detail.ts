import { Component, OnInit, inject, input, signal } from '@angular/core';
import { JsonPipe } from '@angular/common';
import { RouterLink } from '@angular/router';
import { MonitoringService } from '../monitoring/monitoring.service';
import { EmptyState } from '../../shared/empty-state';
import { StatusPill } from '../../shared/status-pill';
import { WorkflowSummary } from '../../core/api.types';

/**
 * Read-only detail for a single workflow execution — its status, per-activity states,
 * and the durable event timeline. Backed by `GET /api/v1/workflows/{id}`. The `id`
 * input is bound from the route param (withComponentInputBinding).
 */
@Component({
  selector: 'app-execution-detail',
  imports: [JsonPipe, RouterLink, EmptyState, StatusPill],
  templateUrl: './execution-detail.html',
  styleUrl: './execution-detail.scss',
})
export class ExecutionDetail implements OnInit {
  private svc = inject(MonitoringService);

  readonly id = input('');
  readonly execution = signal<WorkflowSummary | null>(null);
  readonly events = signal<unknown[]>([]);
  readonly error = signal('');

  ngOnInit(): void {
    this.svc.execution(this.id()).subscribe({
      next: (r) => {
        this.execution.set(r.execution);
        this.events.set(r.events ?? []);
      },
      error: (e) => {
        const err = e as { status?: number; error?: { error?: { message?: string } } };
        this.error.set(
          err?.status === 404 ? `Execution "${this.id()}" not found.` : 'Failed to load execution.',
        );
      },
    });
  }

  activities(): { id: string; state: string }[] {
    const a = this.execution()?.activities ?? {};
    return Object.entries(a).map(([id, state]) => ({ id, state: String(state) }));
  }

}
