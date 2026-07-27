import { Component, HostListener, computed, inject, signal } from '@angular/core';
import { Router, NavigationEnd, RouterOutlet, RouterLink, RouterLinkActive } from '@angular/router';
import { filter } from 'rxjs/operators';
import { Session } from './core/session';
import { ThemeService } from './core/theme.service';
import { ToastService } from './core/toast.service';
import { CommandPalette } from './shared/command-palette';
import { ConfirmDialog } from './shared/confirm';
import { NAV_GROUPS, EXTRA_CRUMBS, crumbFor } from './core/nav-groups';

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

  readonly crumb = signal<{ root: string; leaf: string }>({ root: 'Operate', leaf: 'Monitoring' });

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

  /** DASH-404: the single nav-grouping source of truth (`core/nav-groups.ts`) —
   * both the rail and the breadcrumb read from this, so they cannot disagree. */
  readonly navGroups = NAV_GROUPS;
  /** The rail's own nav sections — every `navGroups` entry rendered in `<nav>`. */
  readonly railGroups = computed(() => this.navGroups.filter((g) => g.showInNav !== false));

  constructor() {
    this.router.events
      .pipe(filter((e): e is NavigationEnd => e instanceof NavigationEnd))
      .subscribe((e) => {
        const seg = e.urlAfterRedirects.split('?')[0].split('/')[1] || 'monitoring';
        this.crumb.set(crumbFor(this.navGroups, EXTRA_CRUMBS, seg));
        this.navOpen.set(false); // navigating closes the mobile drawer
      });
  }

  @HostListener('document:keydown.escape')
  closeNav(): void {
    this.navOpen.set(false);
  }
}
