"""`wovyr_sdk.aio.AsyncWovyrClient` (DX-301) — offline unit tests over fake
openers (no server needed), plus live integration tests that skip cleanly
when no `wovyr dev` answers at `WOVYR_TEST_BASE_URL` (same gating as
`test_client.py`)."""

from __future__ import annotations

import asyncio
import io
import os
import unittest
import urllib.error
import urllib.request
from email.message import Message
from typing import Any

from wovyr_sdk import WovyrTimeoutError
from wovyr_sdk.aio import AsyncWovyrClient, paginate_all
from wovyr_sdk.http import Opener

BASE_URL = os.environ.get("WOVYR_TEST_BASE_URL", "http://127.0.0.1:8080")

HELLO_MANIFEST = """
apiVersion: agent.wovyr.io/v1
kind: Agent
metadata:
  name: hello
spec:
  model_selector: { capability: chat, class: fast }
  instructions: |
    You are a friendly assistant. Greet the user and answer briefly.
"""


def _server_available() -> bool:
    try:
        with urllib.request.urlopen(f"{BASE_URL}/healthz", timeout=3) as resp:
            return 200 <= resp.status < 300
    except Exception:
        return False


class _FakeHttpResponse:
    def __init__(self, status: int, body: bytes, headers: Message) -> None:
        self.status = status
        self.headers = headers
        self._fp = io.BytesIO(body)

    def read(self) -> bytes:
        return self._fp.read()

    def __iter__(self) -> Any:
        return iter(self._fp)

    def close(self) -> None:
        pass


def _json_response(body: bytes, status: int = 200) -> _FakeHttpResponse:
    headers = Message()
    headers["content-type"] = "application/json"
    return _FakeHttpResponse(status, body, headers)


class _ScriptedOpener(Opener):
    """Serves responses from a list of `(match_substring, response_factory)`
    rules; the first matching rule answers."""

    def __init__(self, rules: list[tuple[str, Any]]) -> None:
        self.rules = rules
        self.calls: list[str] = []

    def open(self, request: urllib.request.Request) -> Any:
        url = request.full_url
        self.calls.append(url)
        for needle, factory in self.rules:
            if needle in url:
                return factory()
        raise AssertionError(f"no scripted response for {url}")


class AsyncClientUnitTests(unittest.TestCase):
    """Offline: the async facade delegates faithfully — same results, same
    exceptions — and the SSE bridge yields frames as an async iterator."""

    def test_health_and_resource_methods_await_the_sync_results(self) -> None:
        opener = _ScriptedOpener(
            [
                ("/healthz", lambda: _json_response(b'{"status": "ok", "version": "test"}')),
                (
                    "/api/v1/workflows/wf-1",
                    lambda: _json_response(
                        b'{"execution": {"execution_id": "wf-1", "status": "completed"}, "events": []}'
                    ),
                ),
            ]
        )
        client = AsyncWovyrClient("http://unit-test.invalid", opener=opener)

        async def main() -> None:
            health = await client.health()
            self.assertEqual(health["status"], "ok")
            got = await client.workflows.wait_for_completion("wf-1", interval_s=0.001)
            self.assertEqual(got["execution"]["status"], "completed")

        asyncio.run(main())

    def test_wait_for_completion_timeout_propagates(self) -> None:
        opener = _ScriptedOpener(
            [
                (
                    "/api/v1/workflows/",
                    lambda: _json_response(
                        b'{"execution": {"execution_id": "wf-1", "status": "running"}, "events": []}'
                    ),
                ),
            ]
        )
        client = AsyncWovyrClient("http://unit-test.invalid", opener=opener)

        async def main() -> None:
            with self.assertRaises(WovyrTimeoutError):
                await client.workflows.wait_for_completion(
                    "wf-1", interval_s=0.005, timeout_s=0.02
                )

        asyncio.run(main())

    def test_stream_bridges_sse_frames_to_an_async_iterator(self) -> None:
        sse = (
            b'data: {"type":"start","model":"mock"}\n\n'
            b'data: {"type":"delta","text":"Hi"}\n\n'
            b'event: result\ndata: {"status":"succeeded","output":{"message":"Hi"},"steps":1}\n\n'
        )

        def stream_response() -> _FakeHttpResponse:
            headers = Message()
            headers["content-type"] = "text/event-stream"
            return _FakeHttpResponse(200, sse, headers)

        opener = _ScriptedOpener([("/api/v1/agents:stream", stream_response)])
        client = AsyncWovyrClient("http://unit-test.invalid", opener=opener)

        async def main() -> None:
            kinds = []
            async for frame in client.agents.stream({"manifest": "m", "input": {}}):
                kinds.append(frame["type"])
            self.assertEqual(kinds, ["start", "delta", "result"])

        asyncio.run(main())

    def test_async_paginate_all_drains_pages(self) -> None:
        pages = [
            b'{"data": ["a", "b"], "has_more": true, "next_cursor": "c1", "total_estimate": 3}',
            b'{"data": ["c"], "has_more": false, "next_cursor": null, "total_estimate": 3}',
        ]
        calls = {"n": 0}

        def next_page() -> _FakeHttpResponse:
            body = pages[min(calls["n"], len(pages) - 1)]
            calls["n"] += 1
            return _json_response(body)

        opener = _ScriptedOpener([("/api/v1/agents", next_page)])
        client = AsyncWovyrClient("http://unit-test.invalid", opener=opener)

        async def main() -> None:
            items = [item async for item in paginate_all(client.agents.list)]
            self.assertEqual(items, ["a", "b", "c"])

        asyncio.run(main())


class AsyncClientIntegrationTests(unittest.TestCase):
    """Live: the async client passes the same core flows as the sync suite —
    the DX-301 acceptance run. Skips cleanly with no server."""

    def setUp(self) -> None:
        if not _server_available():
            self.skipTest(f"no wovyr-server reachable at {BASE_URL}")
        self.client = AsyncWovyrClient(BASE_URL, principal="sdk-test-admin")

    def test_health_reports_ok(self) -> None:
        health = asyncio.run(self.client.health())
        self.assertEqual(health["status"], "ok")

    def test_agents_run_and_stream_end_to_end(self) -> None:
        async def main() -> None:
            result = await self.client.agents.run(
                {"manifest": HELLO_MANIFEST, "input": {"message": "Hi"}}
            )
            self.assertEqual(result["status"], "succeeded")

            kinds = []
            async for frame in self.client.agents.stream(
                {"manifest": HELLO_MANIFEST, "input": {"message": "Hi"}}
            ):
                kinds.append(frame["type"])
            self.assertIn("start", kinds)
            self.assertEqual(kinds[-1], "result")

        asyncio.run(main())

    def test_workflows_submit_then_wait_for_completion(self) -> None:
        manifest = (
            "metadata:\n  name: sdk-async-test\n  version: 1.0.0\n"
            "spec:\n  activities:\n    - {id: a, type: function, name: echo, inputs: {message: hi}}\n"
        )

        async def main() -> None:
            import time as _time

            submitted = await self.client.workflows.submit(
                {
                    "manifest": manifest,
                    "input": {},
                    "execution_id": f"sdk-async-test-{int(_time.time() * 1000)}",
                }
            )
            self.assertEqual(submitted["status"], "submitted")
            got = await self.client.workflows.wait_for_completion(
                submitted["execution_id"], interval_s=0.1, timeout_s=10.0
            )
            self.assertEqual(got["execution"]["status"], "completed")

        try:
            asyncio.run(main())
        except Exception as err:  # pragma: no cover - env-dependent skip
            from wovyr_sdk import WovyrApiError

            if isinstance(err, WovyrApiError) and err.status == 403:
                self.skipTest("server not started with WOVYR_PLATFORM_ADMINS=sdk-test-admin")
            raise


if __name__ == "__main__":
    unittest.main()
