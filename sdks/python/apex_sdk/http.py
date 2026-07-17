"""The shared request/response plumbing every resource method calls through:
base-URL joining, default tenant/principal headers, query-string building,
JSON encode/decode, and error-envelope mapping into `ApexApiError`.

Built on `urllib` alone (no `requests`/`httpx`) so the SDK has zero runtime
dependencies — deliberate, since this repo's dev environment has no working
package installer to verify a third-party HTTP library against."""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from email.message import Message
from typing import Any, Iterator, Mapping, Optional

from .errors import ApexApiError

RETRYABLE_STATUSES = {429, 502, 503, 504}
DEFAULT_MAX_RETRIES = 2
DEFAULT_BASE_DELAY_S = 0.25


@dataclass
class RetryOptions:
    """Retry policy for transient failures (network errors, 429/502/503/504).
    Applied to every `GET`, and — DX-301 — to mutating requests **only when
    they carry an `Idempotency-Key`** (pass `idempotency_key=` on the call):
    the server's replay middleware then makes the retry safe, whereas a
    keyless retry could double-execute."""

    max_retries: int = DEFAULT_MAX_RETRIES
    base_delay_s: float = DEFAULT_BASE_DELAY_S


class RawResponse:
    """Normalizes `urlopen`'s success response and the response reconstructed
    from a caught `HTTPError` behind one interface, so callers don't care
    which path produced it."""

    def __init__(self, status: int, headers: Message, fp: Any) -> None:
        self.status = status
        self.headers = headers
        self._fp = fp

    def read(self) -> bytes:
        try:
            return self._fp.read()
        finally:
            close = getattr(self._fp, "close", None)
            if close is not None:
                close()

    def iter_lines(self) -> Iterator[bytes]:
        for raw_line in self._fp:
            yield raw_line


class Opener:
    """The subset of `urllib.request.OpenerDirector` this client depends on —
    lets tests substitute a fake opener without a real socket."""

    def open(self, request: urllib.request.Request) -> Any:  # pragma: no cover - protocol
        raise NotImplementedError


def _parse_error_body(text: str) -> Optional[dict[str, Any]]:
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    if isinstance(parsed, dict):
        error = parsed.get("error")
        if isinstance(error, dict):
            return error
    return None


class HttpClient:
    def __init__(
        self,
        base_url: str,
        *,
        tenant: Optional[str] = None,
        principal: Optional[str] = None,
        retry: Optional[RetryOptions] = None,
        opener: Optional[Opener] = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.tenant = tenant
        self.principal = principal
        self.retry = retry or RetryOptions()
        self._opener: Opener = opener or urllib.request.build_opener()

    def _url(self, path: str, query: Optional[Mapping[str, Any]] = None) -> str:
        url = self.base_url + path
        if query:
            filtered = {k: str(v) for k, v in query.items() if v is not None}
            if filtered:
                url += "?" + urllib.parse.urlencode(filtered)
        return url

    def _default_headers(self, extra: Optional[Mapping[str, str]] = None) -> dict[str, str]:
        headers = dict(extra or {})
        if self.tenant is not None:
            headers["X-Apex-Tenant"] = self.tenant
        if self.principal is not None:
            headers["X-Apex-Principal"] = self.principal
        return headers

    def raw(
        self,
        method: str,
        path: str,
        body: Any = None,
        *,
        query: Optional[Mapping[str, Any]] = None,
        headers: Optional[Mapping[str, str]] = None,
    ) -> RawResponse:
        """Perform a request and return the raw response (used by the SSE
        helper, which needs to iterate the body rather than decode it whole).
        Retries transient failures per `self.retry`: always for `GET`, and for
        mutating requests **only when an `Idempotency-Key` header rides
        along** (DX-301) — the server's replay middleware then guarantees a
        retried mutation can't double-execute, which is exactly the property
        a keyless retry would violate."""
        hdrs = self._default_headers(headers)
        data: Optional[bytes] = None
        if body is not None:
            hdrs["Content-Type"] = "application/json"
            data = json.dumps(body).encode("utf-8")
        url = self._url(path, query)
        idempotent = method == "GET" or "Idempotency-Key" in hdrs
        retries_allowed = self.retry.max_retries if idempotent else 0

        attempt = 0
        while True:
            request = urllib.request.Request(url, data=data, headers=hdrs, method=method)
            try:
                resp = self._opener.open(request)
                return RawResponse(resp.status, resp.headers, resp)
            except urllib.error.HTTPError as err:
                if attempt >= retries_allowed or err.code not in RETRYABLE_STATUSES:
                    return RawResponse(err.code, err.headers, err)
                err.close()
            except urllib.error.URLError:
                if attempt >= retries_allowed:
                    raise
            time.sleep(self.retry.base_delay_s * (2**attempt))
            attempt += 1

    def request(
        self,
        method: str,
        path: str,
        body: Any = None,
        *,
        query: Optional[Mapping[str, Any]] = None,
        headers: Optional[Mapping[str, str]] = None,
    ) -> Any:
        """Perform a request and decode the JSON body, raising `ApexApiError`
        on any non-2xx status. Returns `None` for `204 No Content`."""
        data, _ = self.request_with_headers(method, path, body, query=query, headers=headers)
        return data

    def request_with_headers(
        self,
        method: str,
        path: str,
        body: Any = None,
        *,
        query: Optional[Mapping[str, Any]] = None,
        headers: Optional[Mapping[str, str]] = None,
    ) -> tuple[Any, Message]:
        """Like `request`, but also returns the response headers — for
        endpoints that carry state in headers rather than the body (`ETag`
        on projects)."""
        response = self.raw(method, path, body, query=query, headers=headers)
        if response.status == 204:
            return None, response.headers
        text = response.read().decode("utf-8")
        if not (200 <= response.status < 300):
            raise ApexApiError(response.status, _parse_error_body(text), text)
        if not text:
            return None, response.headers
        return json.loads(text), response.headers
