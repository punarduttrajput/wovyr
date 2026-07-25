import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { MarketplaceService } from './marketplace.service';
import { errText } from '../../core/http-error';
import { MarketplaceListing, PluginAttestation, PluginInfo } from '../../core/api.types';
import { ToastService } from '../../core/toast.service';
import { ConfirmService } from '../../shared/confirm';
import { Tabs, TabSpec } from '../../shared/tabs';

/** Capability-kind filter options for browse (mirrors `wovyr_plugin::CapabilityKind`). */
const CAPABILITY_KINDS = [
  'tool',
  'provider',
  'memory_backend',
  'policy',
  'workflow_activity',
] as const;

@Component({
  selector: 'app-marketplace',
  imports: [FormsModule, Tabs],
  templateUrl: './marketplace.html',
  styleUrl: './marketplace.scss',
})
export class Marketplace implements OnInit {
  private svc = inject(MarketplaceService);
  private toast = inject(ToastService);
  private confirm = inject(ConfirmService);

  readonly capabilityKinds = CAPABILITY_KINDS;

  readonly plugins = signal<PluginInfo[]>([]);
  readonly status = signal('');
  readonly busy = signal<string | null>(null);

  // ── tabs ─────────────────────────────────────────────────────────────────────
  readonly tab = signal<'browse' | 'installed'>('browse');
  readonly tabSpecs = computed<TabSpec[]>(() => [
    { id: 'browse', label: 'Browse' },
    { id: 'installed', label: 'Installed', count: this.plugins().length },
  ]);

  // ── browse / discovery ─────────────────────────────────────────────────────────
  readonly listings = signal<MarketplaceListing[]>([]);
  readonly searched = signal(false);
  searchText = '';
  searchCategory = '';
  searchCapability = '';

  /** Qualified ids of installed plugins, for the "installed" badge on browse cards. */
  readonly installedIds = computed(() => new Set(this.plugins().map((p) => p.id)));

  // ── install-from-marketplace panel ─────────────────────────────────────────────
  readonly installTarget = signal<MarketplaceListing | null>(null);
  browseGrants = '';
  browseVersion = '';

  /** Supply-chain attestation for the selected install target/version. */
  readonly attestation = signal<PluginAttestation | null>(null);
  readonly attestationBusy = signal(false);

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
    this.search();
  }

  setTab(t: string): void {
    this.tab.set(t as 'browse' | 'installed');
    if (t === 'browse' && !this.searched()) this.search();
    if (t === 'installed') this.refresh();
  }

  refresh(): void {
    this.svc.list().subscribe({
      next: (p) => this.plugins.set(p),
      error: (e) => this.fail(e),
    });
  }

  // ── browse ─────────────────────────────────────────────────────────────────────

  search(): void {
    this.svc.searchListings(this.searchText, this.searchCategory, this.searchCapability).subscribe({
      next: (ls) => {
        this.listings.set(ls);
        this.searched.set(true);
      },
      error: (e) => this.fail(e),
    });
  }

  clearFilters(): void {
    this.searchText = '';
    this.searchCategory = '';
    this.searchCapability = '';
    this.search();
  }

  /** Open the install panel for a listing, pre-filling grants with its declared perms. */
  openInstall(l: MarketplaceListing): void {
    this.installTarget.set(l);
    this.browseGrants = l.permissions.join(', ');
    this.browseVersion = l.versions[0] ?? '';
    this.loadAttestation();
  }

  /** Close the install panel and clear the loaded attestation. */
  closeInstall(): void {
    this.installTarget.set(null);
    this.attestation.set(null);
  }

  /** Fetch the attestation for the current install target + selected version. */
  loadAttestation(): void {
    const l = this.installTarget();
    if (!l) return;
    this.attestation.set(null);
    this.attestationBusy.set(true);
    this.svc.attestation(l.id, this.browseVersion).subscribe({
      next: (a) => {
        this.attestation.set(a);
        this.attestationBusy.set(false);
      },
      // A missing/unreadable attestation is non-fatal — install stays available.
      error: () => this.attestationBusy.set(false),
    });
  }

  confirmInstall(): void {
    const l = this.installTarget();
    if (!l) return;
    const grants = this.browseGrants
      .split(',')
      .map((g) => g.trim())
      .filter(Boolean);
    this.busy.set(l.id);
    this.svc.installFromMarketplace(l.id, this.browseVersion || undefined, grants).subscribe({
      next: () => {
        this.toast.show(`${l.name} installed (disabled)`);
        this.busy.set(null);
        this.installTarget.set(null);
        this.attestation.set(null);
        this.refresh(); // updated installed set → browse cards reflect it
        this.search(); // refreshed install counts
      },
      error: (e) => {
        this.busy.set(null);
        this.fail(e);
      },
    });
  }

  isInstalled(id: string): boolean {
    return this.installedIds().has(id);
  }

  /** Filled/empty star glyphs for a 0–5 rating (rounded to nearest whole star). */
  stars(rating: number | null): string {
    if (rating == null) return '';
    const filled = Math.round(rating);
    return '★'.repeat(filled) + '☆'.repeat(Math.max(0, 5 - filled));
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

  async uninstall(p: PluginInfo): Promise<void> {
    const ok = await this.confirm.ask({
      title: 'Uninstall plugin',
      message: `Uninstall ${p.name}? Its capabilities are withdrawn immediately.`,
      confirmLabel: 'Uninstall',
      danger: true,
    });
    if (!ok) return;
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
      this.status.set('Error: select an .wovyrpkg file first');
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
    this.status.set('Error: ' + errText(e));
  }
}
