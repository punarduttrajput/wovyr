import { Routes } from '@angular/router';

/**
 * Feature routes are lazy-loaded (overview §8). DASH-406: `/` lands on
 * Monitoring — "what is the state of my system" is the right first question
 * for an operator console, and it's also the surface that degrades most
 * informatively when the server is unreachable. (The stale comment this
 * replaced dated from when Agent Studio was the only built surface and
 * everything else was a placeholder — no longer true; every surface below is
 * real.)
 */
export const routes: Routes = [
  { path: '', pathMatch: 'full', redirectTo: 'monitoring' },
  {
    path: 'agents',
    title: 'Agent Studio · Wovyr',
    loadComponent: () =>
      import('./features/agent-studio/agent-studio').then((m) => m.AgentStudio),
  },
  {
    path: 'monitoring',
    title: 'Monitoring · Wovyr',
    loadComponent: () => import('./features/monitoring/monitoring').then((m) => m.Monitoring),
  },
  {
    path: 'audit',
    title: 'Audit log · Wovyr',
    loadComponent: () => import('./features/audit/audit').then((m) => m.Audit),
  },
  {
    path: 'workflows',
    title: 'Workflow Builder · Wovyr',
    loadComponent: () =>
      import('./features/workflow-builder/workflow-builder').then((m) => m.WorkflowBuilder),
  },
  {
    path: 'executions/:id',
    title: 'Execution · Wovyr',
    loadComponent: () =>
      import('./features/execution-detail/execution-detail').then((m) => m.ExecutionDetail),
  },
  {
    path: 'playground',
    title: 'Playground · Wovyr',
    loadComponent: () => import('./features/playground/playground').then((m) => m.Playground),
  },
  {
    path: 'memory',
    title: 'Memory Explorer · Wovyr',
    loadComponent: () =>
      import('./features/memory-explorer/memory-explorer').then((m) => m.MemoryExplorer),
  },
  {
    path: 'surfaces',
    title: 'Surfaces · Wovyr',
    loadComponent: () => import('./features/surfaces/surfaces').then((m) => m.Surfaces),
  },
  {
    path: 'mcp-servers',
    title: 'MCP Servers · Wovyr',
    loadComponent: () =>
      import('./features/mcp-servers/mcp-servers').then((m) => m.McpServers),
  },
  {
    path: 'marketplace',
    title: 'Marketplace · Wovyr',
    loadComponent: () => import('./features/marketplace/marketplace').then((m) => m.Marketplace),
  },
  {
    path: 'settings',
    title: 'Settings · Wovyr',
    loadComponent: () => import('./features/settings/settings').then((m) => m.Settings),
  },
  {
    path: 'login',
    title: 'Sign in · Wovyr',
    loadComponent: () => import('./features/login/login').then((m) => m.Login),
  },
  {
    // DASH-405: this used to be `redirectTo: 'agents'` — every unknown route
    // silently landed in Agent Studio with a rewritten URL and a breadcrumb
    // implying the operator arrived where they intended. No `redirectTo`
    // here means the router renders this in place and keeps the real URL.
    path: '**',
    title: 'Not found · Wovyr',
    loadComponent: () => import('./features/not-found/not-found').then((m) => m.NotFound),
  },
];
