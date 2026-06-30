import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { SettingsService } from './settings.service';
import { Membership, Organization, Project, QuotaLimits, Role, Webhook } from '../../core/api.types';
import { PRINCIPAL, TENANT } from '../../core/tenant.config';

type Tab = 'projects' | 'members' | 'quotas' | 'integrations';

@Component({
  selector: 'app-settings',
  imports: [FormsModule],
  templateUrl: './settings.html',
  styleUrl: './settings.scss',
})
export class Settings implements OnInit {
  private svc = inject(SettingsService);

  readonly tenant = TENANT;
  readonly principal = PRINCIPAL;
  readonly roles: Role[] = ['viewer', 'editor', 'project_admin', 'org_admin', 'platform_admin'];

  readonly tab = signal<Tab>('projects');
  readonly orgs = signal<Organization[]>([]);
  readonly projects = signal<Project[]>([]);
  readonly members = signal<Membership[]>([]);
  readonly webhooks = signal<Webhook[]>([]);
  readonly selected = signal<Project | null>(null);
  readonly status = signal('');
  readonly forbidden = signal(false);

  quota: QuotaLimits = {};
  newOrg = '';
  newProject = { name: '', organization: '' };
  newMember: { user: string; role: Role } = { user: '', role: 'editor' };
  newWebhook = { url: '', events: 'agent.run.*, project.*', secret: '' };

  ngOnInit(): void {
    this.loadOrgs();
    this.loadProjects();
    this.loadWebhooks();
  }

  setTab(t: Tab): void {
    this.tab.set(t);
  }

  // --- loads ---
  loadOrgs(): void {
    this.svc.listOrgs().subscribe({
      next: (o) => {
        this.orgs.set(o);
        if (!this.newProject.organization && o[0]) this.newProject.organization = o[0].id;
      },
      error: (e) => this.fail(e),
    });
  }
  loadProjects(): void {
    this.svc.listProjects().subscribe({ next: (p) => this.projects.set(p), error: (e) => this.fail(e) });
  }
  loadWebhooks(): void {
    this.svc.listWebhooks().subscribe({ next: (w) => this.webhooks.set(w), error: (e) => this.fail(e) });
  }

  select(p: Project): void {
    this.selected.set(p);
    this.svc.listMembers(p.id).subscribe({ next: (m) => this.members.set(m), error: (e) => this.fail(e) });
    this.svc.getQuota(p.id).subscribe({ next: (q) => (this.quota = q), error: (e) => this.fail(e) });
  }

  // --- mutations ---
  createOrg(): void {
    if (!this.newOrg.trim()) return;
    this.svc.createOrg(this.newOrg.trim()).subscribe({
      next: () => { this.ok('Organization created'); this.newOrg = ''; this.loadOrgs(); },
      error: (e) => this.fail(e),
    });
  }
  createProject(): void {
    if (!this.newProject.name.trim() || !this.newProject.organization) return;
    this.svc.createProject(this.newProject.name.trim(), this.newProject.organization).subscribe({
      next: () => { this.ok('Project created'); this.newProject.name = ''; this.loadProjects(); },
      error: (e) => this.fail(e),
    });
  }
  removeProject(p: Project): void {
    this.svc.deleteProject(p.id).subscribe({
      next: () => { this.ok('Project deleted'); if (this.selected()?.id === p.id) this.selected.set(null); this.loadProjects(); },
      error: (e) => this.fail(e),
    });
  }
  addMember(): void {
    const p = this.selected();
    if (!p || !this.newMember.user.trim()) return;
    this.svc.addMember(p.id, this.newMember.user.trim(), this.newMember.role).subscribe({
      next: () => { this.ok('Member added'); this.newMember.user = ''; this.select(p); },
      error: (e) => this.fail(e),
    });
  }
  removeMember(m: Membership): void {
    const p = this.selected();
    if (!p) return;
    this.svc.removeMember(p.id, m.user).subscribe({
      next: () => { this.ok('Member removed'); this.select(p); },
      error: (e) => this.fail(e),
    });
  }
  saveQuota(): void {
    const p = this.selected();
    if (!p) return;
    this.svc.setQuota(p.id, this.clean(this.quota)).subscribe({
      next: (q) => { this.quota = q; this.ok('Quota saved'); },
      error: (e) => this.fail(e),
    });
  }
  createWebhook(): void {
    const events = this.newWebhook.events.split(',').map((s) => s.trim()).filter(Boolean);
    if (!this.newWebhook.url.trim() || !events.length || !this.newWebhook.secret.trim()) return;
    this.svc.createWebhook(this.newWebhook.url.trim(), events, this.newWebhook.secret.trim()).subscribe({
      next: () => { this.ok('Webhook registered'); this.newWebhook.url = ''; this.newWebhook.secret = ''; this.loadWebhooks(); },
      error: (e) => this.fail(e),
    });
  }
  removeWebhook(w: Webhook): void {
    this.svc.deleteWebhook(w.id).subscribe({ next: () => { this.ok('Webhook deleted'); this.loadWebhooks(); }, error: (e) => this.fail(e) });
  }

  roleLabel(r: Role): string {
    return r.replace('_', '.');
  }

  private clean(q: QuotaLimits): QuotaLimits {
    const out: QuotaLimits = {};
    for (const k of Object.keys(q) as (keyof QuotaLimits)[]) {
      const v = q[k];
      if (v !== null && v !== undefined && `${v}` !== '') out[k] = Number(v);
    }
    return out;
  }
  private ok(msg: string): void {
    this.forbidden.set(false);
    this.status.set(msg);
  }
  private fail(e: unknown): void {
    const err = e as { status?: number; error?: { error?: { message?: string } }; message?: string };
    if (err?.status === 403) this.forbidden.set(true);
    this.status.set('Error: ' + (err?.error?.error?.message ?? err?.message ?? 'request failed'));
  }
}
