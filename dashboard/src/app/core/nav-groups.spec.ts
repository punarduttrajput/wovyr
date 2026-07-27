import { NAV_GROUPS, EXTRA_CRUMBS, crumbFor } from './nav-groups';
import { routes } from '../app.routes';

/**
 * DASH-404's regression guard: every real route must resolve to a breadcrumb
 * root that is an actual nav-group label, so the rail and the breadcrumb
 * cannot silently disagree again the way they used to (Settings/Sign-in
 * breadcrumbed under "Administer"/"Account", neither of which existed in the
 * rail's Operate/Build/Extend sections).
 */
describe('nav-groups (DASH-404)', () => {
  const groupLabels = new Set(NAV_GROUPS.map((g) => g.label));

  const concreteSegments = routes
    .map((r) => r.path ?? '')
    .filter((p) => p && p !== '**')
    .map((p) => p.split('/')[0]);

  for (const seg of concreteSegments) {
    it(`/${seg}'s breadcrumb root is a real nav group`, () => {
      const { root } = crumbFor(NAV_GROUPS, EXTRA_CRUMBS, seg);
      expect(groupLabels.has(root)).withContext(`root was "${root}"`).toBe(true);
    });
  }

  it('every nav item belongs to exactly one group', () => {
    const paths = NAV_GROUPS.flatMap((g) => g.items.map((i) => i.path));
    expect(new Set(paths).size).toBe(paths.length);
  });

  it('the account group backs /login but is not rendered as a rail section', () => {
    const account = NAV_GROUPS.find((g) => g.key === 'account');
    expect(account?.showInNav).toBe(false);
    expect(account?.items[0].path).toBe('/login');
  });
});
