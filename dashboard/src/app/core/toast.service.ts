import { Injectable, signal } from '@angular/core';

export interface Toast {
  id: number;
  message: string;
  kind: 'ok' | 'err';
}

/** Lightweight transient notifications. A control should say exactly what happened. */
@Injectable({ providedIn: 'root' })
export class ToastService {
  readonly toasts = signal<Toast[]>([]);
  private next = 1;

  show(message: string, kind: 'ok' | 'err' = 'ok'): void {
    const id = this.next++;
    this.toasts.update((t) => [...t, { id, message, kind }]);
    setTimeout(() => this.dismiss(id), 2800);
  }

  dismiss(id: number): void {
    this.toasts.update((t) => t.filter((x) => x.id !== id));
  }
}
