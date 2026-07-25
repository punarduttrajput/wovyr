"""The `{ error: { code, message, type, status, request_id } }` envelope every
Wovyr API error response carries (see docs/09-api/overview.md §8)."""

from __future__ import annotations

from typing import Any, Optional


class WovyrApiError(Exception):
    """Raised for any non-2xx response. Carries the parsed error envelope when
    the server returned one (it always does, for JSON endpoints); falls back
    to the raw response text otherwise (e.g. a proxy/network failure upstream
    of the server)."""

    def __init__(self, status: int, body: Optional[dict[str, Any]], raw_text: str) -> None:
        self.status = status
        self.body = body
        self.code: str = (body or {}).get("code", "unknown_error")
        self.request_id: Optional[str] = (body or {}).get("request_id")
        message = (body or {}).get("message") if body else None
        super().__init__(message or f"Wovyr API request failed with status {status}: {raw_text}")


class WovyrTimeoutError(Exception):
    """Raised when a client-side wait (e.g. `workflows.wait_for_completion`)
    exhausts its timeout before the awaited condition holds. Not an API error
    — the server never rejected anything; the caller's deadline simply
    passed."""
