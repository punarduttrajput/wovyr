export interface NavItem {
  path: string;
  label: string;
  /** Symbol id in the shared `icons.svg` sprite (UI-306). */
  icon: string;
  /** Live count rendered as a badge; absent = no badge. Fake hardcoded badge
   * strings were removed in UI-306 — a badge must mean something real. */
  badge?: string;
}

export interface NavGroup {
  key: string;
  label: string;
  items: NavItem[];
  /** `account` is a real group (its label is what the breadcrumb shows for
   * `/login`) but isn't rendered as a rail *section* — its one item lives
   * beside the identity block instead (see `app.html`'s rail-foot), since
   * authentication is not an "extension point" the way Marketplace/Settings
   * are. */
  showInNav?: boolean;
}

/**
 * DASH-404: nav grouping, defined exactly once. The rail (`app.html`, every
 * group with `showInNav !== false`) and the breadcrumb (`crumbFor` below)
 * both read from this — they cannot disagree the way the rail's three
 * sections and a separate hand-written breadcrumb label map used to (Settings
 * and Sign-in were filed under "Administer"/"Account" in the breadcrumb while
 * the rail itself only ever had Operate/Build/Extend, and both Settings and
 * Sign-in lived under "Extend" in the rail regardless).
 */
export const NAV_GROUPS: NavGroup[] = [
  {
    key: 'operate',
    label: 'Operate',
    items: [
      { path: '/monitoring', label: 'Monitoring', icon: 'i-monitoring' },
      { path: '/audit', label: 'Audit log', icon: 'i-audit' },
    ],
  },
  {
    key: 'build',
    label: 'Build',
    items: [
      { path: '/workflows', label: 'Workflow Builder', icon: 'i-workflows' },
      { path: '/agents', label: 'Agent Studio', icon: 'i-agents' },
      { path: '/playground', label: 'Playground', icon: 'i-playground' },
      { path: '/memory', label: 'Memory Explorer', icon: 'i-memory' },
      { path: '/surfaces', label: 'Surfaces', icon: 'i-surfaces' },
      { path: '/mcp-servers', label: 'MCP Servers', icon: 'i-mcp' },
    ],
  },
  {
    key: 'extend',
    label: 'Extend',
    items: [{ path: '/marketplace', label: 'Marketplace', icon: 'i-marketplace' }],
  },
  {
    key: 'administer',
    label: 'Administer',
    items: [{ path: '/settings', label: 'Settings', icon: 'i-settings' }],
  },
  {
    key: 'account',
    label: 'Account',
    items: [{ path: '/login', label: 'Sign in', icon: 'i-login' }],
    showInNav: false,
  },
];

/** Breadcrumb roots for routes that have no nav item of their own. */
export const EXTRA_CRUMBS: Record<string, { root: string; leaf: string }> = {
  executions: { root: 'Operate', leaf: 'Execution' },
};

/** Derives a `{root, leaf}` breadcrumb for a top-level route segment from `groups`. */
export function crumbFor(
  groups: NavGroup[],
  extra: Record<string, { root: string; leaf: string }>,
  segment: string,
): { root: string; leaf: string } {
  for (const g of groups) {
    const item = g.items.find((i) => i.path === '/' + segment);
    if (item) return { root: g.label, leaf: item.label };
  }
  return extra[segment] ?? { root: '', leaf: segment };
}
