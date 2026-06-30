import { Routes } from '@angular/router';

/**
 * Feature routes are lazy-loaded (overview §8). Agent Studio is the first built
 * surface; the remaining surfaces resolve to a placeholder until their slices land.
 */
export const routes: Routes = [
  { path: '', pathMatch: 'full', redirectTo: 'agents' },
  {
    path: 'agents',
    title: 'Agent Studio · Apex',
    loadComponent: () =>
      import('./features/agent-studio/agent-studio').then((m) => m.AgentStudio),
  },
  {
    path: 'monitoring',
    title: 'Monitoring · Apex',
    loadComponent: () => import('./features/monitoring/monitoring').then((m) => m.Monitoring),
  },
  {
    path: 'workflows',
    title: 'Workflow Builder · Apex',
    loadComponent: () => import('./features/placeholder/placeholder').then((m) => m.Placeholder),
    data: { surface: 'Workflow Builder', eyebrow: 'Build / Workflow Builder' },
  },
  {
    path: 'memory',
    title: 'Memory Explorer · Apex',
    loadComponent: () => import('./features/placeholder/placeholder').then((m) => m.Placeholder),
    data: { surface: 'Memory Explorer', eyebrow: 'Build / Memory Explorer' },
  },
  {
    path: 'marketplace',
    title: 'Marketplace · Apex',
    loadComponent: () => import('./features/placeholder/placeholder').then((m) => m.Placeholder),
    data: { surface: 'Marketplace', eyebrow: 'Extend / Marketplace' },
  },
  {
    path: 'settings',
    title: 'Settings · Apex',
    loadComponent: () => import('./features/settings/settings').then((m) => m.Settings),
  },
  { path: '**', redirectTo: 'agents' },
];
