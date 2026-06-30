import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { MarketplaceService } from './marketplace.service';
import { PluginInfo } from '../../core/api.types';
import { ToastService } from '../../core/toast.service';

@Component({
  selector: 'app-marketplace',
  imports: [FormsModule],
  templateUrl: './marketplace.html',
  styleUrl: './marketplace.scss',
})
export class Marketplace implements OnInit {
  private svc = inject(MarketplaceService);
  private toast = inject(ToastService);

  readonly plugins = signal<PluginInfo[]>([]);
  readonly status = signal('');
  readonly busy = signal<string | null>(null);

  // ── install panel ────────────────────────────────────────────────────────────
  readonly showInstall = signal(false);
  installGrants = '';
  private pendingApkgBase64 = '';
  installFileName = '';

  // ── trust panel ──────────────────────────────────────────────────────────────
  readonly showTrust = signal(false);
  trustPublisher = '';
  trustKeyHex = '';

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

  uninstall(p: PluginInfo): void {
    if (!confirm(`Uninstall ${p.name}?`)) return;
    this.busy.set(p.id);
    this.svc.uninstall(p.id).subscribe({
      next: () => {
        this.toast.show(`${p.name} uninstalled`);
        this.busy.set(null);
        this.refresh();
      },
      error: (e) => {
        this.busy.set(null);
        this.fail(e);
      },
    });
  }

  onFileSelected(event: Event): void {
    const input = event.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    this.installFileName = file.name;
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      // result is "data:<mime>;base64,<data>" — strip the prefix.
      const comma = result.indexOf(',');
      this.pendingApkgBase64 = comma >= 0 ? result.slice(comma + 1) : result;
    };
    reader.readAsDataURL(file);
  }

  installPlugin(): void {
    if (!this.pendingApkgBase64) {
      this.status.set('Error: select an .apexpkg file first');
      return;
    }
    const grants = this.installGrants
      .split(',')
      .map((g) => g.trim())
      .filter(Boolean);
    this.busy.set('install');
    this.svc.install(this.pendingApkgBase64, grants).subscribe({
      next: (p) => {
        const name = (p as unknown as { name?: string }).name ?? 'plugin';
        this.toast.show(`${name} installed (disabled)`);
        this.busy.set(null);
        this.showInstall.set(false);
        this.pendingApkgBase64 = '';
        this.installFileName = '';
        this.installGrants = '';
        this.refresh();
      },
      error: (e) => {
        this.busy.set(null);
        this.fail(e);
      },
    });
  }

  addTrust(): void {
    if (!this.trustPublisher.trim() || !this.trustKeyHex.trim()) {
      this.status.set('Error: publisher and public key are required');
      return;
    }
    this.busy.set('trust');
    this.svc.trustPublisher(this.trustPublisher.trim(), this.trustKeyHex.trim()).subscribe({
      next: (r) => {
        this.toast.show(`Trusted publisher ${r.publisher}`);
        this.busy.set(null);
        this.showTrust.set(false);
        this.trustPublisher = '';
        this.trustKeyHex = '';
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
