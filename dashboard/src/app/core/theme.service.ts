import { Injectable, signal } from '@angular/core';

type Theme = 'light' | 'dark';
const KEY = 'wovyr.theme';

/** Toggles the `data-theme` attribute on <html>, persisted to localStorage. */
@Injectable({ providedIn: 'root' })
export class ThemeService {
  readonly theme = signal<Theme>(this.initial());

  constructor() {
    this.apply(this.theme());
  }

  toggle(): void {
    const next: Theme = this.theme() === 'dark' ? 'light' : 'dark';
    this.theme.set(next);
    this.apply(next);
    try {
      localStorage.setItem(KEY, next);
    } catch {
      /* storage unavailable — in-memory only */
    }
  }

  private apply(t: Theme): void {
    document.documentElement.setAttribute('data-theme', t === 'dark' ? 'dark' : '');
  }

  private initial(): Theme {
    try {
      const saved = localStorage.getItem(KEY) as Theme | null;
      if (saved === 'light' || saved === 'dark') return saved;
    } catch {
      /* ignore */
    }
    return matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
}
