import { Component, HostListener, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';

interface Cmd {
  section: string;
  label: string;
  hint?: string;
  go: string;
}

/**
 * A ⌘K / Ctrl-K command palette for jumping between surfaces. Opened by the shortcut
 * or by clicking the top-bar search; Esc closes, ↑/↓ move, ↵ runs the selection.
 */
@Component({
  selector: 'app-command-palette',
  template: `
    @if (open()) {
      <div class="scrim" (click)="close()">
        <div class="cmdk" role="dialog" aria-label="Command palette" (click)="$event.stopPropagation()">
          <div class="cin">
            <svg viewBox="0 0 24 24" fill="none"><circle cx="11" cy="11" r="7" stroke="currentColor" stroke-width="2"/><path d="M21 21l-4-4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
            <input #box [value]="q()" (input)="onInput($event)" placeholder="Jump to a surface…" autocomplete="off" />
            <kbd>esc</kbd>
          </div>
          <div class="clist">
            @for (c of filtered(); track c.label; let i = $index) {
              <button class="crow" [class.sel]="i === sel()" (mouseenter)="sel.set(i)" (click)="run(c)">
                <span class="ci"></span>
                <span class="cl">{{ c.label }}</span>
                <span class="cs">{{ c.section }}</span>
                @if (c.hint) { <span class="ck">{{ c.hint }}</span> }
              </button>
            } @empty {
              <div class="cempty">No matches</div>
            }
          </div>
        </div>
      </div>
    }
  `,
  styles: [
    `
    .scrim { position:fixed; inset:0; background:rgba(13,20,36,.42); backdrop-filter:blur(3px);
      display:flex; align-items:flex-start; justify-content:center; padding-top:12vh; z-index:100; }
    .cmdk { width:min(560px,92vw); background:var(--surface); border:1px solid var(--line);
      border-radius:14px; box-shadow:var(--shadow-lg); overflow:hidden; }
    .cin { display:flex; align-items:center; gap:10px; padding:14px 16px; border-bottom:1px solid var(--line-2); }
    .cin svg { width:18px; height:18px; color:var(--ink-3); }
    .cin input { border:0; background:transparent; outline:none; font-size:15.5px; color:var(--ink); width:100%; font-family:var(--sans); }
    .cin kbd { font-family:var(--mono); font-size:10px; border:1px solid var(--line); border-radius:4px; padding:1px 5px; color:var(--ink-3); }
    .clist { max-height:340px; overflow:auto; padding:7px; }
    .crow { display:flex; align-items:center; gap:11px; padding:9px 11px; border-radius:8px; cursor:pointer;
      font-size:13.5px; border:0; background:transparent; width:100%; text-align:left; color:var(--ink); }
    .crow .ci { width:8px; height:8px; border-radius:50%; background:var(--accent); flex:none; opacity:.5; }
    .crow .cl { font-weight:500; }
    .crow .cs { margin-left:auto; font-family:var(--mono); font-size:10px; text-transform:uppercase; letter-spacing:.06em; color:var(--ink-3); }
    .crow.sel { background:var(--accent-weak); color:var(--accent-ink); }
    .crow.sel .ci { opacity:1; }
    .cempty { padding:18px; text-align:center; color:var(--ink-3); font-size:13px; }
    @media (prefers-reduced-motion:reduce){ .scrim{ backdrop-filter:none; } }
  `,
  ],
})
export class CommandPalette {
  private router = inject(Router);
  readonly open = signal(false);
  readonly q = signal('');
  readonly sel = signal(0);

  private cmds: Cmd[] = [
    { section: 'Operate', label: 'Monitoring', hint: 'G M', go: '/monitoring' },
    { section: 'Build', label: 'Workflow Builder', hint: 'G W', go: '/workflows' },
    { section: 'Build', label: 'Agent Studio', hint: 'G A', go: '/agents' },
    { section: 'Build', label: 'Memory Explorer', hint: 'G E', go: '/memory' },
    { section: 'Build', label: 'Surfaces', hint: 'G U', go: '/surfaces' },
    { section: 'Extend', label: 'Marketplace', hint: 'G K', go: '/marketplace' },
    { section: 'Administer', label: 'Settings', hint: 'G S', go: '/settings' },
  ];

  readonly filtered = computed(() => {
    const needle = this.q().toLowerCase().trim();
    if (!needle) return this.cmds;
    return this.cmds.filter((c) => (c.label + ' ' + c.section).toLowerCase().includes(needle));
  });

  toggle(): void {
    this.open.update((o) => !o);
    if (this.open()) {
      this.q.set('');
      this.sel.set(0);
    }
  }
  close(): void {
    this.open.set(false);
  }
  onInput(e: Event): void {
    this.q.set((e.target as HTMLInputElement).value);
    this.sel.set(0);
  }
  run(c: Cmd): void {
    this.close();
    this.router.navigateByUrl(c.go);
  }

  @HostListener('document:keydown', ['$event'])
  onKey(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      this.toggle();
      return;
    }
    if (!this.open()) return;
    const list = this.filtered();
    if (e.key === 'Escape') this.close();
    else if (e.key === 'ArrowDown') {
      e.preventDefault();
      this.sel.update((s) => Math.min(list.length - 1, s + 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      this.sel.update((s) => Math.max(0, s - 1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const c = list[this.sel()];
      if (c) this.run(c);
    }
  }
}
