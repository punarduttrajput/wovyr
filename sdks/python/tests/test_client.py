"""Integration tests against a real, locally running `wovyr dev` server
(`cargo run -p wovyr-cli -- dev --addr 127.0.0.1:8080`) — not mocked. Run with
the server already up:

    python3 -m unittest discover -s tests -v

Skips cleanly (not failing) if no server answers at `WOVYR_TEST_BASE_URL`
(default `http://127.0.0.1:8080`), so this suite doesn't fail an offline run
that never started one.

Like the TypeScript suite, almost every mutating/tenant-scoped flow below
(workflows submit/poll, memory, secrets, both pagination tests, org/project)
uses `_admin_client()` and needs the server started with
`WOVYR_PLATFORM_ADMINS=sdk-test-admin` (SEC-105: nothing tenant-scoped is
reachable via anonymity alone); those tests error with a 403 otherwise.
Run-only agent routes and reads (`health`, `tools`, `validate`) stay
anonymous.
"""

from __future__ import annotations

import io
import os
import time
import unittest
import urllib.error
import urllib.request
from email.message import Message
from typing import Any, Optional

from wovyr_sdk import (
    WovyrApiError,
    WovyrClient,
    WovyrTimeoutError,
    WovyrVersionSkewWarning,
    RetryOptions,
    paginate_all,
    sdk_version,
)
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


def _client() -> WovyrClient:
    return WovyrClient(BASE_URL)


def _admin_client() -> WovyrClient:
    """Org/project management routes need a real `org.admin`/`platform.admin`
    role. The test server is expected to run with
    `WOVYR_PLATFORM_ADMINS=sdk-test-admin`, making this principal a platform
    admin."""
    return WovyrClient(BASE_URL, principal="sdk-test-admin")


@unittest.skipUnless(_server_available(), f"no wovyr-server reachable at {BASE_URL}")
class WovyrClientIntegrationTests(unittest.TestCase):
    def test_health_reports_ok(self) -> None:
        health = _client().health()
        self.assertEqual(health["status"], "ok")
        self.assertGreater(len(health["version"]), 0)

    def test_tools_list_includes_the_built_in_echo_tool(self) -> None:
        res = _client().tools.list()
        # Default hosted registry (SEC-301): echo, fs_read, http_get — shell,
        # image_generate, and any plugin tools are each conditional opt-ins a
        # clean environment won't have, so 3 is the true floor, not 4.
        self.assertGreaterEqual(res["total_estimate"], 3)
        self.assertTrue(any(tool["id"] == "echo" for tool in res["data"]))

    def test_agents_run_runs_an_inline_manifest_end_to_end(self) -> None:
        result = _client().agents.run({"manifest": HELLO_MANIFEST, "input": {"message": "Hi"}})
        self.assertEqual(result["status"], "succeeded")
        self.assertGreater(len(result["output"]["message"]), 0)
        self.assertGreaterEqual(result["steps"], 1)

    def test_agents_run_with_a_malformed_manifest_raises_wovyr_api_error_400(self) -> None:
        with self.assertRaises(WovyrApiError) as ctx:
            _client().agents.run({"manifest": "not: [valid, agent"})
        err = ctx.exception
        self.assertEqual(err.status, 400)
        self.assertEqual(err.code, "validation_failed")
        self.assertTrue(err.request_id)

    def test_agents_stream_yields_a_terminal_result_frame(self) -> None:
        frames = []
        result = None
        for frame in _client().agents.stream({"manifest": HELLO_MANIFEST, "input": {"message": "Hi"}}):
            frames.append(frame["type"])
            if frame["type"] == "result":
                result = frame
        self.assertIn("start", frames)
        self.assertIn("done", frames)
        self.assertEqual(frames[-1], "result")
        self.assertIsNotNone(result)

    def test_workflows_validate_accepts_a_valid_definition_and_rejects_a_bad_one(self) -> None:
        valid = _client().workflows.validate(
            "metadata:\n  name: sdk-test\n  version: 1.0.0\nspec:\n  activities:\n    - {id: a, type: function}\n"
        )
        self.assertTrue(valid["valid"])
        self.assertEqual(valid["name"], "sdk-test")
        self.assertEqual(valid["activity_count"], 1)

        with self.assertRaises(WovyrApiError) as ctx:
            _client().workflows.validate("not a workflow")
        self.assertEqual(ctx.exception.status, 400)

    def test_workflows_submit_then_poll_to_completion(self) -> None:
        # A `function`-type activity needs a `name` naming a registered tool
        # (the server dispatches it through the ToolRegistry) — `echo` is a
        # built-in.
        manifest = (
            "metadata:\n  name: sdk-submit-test\n  version: 1.0.0\n"
            "spec:\n  activities:\n    - {id: a, type: function, name: echo, inputs: {message: hi}}\n"
        )
        submitted = _admin_client().workflows.submit({"manifest": manifest, "input": {}})
        self.assertEqual(submitted["status"], "submitted")
        execution_id = submitted["execution_id"]

        # DX-301: the poll loop every caller used to hand-roll is now the
        # SDK's `wait_for_completion` — this exercises it against the real
        # server. (RM-GA-P4 API-702: status serializes snake_case.)
        got = _admin_client().workflows.wait_for_completion(
            execution_id, interval_s=0.1, timeout_s=10.0
        )
        self.assertEqual(got["execution"].get("status"), "completed")

    def test_memory_put_then_query_round_trips_a_record(self) -> None:
        namespace = f"sdk-test-{int(time.time() * 1000)}"
        client = _admin_client()
        client.memory.put(
            {
                "namespace": namespace,
                "content": "The Wovyr Python SDK integration test wrote this record.",
                "tags": ["sdk-test"],
            }
        )
        res = client.memory.query(
            {"text": "Wovyr Python SDK integration test", "namespace": namespace, "strategy": "keyword"}
        )
        self.assertGreaterEqual(len(res["data"]), 1)
        self.assertIn("Wovyr Python SDK", res["data"][0]["content"])

    def test_secrets_create_get_rotate_delete_round_trip(self) -> None:
        name = f"sdk-test-secret-{int(time.time() * 1000)}"
        client = _admin_client()
        created = client.secrets.create(name, "s3cr3t-v1")
        self.assertEqual(created["version"], 1)
        self.assertNotIn("value", created)

        fetched = client.secrets.get(name)
        self.assertEqual(fetched["name"], name)

        rotated = client.secrets.rotate(name, "s3cr3t-v2")
        self.assertEqual(rotated["version"], 2)

        client.secrets.delete(name)
        with self.assertRaises(WovyrApiError) as ctx:
            client.secrets.get(name)
        self.assertEqual(ctx.exception.status, 404)

    def test_projects_create_with_stale_if_match_is_rejected(self) -> None:
        client = _admin_client()
        org_name = f"sdk-test-org-{int(time.time() * 1000)}"
        try:
            org = client.organizations.create(org_name)
        except WovyrApiError as err:
            if err.status == 403:
                self.skipTest("server not started with WOVYR_PLATFORM_ADMINS=sdk-test-admin")
            raise

        project, etag = client.projects.create(f"sdk-test-project-{int(time.time() * 1000)}", org["id"])
        self.assertTrue(etag)

        # First update with the correct etag succeeds and bumps the version.
        _, updated_etag = client.projects.update(project["id"], settings={"a": 1}, if_match=etag)
        self.assertNotEqual(updated_etag, etag)

        # Re-using the now-stale original etag must be rejected.
        with self.assertRaises(WovyrApiError) as ctx:
            client.projects.update(project["id"], settings={"a": 2}, if_match=etag)
        self.assertEqual(ctx.exception.status, 409)

    def test_pagination_agents_list_honors_limit(self) -> None:
        page = _admin_client().agents.list(limit=1)
        self.assertLessEqual(len(page["data"]), 1)
        self.assertIsInstance(page["has_more"], bool)

    def test_pagination_paginate_all_drains_every_stored_agent_across_pages(self) -> None:
        client = _admin_client()
        created = []
        for i in range(3):
            manifest = (
                f"apiVersion: agent.wovyr.io/v1\nkind: Agent\nmetadata:\n"
                f"  name: paginate-test-{i}-{int(time.time() * 1000)}\n"
                f"spec:\n  model_selector: {{ capability: chat, class: fast }}\n  instructions: hi\n"
            )
            created.append(client.agents.create(manifest)["id"])
        try:
            seen = set(paginate_all(client.agents.list, limit=1))
            for agent_id in created:
                self.assertIn(agent_id, seen)
        finally:
            for agent_id in created:
                try:
                    client.agents.delete(agent_id)
                except WovyrApiError:
                    pass


class _FakeHttpResponse:
    def __init__(self, status: int, body: bytes, headers: Optional[Message] = None) -> None:
        self.status = status
        self.headers = headers or Message()
        self._body = body

    def read(self) -> bytes:
        return self._body

    def close(self) -> None:
        pass

    def __iter__(self):
        return iter(self._body.splitlines(keepends=True))


class _FlakyOpener(Opener):
    """Fails the first `fail_count` calls with a 503, then succeeds."""

    def __init__(self, fail_count: int, final_status: int = 200) -> None:
        self.fail_count = fail_count
        self.final_status = final_status
        self.calls = 0

    def open(self, request: urllib.request.Request) -> Any:
        self.calls += 1
        if self.calls <= self.fail_count:
            raise urllib.error.HTTPError(
                request.full_url, 503, "Service Unavailable", Message(), io.BytesIO(b"service unavailable")
            )
        headers = Message()
        headers["content-type"] = "application/json"
        return _FakeHttpResponse(
            self.final_status, b'{"status": "ok", "version": "test"}', headers
        )


class HttpClientRetryTests(unittest.TestCase):
    def test_get_retries_a_503_and_eventually_succeeds(self) -> None:
        opener = _FlakyOpener(fail_count=2)
        client = WovyrClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        health = client.health()
        self.assertEqual(health["status"], "ok")
        self.assertEqual(opener.calls, 3)

    def test_get_gives_up_after_exhausting_retries(self) -> None:
        opener = _FlakyOpener(fail_count=5)
        client = WovyrClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        with self.assertRaises(WovyrApiError):
            client.health()
        self.assertEqual(opener.calls, 3)  # 1 initial + 2 retries, then surfaces the error

    def test_post_is_never_auto_retried_even_on_a_503(self) -> None:
        opener = _FlakyOpener(fail_count=1)
        client = WovyrClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        with self.assertRaises(WovyrApiError):
            client.agents.run({"manifest": "x"})
        self.assertEqual(opener.calls, 1)


class _RecordingFlakyOpener(_FlakyOpener):
    """A `_FlakyOpener` that also records each request's Idempotency-Key."""

    def __init__(self, fail_count: int) -> None:
        super().__init__(fail_count)
        self.keys: list[Optional[str]] = []

    def open(self, request: urllib.request.Request) -> Any:
        self.keys.append(request.get_header("Idempotency-key"))
        return super().open(request)


class IdempotentMutationRetryTests(unittest.TestCase):
    """DX-301: a mutating request retries transient failures only when it
    carries an `Idempotency-Key` — the server's replay middleware then makes
    the retry safe."""

    def test_a_keyed_mutation_retries_a_503_and_succeeds(self) -> None:
        opener = _RecordingFlakyOpener(fail_count=2)
        client = WovyrClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        client.secrets.create("token", "v1", idempotency_key="sdk-key-1")
        self.assertEqual(opener.calls, 3)
        # Every attempt carried the same key — that's what makes it safe.
        self.assertEqual(opener.keys, ["sdk-key-1"] * 3)

    def test_the_same_mutation_without_a_key_still_never_retries(self) -> None:
        opener = _RecordingFlakyOpener(fail_count=1)
        client = WovyrClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        with self.assertRaises(WovyrApiError):
            client.secrets.create("token", "v1")
        self.assertEqual(opener.calls, 1)


class _StatusSequenceOpener(Opener):
    """Serves `GET /workflows/{id}` snapshots from a scripted status list
    (the last status repeats once the script is exhausted)."""

    def __init__(self, statuses: list[str]) -> None:
        self.statuses = statuses
        self.calls = 0

    def open(self, request: urllib.request.Request) -> Any:
        status = self.statuses[min(self.calls, len(self.statuses) - 1)]
        self.calls += 1
        headers = Message()
        headers["content-type"] = "application/json"
        body = ('{"execution": {"execution_id": "wf-1", "status": "%s"}, "events": []}' % status).encode()
        return _FakeHttpResponse(200, body, headers)


class WaitForCompletionTests(unittest.TestCase):
    """DX-301: `workflows.wait_for_completion` — polls to a terminal status,
    treats `failed` as terminal, and raises `WovyrTimeoutError` on deadline."""

    def _client(self, opener: Opener) -> WovyrClient:
        return WovyrClient("http://unit-test.invalid", opener=opener)

    def test_polls_until_terminal_and_returns_the_final_snapshot(self) -> None:
        opener = _StatusSequenceOpener(["running", "running", "completed"])
        got = self._client(opener).workflows.wait_for_completion("wf-1", interval_s=0.001)
        self.assertEqual(got["execution"]["status"], "completed")
        self.assertEqual(opener.calls, 3)

    def test_failed_is_terminal_too(self) -> None:
        opener = _StatusSequenceOpener(["failed"])
        got = self._client(opener).workflows.wait_for_completion("wf-1", interval_s=0.001)
        self.assertEqual(got["execution"]["status"], "failed")

    def test_raises_wovyr_timeout_error_once_the_deadline_passes(self) -> None:
        opener = _StatusSequenceOpener(["running"])
        with self.assertRaises(WovyrTimeoutError):
            self._client(opener).workflows.wait_for_completion(
                "wf-1", interval_s=0.005, timeout_s=0.02
            )


if __name__ == "__main__":
    unittest.main()


class VersionSkewTests(unittest.TestCase):
    """DX-303: `health()` is the version handshake — a major.minor mismatch
    warns (once), agreement and unparseable dev versions stay silent."""

    def _health_opener(self, server_version: str) -> "_FlakyOpener":
        opener = _FlakyOpener(fail_count=0)

        def open_(request: urllib.request.Request) -> Any:  # noqa: ANN401
            headers = Message()
            headers["content-type"] = "application/json"
            body = ('{"status": "ok", "version": "%s"}' % server_version).encode()
            return _FakeHttpResponse(200, body, headers)

        opener.open = open_  # type: ignore[method-assign]
        return opener

    def test_matching_major_minor_is_silent(self) -> None:
        import warnings as w

        client = WovyrClient("http://unit-test.invalid", opener=self._health_opener(sdk_version()))
        with w.catch_warnings():
            w.simplefilter("error", WovyrVersionSkewWarning)
            client.health()  # would raise if it warned

    def test_minor_skew_warns_once_per_client(self) -> None:
        client = WovyrClient("http://unit-test.invalid", opener=self._health_opener("0.99.0"))
        with self.assertWarns(WovyrVersionSkewWarning):
            client.health()
        import warnings as w

        with w.catch_warnings():
            w.simplefilter("error", WovyrVersionSkewWarning)
            client.health()  # second call: already warned, stays silent

    def test_unparseable_dev_version_is_silent(self) -> None:
        import warnings as w

        client = WovyrClient("http://unit-test.invalid", opener=self._health_opener("dry-run"))
        with w.catch_warnings():
            w.simplefilter("error", WovyrVersionSkewWarning)
            client.health()


class VersionLockstepTests(unittest.TestCase):
    """The source-tree fallback version must match `pyproject.toml` — the
    mechanical guard DX-303 relies on instead of a manual convention."""

    def test_fallback_version_matches_pyproject(self) -> None:
        import pathlib
        import re

        from wovyr_sdk.version import _FALLBACK_VERSION

        pyproject = (pathlib.Path(__file__).parent.parent / "pyproject.toml").read_text()
        m = re.search(r'^version = "([^"]+)"', pyproject, re.M)
        assert m is not None
        self.assertEqual(_FALLBACK_VERSION, m.group(1))
