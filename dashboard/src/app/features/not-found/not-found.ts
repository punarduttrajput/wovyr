import { Component, inject } from '@angular/core';
import { RouterLink, Router } from '@angular/router';

/**
 * DASH-405: previously every unknown, stale, or mistyped URL silently
 * redirected to Agent Studio (`{ path: '**', redirectTo: 'agents' }`) — the
 * breadcrumb then read "Build / Agent Studio", actively implying the
 * operator arrived where they intended. This route replaces that redirect:
 * it renders in place, keeps the real URL (no `redirectTo` on the wildcard
 * route in `app.routes.ts`), and offers a way out. The command palette's
 * ⌘K listener is mounted globally in the app shell, so it already works
 * here with no extra wiring — this page just tells the operator so.
 */
@Component({
  selector: 'app-not-found',
  imports: [RouterLink],
  template: `
    <section class="view not-found">
      <div class="card">
        <div class="code mono">404</div>
        <h1>Page not found</h1>
        <p>
          There's nothing at <code class="mono">{{ path() }}</code>. It may have moved, been
          deleted, or never existed.
        </p>
        <p class="hint">Press <kbd>⌘K</kbd> / <kbd>Ctrl K</kbd> to jump to a surface, or go to:</p>
        <nav aria-label="Main surfaces">
          <a routerLink="/monitoring">Monitoring</a>
          <a routerLink="/agents">Agent Studio</a>
          <a routerLink="/workflows">Workflow Builder</a>
          <a routerLink="/memory">Memory Explorer</a>
        </nav>
      </div>
    </section>
  `,
  styles: `
    .not-found { display:grid; place-items:center; min-height:60vh; padding:24px; }
    .card { max-width:440px; text-align:center; padding:32px 28px; }
    .code { font-size:13px; letter-spacing:.08em; color:var(--ink-3); margin-bottom:6px; }
    h1 { margin:0 0 10px; font-size:20px; }
    p { color:var(--ink-2); font-size:13.5px; margin:0 0 10px; }
    .hint { color:var(--ink-3); font-size:12.5px; }
    kbd { font-family:var(--mono); font-size:10.5px; border:1px solid var(--line); border-radius:4px; padding:1px 5px; }
    nav { display:flex; flex-wrap:wrap; gap:14px; justify-content:center; margin-top:14px; }
    nav a {
      color:var(--accent); text-decoration:none; font-size:13px; font-weight:600;
      padding:6px 4px; /* A11Y-208: pad the hit area, not the type */
    }
    nav a:hover { text-decoration:underline; }
  `,
})
export class NotFound {
  private router = inject(Router);
  path(): string {
    return this.router.url.split('?')[0];
  }
}
