import {
  HttpContext,
  HttpContextToken,
  HttpErrorResponse,
  HttpInterceptorFn,
} from '@angular/common/http';
import { inject } from '@angular/core';
import { throwError } from 'rxjs';
import { catchError } from 'rxjs/operators';
import { ToastService } from './toast.service';

/**
 * UI-302: central HTTP error handling. One `errText` (previously copied verbatim
 * into ≥4 components) and one interceptor that surfaces every failed platform
 * call as an error toast — so a failed background poll can never again be
 * silently swallowed by an `error: () => {}` no-op. Components that render a
 * failure inline still do; the toast is the floor, not the ceiling.
 */

/** Human-readable text for an HTTP (or other) error. Understands the server's
 * `{error: {message, code?}}` envelope (RM-GA-P4 API-702) as surfaced through
 * Angular's `HttpErrorResponse.error` body. */
export function errText(e: unknown): string {
  const err = e as {
    status?: number;
    error?: { error?: { message?: string } } | string;
    message?: string;
  };
  const body = err?.error;
  const enveloped = typeof body === 'object' ? body?.error?.message : undefined;
  if (enveloped) return enveloped;
  if (typeof body === 'string' && body.trim() && !body.trim().startsWith('<')) return body.trim();
  return err?.message ?? 'request failed';
}

/** Marks a request whose failure the caller surfaces through its own dedicated
 * UI (e.g. Monitoring's "unreachable" banner) — the interceptor stays quiet.
 * Usage: `this.http.get(url, { context: silentErrors() })`. */
export const SILENT_HTTP_ERRORS = new HttpContextToken<boolean>(() => false);

export function silentErrors(): HttpContext {
  return new HttpContext().set(SILENT_HTTP_ERRORS, true);
}

/** How long an identical error message is suppressed after being toasted, so a
 * 5s poll against a down server complains once, not continuously. */
const DEDUPE_MS = 30_000;

export const httpErrorInterceptor: HttpInterceptorFn = (req, next) => {
  const toast = inject(ToastService);
  return next(req).pipe(
    catchError((e: unknown) => {
      if (!req.context.get(SILENT_HTTP_ERRORS) && e instanceof HttpErrorResponse) {
        const message = `${req.method} ${req.url.split('?')[0]} — ${errText(e)}`;
        const now = Date.now();
        const last = recentToasts.get(message) ?? 0;
        if (now - last > DEDUPE_MS) {
          recentToasts.set(message, now);
          // Bound the dedupe map — it only ever holds recent failure lines.
          for (const [k, t] of recentToasts) {
            if (now - t > DEDUPE_MS) recentToasts.delete(k);
          }
          toast.show(message, 'err');
        }
      }
      return throwError(() => e);
    }),
  );
};

const recentToasts = new Map<string, number>();

/** Test hook: forget dedupe history so specs are order-independent. */
export function resetHttpErrorDedupe(): void {
  recentToasts.clear();
}
