import { Component, HostListener, computed, inject, signal } from '@angular/core';
import { Router, NavigationEnd, RouterOutlet, RouterLink, RouterLinkActive } from '@angular/router';
import { filter } from 'rxjs/operators';
import { Session } from './core/session';
import { ThemeService } from './core/theme.service';
import { ToastService } from './core/toast.service';
import { CommandPalette } from './shared/command-palette';
import { ConfirmDialog } from './shared/confirm';

interface NavItem {
  path: string;
  label: string;
  /** Symbol id in the shared `icons.svg` sprite (UI-306). */
  icon: string;
  /** Live count rendered as a badge; absent = no badge. Fake hardcoded badge
   * strings were removed in UI-306 — a badge must mean something real. */
  badge?: string;
}

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, CommandPalette, ConfirmDialog],
  templateUrl: './app.html',
  styleUrl: './app.scss',
})
export class App {
  readonly theme = inject(ThemeService);
  readonly toasts = inject(ToastService);
  /** DASH-401/402: the shell's identity block and auth-mode indicator bind to
   * this — the actual tenant/principal/credential every API call carries
   * (see tenant.interceptor.ts) — instead of the hardcoded "Punar R." /
   * "org.admin" strings that used to be unrelated to the real session. */
  readonly session = inject(Session);
  private router = inject(Router);

  readonly crumb = signal<{ root: string; leaf: string }>({ root: 'Build', leaf: 'Agent Studio' });

  /** Avatar initials derived from the real principal (e.g. "admin@wovyr.local"
   * → "AW"), replacing the hardcoded "PR". Not a role/name lookup — the shell
   * has no membership/role data without an extra API call, so it derives only
   * from what's already known client-side. */
  readonly initials = computed(() => {
    const local = this.session.principal().split('@')[0] || this.session.principal();
    const words = local.split(/[._-]+/).filter(Boolean);
    if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
    return local.slice(0, 2).toUpperCase() || '··';
  });
  /** Mobile-only nav drawer state (UI-304); the rail is always visible on desktop. */
  readonly navOpen = signal(false);
  private readonly labels: Record<string, { root: string; leaf: string }> = {
    agents: { root: 'Build', leaf: 'Agent Studio' },
    playground: { root: 'Build', leaf: 'Playground' },
    monitoring: { root: 'Operate', leaf: 'Monitoring' },
    workflows: { root: 'Build', leaf: 'Workflow Builder' },
    memory: { root: 'Build', leaf: 'Memory Explorer' },
    surfaces: { root: 'Build', leaf: 'Surfaces' },
    'mcp-servers': { root: 'Build', leaf: 'MCP Servers' },
    marketplace: { root: 'Extend', leaf: 'Marketplace' },
    settings: { root: 'Administer', leaf: 'Settings' },
    executions: { root: 'Operate', leaf: 'Execution' },
    audit: { root: 'Operate', leaf: 'Audit log' },
    login: { root: 'Account', leaf: 'Sign in' },
  };

  constructor() {
    this.router.events
      .pipe(filter((e): e is NavigationEnd => e instanceof NavigationEnd))
      .subscribe((e) => {
        const seg = e.urlAfterRedirects.split('?')[0].split('/')[1] || 'agents';
        this.crumb.set(this.labels[seg] ?? { root: '', leaf: seg });
        this.navOpen.set(false); // navigating closes the mobile drawer
      });
  }

  @HostListener('document:keydown.escape')
  closeNav(): void {
    this.navOpen.set(false);
  }

  readonly operate: NavItem[] = [
    { path: '/monitoring', label: 'Monitoring', icon: 'i-monitoring' },
    { path: '/audit', label: 'Audit log', icon: 'i-audit' },
  ];

  readonly build: NavItem[] = [
    { path: '/workflows', label: 'Workflow Builder', icon: 'i-workflows' },
    { path: '/agents', label: 'Agent Studio', icon: 'i-agents' },
    { path: '/playground', label: 'Playground', icon: 'i-playground' },
    { path: '/memory', label: 'Memory Explorer', icon: 'i-memory' },
    { path: '/surfaces', label: 'Surfaces', icon: 'i-surfaces' },
    { path: '/mcp-servers', label: 'MCP Servers', icon: 'i-mcp' },
  ];

  readonly extend: NavItem[] = [
    { path: '/marketplace', label: 'Marketplace', icon: 'i-marketplace' },
    { path: '/settings', label: 'Settings', icon: 'i-settings' },
    { path: '/login', label: 'Sign in', icon: 'i-login' },
  ];
}
