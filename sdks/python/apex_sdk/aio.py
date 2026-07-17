"""The asyncio-friendly client (DX-301): the same resource surface as
`ApexClient`, with every method awaitable.

Transport strategy — deliberate, not an oversight: each call delegates the
sync client's corresponding method to a worker thread via
`asyncio.to_thread`, so the event loop never blocks. The alternative (a
hand-rolled asyncio HTTP/1.1 client) would re-implement redirects, chunked
transfer decoding, and TLS plumbing that `urllib` already gets right, for no
observable API difference — this SDK stays zero-dependency either way.
Streaming (`agents.stream`) bridges the sync SSE iterator through an
`asyncio.Queue` fed from the worker thread, so frames are yielded as they
arrive rather than after the run finishes.

Usage::

    from apex_sdk.aio import AsyncApexClient

    client = AsyncApexClient("http://127.0.0.1:8080")
    health = await client.health()
    result = await client.agents.run({"manifest": manifest, "input": {"message": "Hi"}})
    async for frame in client.agents.stream({"manifest": manifest}):
        ...
    final = await client.workflows.wait_for_completion(execution_id)

Every wrapped method keeps the sync method's signature, docstring, and
error behavior (`ApexApiError`, `ApexTimeoutError`) — `help()` on an async
resource method shows the underlying documentation.
"""

from __future__ import annotations

import asyncio
import functools
from typing import Any, AsyncIterator, Callable, Optional

from .client import AgentsResource, ApexClient
from .http import Opener, RetryOptions
from .types import Page, RunRequest


class _AsyncResource:
    """Wraps a sync resource: attribute access returns an awaitable version of
    the same method (run in a worker thread). Non-callable attributes pass
    through unchanged."""

    def __init__(self, target: Any) -> None:
        self._target = target

    def __getattr__(self, name: str) -> Any:
        attr = getattr(self._target, name)
        if not callable(attr):
            return attr

        @functools.wraps(attr)
        async def call(*args: Any, **kwargs: Any) -> Any:
            return await asyncio.to_thread(attr, *args, **kwargs)

        return call


class _AsyncAgents(_AsyncResource):
    """Agents need one special case: `stream` yields frames incrementally, so
    a plain to-thread wrapper (which would buffer the whole run) won't do."""

    def __init__(self, target: AgentsResource) -> None:
        super().__init__(target)
        self._agents = target

    async def stream(
        self, req: RunRequest, *, project: Optional[str] = None
    ) -> AsyncIterator[dict[str, Any]]:
        """`POST /api/v1/agents:stream` — like the sync `stream`, but an async
        iterator: the blocking SSE read runs in a worker thread and each frame
        is handed to the event loop as it arrives."""
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue[tuple[str, Any]] = asyncio.Queue()

        def pump() -> None:
            try:
                for frame in self._agents.stream(req, project=project):
                    loop.call_soon_threadsafe(queue.put_nowait, ("frame", frame))
                loop.call_soon_threadsafe(queue.put_nowait, ("done", None))
            except BaseException as exc:  # noqa: BLE001 - propagated to the awaiter
                loop.call_soon_threadsafe(queue.put_nowait, ("error", exc))

        pump_task = asyncio.create_task(asyncio.to_thread(pump))
        try:
            while True:
                kind, value = await queue.get()
                if kind == "frame":
                    yield value
                elif kind == "error":
                    raise value
                else:
                    return
        finally:
            await pump_task


class AsyncApexClient:
    """Async facade over `ApexClient` — same constructor, same resources
    (`agents`, `workflows`, `memory`, `plugins`, `marketplace`, `secrets`,
    `organizations`, `projects`, `webhooks`, `audit`, `tools`), every method
    awaitable."""

    def __init__(
        self,
        base_url: str,
        *,
        tenant: Optional[str] = None,
        principal: Optional[str] = None,
        retry: Optional[RetryOptions] = None,
        opener: Optional[Opener] = None,
    ) -> None:
        self._sync = ApexClient(
            base_url, tenant=tenant, principal=principal, retry=retry, opener=opener
        )
        self.agents = _AsyncAgents(self._sync.agents)
        self.workflows = _AsyncResource(self._sync.workflows)
        self.memory = _AsyncResource(self._sync.memory)
        self.plugins = _AsyncResource(self._sync.plugins)
        self.marketplace = _AsyncResource(self._sync.marketplace)
        self.secrets = _AsyncResource(self._sync.secrets)
        self.organizations = _AsyncResource(self._sync.organizations)
        self.projects = _AsyncResource(self._sync.projects)
        self.webhooks = _AsyncResource(self._sync.webhooks)
        self.audit = _AsyncResource(self._sync.audit)
        self.tools = _AsyncResource(self._sync.tools)

    async def health(self) -> dict[str, Any]:
        """`GET /healthz`."""
        return await asyncio.to_thread(self._sync.health)


async def paginate_all(
    fetch_page: Callable[..., Any], **params: Any
) -> AsyncIterator[Any]:
    """Async twin of `apex_sdk.paginate_all`, for the awaitable list methods:
    `async for item in paginate_all(client.agents.list, limit=25)`. Stops once
    `has_more` is false or `next_cursor` is `None`, whichever comes first."""
    cursor = None
    while True:
        page: Page = await fetch_page(cursor=cursor, **params)
        for item in page["data"]:
            yield item
        if not page["has_more"] or not page["next_cursor"]:
            return
        cursor = page["next_cursor"]
