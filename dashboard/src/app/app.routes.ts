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
    loadComponent: () =>
      import('./features/workflow-builder/workflow-builder').then((m) => m.WorkflowBuilder),
  },
  {
    path: 'executions/:id',
    title: 'Execution · Apex',
    loadComponent: () =>
      import('./features/execution-detail/execution-detail').then((m) => m.ExecutionDetail),
  },
  {
    path: 'memory',
    title: 'Memory Explorer · Apex',
    loadComponent: () =>
      import('./features/memory-explorer/memory-explorer').then((m) => m.MemoryExplorer),
  },
  {
    path: 'surfaces',
    title: 'Surfaces · Apex',
    loadComponent: () => import('./features/surfaces/surfaces').then((m) => m.Surfaces),
  },
  {
    path: 'mcp-servers',
    title: 'MCP Servers · Apex',
    loadComponent: () =>
      import('./features/mcp-servers/mcp-servers').then((m) => m.McpServers),
  },
  {
    path: 'marketplace',
    title: 'Marketplace · Apex',
    loadComponent: () => import('./features/marketplace/marketplace').then((m) => m.Marketplace),
  },
  {
    path: 'settings',
    title: 'Settings · Apex',
    loadComponent: () => import('./features/settings/settings').then((m) => m.Settings),
  },
  {
    path: 'login',
    title: 'Sign in · Apex',
    loadComponent: () => import('./features/login/login').then((m) => m.Login),
  },
  { path: '**', redirectTo: 'agents' },
];
