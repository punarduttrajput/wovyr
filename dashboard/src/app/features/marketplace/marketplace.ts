import { Component, OnInit, inject, signal } from '@angular/core';
import { MarketplaceService } from './marketplace.service';
import { PluginInfo } from '../../core/api.types';
import { ToastService } from '../../core/toast.service';

@Component({
  selector: 'app-marketplace',
  templateUrl: './marketplace.html',
  styleUrl: './marketplace.scss',
})
export class Marketplace implements OnInit {
  private svc = inject(MarketplaceService);
  private toast = inject(ToastService);

  readonly plugins = signal<PluginInfo[]>([]);
  readonly status = signal('');
  readonly busy = signal<string | null>(null);

  ngOnInit(): void {
    this.refresh();
  }

  refresh(): void {
    this.svc.list().subscribe({
      next: (p) => this.plugins.set(p),
      error: (e) => this.fail(e),
    });
  }

  toggle(p: PluginInfo): void {
    this.busy.set(p.id);
    const op = p.state === 'enabled' ? this.svc.disable(p.id) : this.svc.enable(p.id);
    op.subscribe({
      next: (r) => {
        this.toast.show(`${p.name} ${r.state}`);
        this.busy.set(null);
        this.refresh();
      },
      error: (e) => {
        this.busy.set(null);
        this.fail(e);
      },
    });
  }

  /** A capability/permission that touches the network or secrets reads as elevated risk. */
  permRisk(perm: string): 'high' | 'med' | 'low' {
    if (perm.includes('egress:*') || perm.includes(':*')) return 'high';
    if (perm.startsWith('net:') || perm.startsWith('secrets:')) return 'med';
    return 'low';
  }

  initials(name: string): string {
    return name.slice(0, 2).toLowerCase();
  }

  private fail(e: unknown): void {
    const err = e as { error?: { error?: { message?: string } }; message?: string };
    this.status.set('Error: ' + (err?.error?.error?.message ?? err?.message ?? 'request failed'));
  }
}
