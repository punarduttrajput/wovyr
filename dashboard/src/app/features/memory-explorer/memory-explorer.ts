import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { MemoryService } from './memory.service';
import { MemoryNamespace, MemoryResult } from '../../core/api.types';

@Component({
  selector: 'app-memory-explorer',
  imports: [FormsModule],
  templateUrl: './memory-explorer.html',
  styleUrl: './memory-explorer.scss',
})
export class MemoryExplorer implements OnInit {
  private svc = inject(MemoryService);

  readonly namespaces = signal<MemoryNamespace[]>([]);
  readonly selectedNs = signal<string | null>(null);
  readonly results = signal<MemoryResult[]>([]);
  readonly mode = signal<'browse' | 'search'>('browse');
  readonly status = signal('');
  readonly showAdd = signal(false);
  readonly totalCount = computed(() => this.namespaces().reduce((a, n) => a + n.count, 0));

  q = '';
  strategy: 'hybrid' | 'vector' | 'keyword' = 'hybrid';
  weights = { relevance: 0.55, recency: 0.2, importance: 0.15 };
  diversity = 0;
  add = { namespace: '', content: '', tags: '', importance: 0.5 };

  ngOnInit(): void {
    this.refresh();
    this.browse();
  }

  refresh(): void {
    this.svc.namespaces().subscribe({
      next: (n) => this.namespaces.set(n),
      error: (e) => this.fail(e),
    });
  }

  select(ns: string | null): void {
    this.selectedNs.set(ns);
    if (this.mode() === 'search' && this.q.trim()) this.search();
    else this.browse();
  }

  browse(): void {
    this.mode.set('browse');
    this.svc.records(this.selectedNs() ?? undefined).subscribe({
      next: (r) => this.results.set(r),
      error: (e) => this.fail(e),
    });
  }

  search(): void {
    if (!this.q.trim()) {
      this.browse();
      return;
    }
    this.mode.set('search');
    this.svc
      .query({
        text: this.q.trim(),
        namespace: this.selectedNs() ?? undefined,
        strategy: this.strategy,
        limit: 10,
        diversity: this.diversity,
        relevance: this.weights.relevance,
        recency: this.weights.recency,
        importance: this.weights.importance,
      })
      .subscribe({ next: (r) => this.results.set(r), error: (e) => this.fail(e) });
  }

  clearSearch(): void {
    this.q = '';
    this.browse();
  }

  /** Toggle the add/seed panel, prefilling the namespace with the current selection
   *  (so seeding a brand-new knowledge base just means typing a new name). */
  toggleAdd(): void {
    if (this.showAdd()) {
      this.showAdd.set(false);
    } else {
      this.add.namespace = this.selectedNs() ?? '';
      this.showAdd.set(true);
    }
  }

  store(): void {
    const ns = this.add.namespace.trim();
    if (!ns || !this.add.content.trim()) {
      this.status.set('Enter a namespace and content.');
      return;
    }
    const tags = this.add.tags.split(',').map((t) => t.trim()).filter(Boolean);
    this.svc
      .put({ namespace: ns, content: this.add.content.trim(), importance: this.add.importance, tags })
      .subscribe({
        next: () => {
          this.status.set(`Stored in "${ns}"`);
          this.add.content = '';
          this.add.tags = '';
          this.showAdd.set(false);
          // Focus the (possibly new) namespace so the seeded record is visible.
          this.selectedNs.set(ns);
          this.refresh();
          this.browse();
        },
        error: (e) => this.fail(e),
      });
  }

  pct(v: number): string {
    return `${Math.round(Math.max(0, Math.min(1, v)) * 100)}%`;
  }
  scoreClass(s: number): string {
    return s >= 0.8 ? 'ok' : s >= 0.6 ? 'warn' : 'mut';
  }

  private fail(e: unknown): void {
    const err = e as { error?: { error?: { message?: string } }; message?: string };
    this.status.set('Error: ' + (err?.error?.error?.message ?? err?.message ?? 'request failed'));
  }
}
