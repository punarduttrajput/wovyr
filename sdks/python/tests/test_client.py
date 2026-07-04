"""Integration tests against a real, locally running `apex dev` server
(`cargo run -p apex-cli -- dev --addr 127.0.0.1:8080`) — not mocked. Run with
the server already up:

    python3 -m unittest discover -s tests -v

Skips cleanly (not failing) if no server answers at `APEX_TEST_BASE_URL`
(default `http://127.0.0.1:8080`), so this suite doesn't fail an offline run
that never started one.

The org/project test additionally needs the server started with
`APEX_PLATFORM_ADMINS=sdk-test-admin` (unlike agents/workflows/memory,
tenancy management routes have no anonymous-default-tenant back-compat
bypass — see `_admin_client()` below); it skips gracefully, rather than
failing, if that wasn't configured.
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

from apex_sdk import ApexApiError, ApexClient, RetryOptions, paginate_all
from apex_sdk.http import Opener

BASE_URL = os.environ.get("APEX_TEST_BASE_URL", "http://127.0.0.1:8080")

HELLO_MANIFEST = """
apiVersion: agent.apex.io/v1
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


def _client() -> ApexClient:
    return ApexClient(BASE_URL)


def _admin_client() -> ApexClient:
    """Org/project management routes need a real `org.admin`/`platform.admin`
    role. The test server is expected to run with
    `APEX_PLATFORM_ADMINS=sdk-test-admin`, making this principal a platform
    admin."""
    return ApexClient(BASE_URL, principal="sdk-test-admin")


@unittest.skipUnless(_server_available(), f"no apex-server reachable at {BASE_URL}")
class ApexClientIntegrationTests(unittest.TestCase):
    def test_health_reports_ok(self) -> None:
        health = _client().health()
        self.assertEqual(health["status"], "ok")
        self.assertGreater(len(health["version"]), 0)

    def test_tools_list_includes_the_built_in_echo_tool(self) -> None:
        res = _client().tools.list()
        self.assertGreaterEqual(res["total"], 4)
        self.assertTrue(any(tool["id"] == "echo" for tool in res["tools"]))

    def test_agents_run_runs_an_inline_manifest_end_to_end(self) -> None:
        result = _client().agents.run({"manifest": HELLO_MANIFEST, "input": {"message": "Hi"}})
        self.assertEqual(result["status"], "succeeded")
        self.assertGreater(len(result["output"]["message"]), 0)
        self.assertGreaterEqual(result["steps"], 1)

    def test_agents_run_with_a_malformed_manifest_raises_apex_api_error_400(self) -> None:
        with self.assertRaises(ApexApiError) as ctx:
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

        with self.assertRaises(ApexApiError) as ctx:
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
        submitted = _client().workflows.submit({"manifest": manifest, "input": {}})
        self.assertEqual(submitted["status"], "submitted")
        execution_id = submitted["execution_id"]

        completed = False
        for _ in range(20):
            got = _client().workflows.get(execution_id)
            # The engine's WorkflowState serializes PascalCase ("Completed",
            # "Failed") — distinct from the lowercase `?status=` filter values
            # the list endpoint accepts.
            if got["execution"].get("status") == "Completed":
                completed = True
                break
            time.sleep(0.1)
        self.assertTrue(completed, "workflow should reach completed status within the poll window")

    def test_memory_put_then_query_round_trips_a_record(self) -> None:
        namespace = f"sdk-test-{int(time.time() * 1000)}"
        client = _client()
        client.memory.put(
            {
                "namespace": namespace,
                "content": "The Apex Python SDK integration test wrote this record.",
                "tags": ["sdk-test"],
            }
        )
        res = client.memory.query(
            {"text": "Apex Python SDK integration test", "namespace": namespace, "strategy": "keyword"}
        )
        self.assertGreaterEqual(len(res["results"]), 1)
        self.assertIn("Apex Python SDK", res["results"][0]["content"])

    def test_secrets_create_get_rotate_delete_round_trip(self) -> None:
        name = f"sdk-test-secret-{int(time.time() * 1000)}"
        client = _client()
        created = client.secrets.create(name, "s3cr3t-v1")
        self.assertEqual(created["version"], 1)
        self.assertNotIn("value", created)

        fetched = client.secrets.get(name)
        self.assertEqual(fetched["name"], name)

        rotated = client.secrets.rotate(name, "s3cr3t-v2")
        self.assertEqual(rotated["version"], 2)

        client.secrets.delete(name)
        with self.assertRaises(ApexApiError) as ctx:
            client.secrets.get(name)
        self.assertEqual(ctx.exception.status, 404)

    def test_projects_create_with_stale_if_match_is_rejected(self) -> None:
        client = _admin_client()
        org_name = f"sdk-test-org-{int(time.time() * 1000)}"
        try:
            org = client.organizations.create(org_name)
        except ApexApiError as err:
            if err.status == 403:
                self.skipTest("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin")
            raise

        project, etag = client.projects.create(f"sdk-test-project-{int(time.time() * 1000)}", org["id"])
        self.assertTrue(etag)

        # First update with the correct etag succeeds and bumps the version.
        _, updated_etag = client.projects.update(project["id"], settings={"a": 1}, if_match=etag)
        self.assertNotEqual(updated_etag, etag)

        # Re-using the now-stale original etag must be rejected.
        with self.assertRaises(ApexApiError) as ctx:
            client.projects.update(project["id"], settings={"a": 2}, if_match=etag)
        self.assertEqual(ctx.exception.status, 409)

    def test_pagination_agents_list_honors_limit(self) -> None:
        page = _client().agents.list(limit=1)
        self.assertLessEqual(len(page["data"]), 1)
        self.assertIsInstance(page["has_more"], bool)

    def test_pagination_paginate_all_drains_every_stored_agent_across_pages(self) -> None:
        client = _client()
        created = []
        for i in range(3):
            manifest = (
                f"apiVersion: agent.apex.io/v1\nkind: Agent\nmetadata:\n"
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
                except ApexApiError:
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
        client = ApexClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        health = client.health()
        self.assertEqual(health["status"], "ok")
        self.assertEqual(opener.calls, 3)

    def test_get_gives_up_after_exhausting_retries(self) -> None:
        opener = _FlakyOpener(fail_count=5)
        client = ApexClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        with self.assertRaises(ApexApiError):
            client.health()
        self.assertEqual(opener.calls, 3)  # 1 initial + 2 retries, then surfaces the error

    def test_post_is_never_auto_retried_even_on_a_503(self) -> None:
        opener = _FlakyOpener(fail_count=1)
        client = ApexClient(
            "http://unit-test.invalid", opener=opener, retry=RetryOptions(max_retries=2, base_delay_s=0.001)
        )
        with self.assertRaises(ApexApiError):
            client.agents.run({"manifest": "x"})
        self.assertEqual(opener.calls, 1)


if __name__ == "__main__":
    unittest.main()
