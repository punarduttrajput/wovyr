import { Component, inject, signal } from '@angular/core';
import { Router, NavigationEnd, RouterOutlet, RouterLink, RouterLinkActive } from '@angular/router';
import { filter } from 'rxjs/operators';
import { ThemeService } from './core/theme.service';
import { SafeSvgPipe } from './core/safe-svg.pipe';

interface NavItem {
  path: string;
  label: string;
  icon: string; // svg inner markup
  badge?: string;
}

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, SafeSvgPipe],
  templateUrl: './app.html',
  styleUrl: './app.scss',
})
export class App {
  readonly theme = inject(ThemeService);
  private router = inject(Router);

  readonly crumb = signal<{ root: string; leaf: string }>({ root: 'Build', leaf: 'Agent Studio' });
  private readonly labels: Record<string, { root: string; leaf: string }> = {
    agents: { root: 'Build', leaf: 'Agent Studio' },
    monitoring: { root: 'Operate', leaf: 'Monitoring' },
    workflows: { root: 'Build', leaf: 'Workflow Builder' },
    memory: { root: 'Build', leaf: 'Memory Explorer' },
    marketplace: { root: 'Extend', leaf: 'Marketplace' },
    settings: { root: 'Administer', leaf: 'Settings' },
  };

  constructor() {
    this.router.events
      .pipe(filter((e): e is NavigationEnd => e instanceof NavigationEnd))
      .subscribe((e) => {
        const seg = e.urlAfterRedirects.split('?')[0].split('/')[1] || 'agents';
        this.crumb.set(this.labels[seg] ?? { root: '', leaf: seg });
      });
  }

  readonly operate: NavItem[] = [
    {
      path: '/monitoring',
      label: 'Monitoring',
      badge: '12',
      icon: '<path d="M3 12h4l3 8 4-16 3 8h4" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>',
    },
  ];

  readonly build: NavItem[] = [
    {
      path: '/workflows',
      label: 'Workflow Builder',
      icon: '<rect x="3" y="4" width="6" height="5" rx="1.4" stroke="currentColor" stroke-width="2"/><rect x="15" y="15" width="6" height="5" rx="1.4" stroke="currentColor" stroke-width="2"/><path d="M9 6h4a2 2 0 012 2v9" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>',
    },
    {
      path: '/agents',
      label: 'Agent Studio',
      icon: '<rect x="4" y="7" width="16" height="12" rx="3" stroke="currentColor" stroke-width="2"/><path d="M12 7V4M9 13h.01M15 13h.01" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>',
    },
    {
      path: '/memory',
      label: 'Memory Explorer',
      icon: '<ellipse cx="12" cy="6" rx="7" ry="3" stroke="currentColor" stroke-width="2"/><path d="M5 6v6c0 1.66 3.13 3 7 3s7-1.34 7-3V6M5 12v6c0 1.66 3.13 3 7 3s7-1.34 7-3v-6" stroke="currentColor" stroke-width="2"/>',
    },
  ];

  readonly extend: NavItem[] = [
    {
      path: '/marketplace',
      label: 'Marketplace',
      badge: '3',
      icon: '<path d="M4 8h16l-1 11H5L4 8Z" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M8 8V6a4 4 0 018 0v2" stroke="currentColor" stroke-width="2"/>',
    },
    {
      path: '/settings',
      label: 'Settings',
      icon: '<circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="2"/><path d="M12 2v3m0 14v3M4.2 4.2l2.1 2.1m11.4 11.4l2.1 2.1M2 12h3m14 0h3M4.2 19.8l2.1-2.1m11.4-11.4l2.1-2.1" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>',
    },
  ];
}
