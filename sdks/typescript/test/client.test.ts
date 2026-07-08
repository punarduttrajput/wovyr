/** Integration tests against a real, locally running `apex dev` server
 * (`cargo run -p apex-cli -- dev --addr 127.0.0.1:8080`) — not mocked. Run
 * with the server already up:
 *
 * ```bash
 * npm run build && node --test dist/test/
 * ```
 *
 * Skips cleanly (logging, not failing) if no server answers at
 * `APEX_TEST_BASE_URL` (default `http://127.0.0.1:8080`), so this suite
 * doesn't fail an offline CI run that never started one.
 *
 * The org/project test additionally needs the server started with
 * `APEX_PLATFORM_ADMINS=sdk-test-admin` (unlike agents/workflows/memory,
 * tenancy management routes have no anonymous-default-tenant back-compat
 * bypass — see `adminClient()` below); it skips gracefully, rather than
 * failing, if that wasn't configured.
 */
import assert from "node:assert/strict";
import { before, describe, test } from "node:test";
import { ApexClient, ApexApiError, paginateAll } from "../src/index.js";

const baseUrl = process.env.APEX_TEST_BASE_URL ?? "http://127.0.0.1:8080";

let serverAvailable = false;

before(async () => {
  try {
    const res = await fetch(`${baseUrl}/healthz`);
    serverAvailable = res.ok;
  } catch {
    serverAvailable = false;
  }
  if (!serverAvailable) {
    console.warn(`skipping integration tests: no apex-server reachable at ${baseUrl}`);
  }
});

function client(): ApexClient {
  return new ApexClient({ baseUrl });
}

/** Org/project management routes (unlike agents/workflows/memory) have no
 * anonymous-default-tenant back-compat bypass — they need a real
 * `org.admin`/`platform.admin` role. The test server is started with
 * `APEX_PLATFORM_ADMINS=sdk-test-admin` (see the `test` npm script) so this
 * principal is always platform admin. */
function adminClient(): ApexClient {
  return new ApexClient({ baseUrl, principal: "sdk-test-admin" });
}

const HELLO_MANIFEST = `
apiVersion: agent.apex.io/v1
kind: Agent
metadata:
  name: hello
spec:
  model_selector: { capability: chat, class: fast }
  instructions: |
    You are a friendly assistant. Greet the user and answer briefly.
`;

describe("ApexClient (integration)", () => {
  test("health() reports ok", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const health = await client().health();
    assert.equal(health.status, "ok");
    assert.ok(health.version.length > 0);
  });

  test("tools.list() includes the built-in echo tool", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const { data, total_estimate } = await client().tools.list();
    // Default hosted registry (SEC-301): echo, fs_read, http_get — shell,
    // image_generate, and any plugin tools are each conditional opt-ins a
    // clean environment won't have, so 3 is the true floor, not 4.
    assert.ok(total_estimate >= 3);
    assert.ok(data.some((tool) => tool.id === "echo"));
  });

  test("agents.run() runs an inline manifest end to end", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const result = await client().agents.run({
      manifest: HELLO_MANIFEST,
      input: { message: "Hi" },
    });
    assert.equal(result.status, "succeeded");
    assert.ok(result.output.message.length > 0);
    assert.ok(result.steps >= 1);
  });

  test("agents.run() with a malformed manifest throws ApexApiError(400)", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    await assert.rejects(
      () => client().agents.run({ manifest: "not: [valid, agent" }),
      (err: unknown) => {
        assert.ok(err instanceof ApexApiError);
        assert.equal(err.status, 400);
        assert.equal(err.code, "validation_failed");
        assert.ok(err.requestId && err.requestId.length > 0);
        return true;
      },
    );
  });

  test("agents.stream() yields a terminal result frame", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const frames: string[] = [];
    let result: unknown;
    for await (const frame of client().agents.stream({
      manifest: HELLO_MANIFEST,
      input: { message: "Hi" },
    })) {
      frames.push(frame.type);
      if (frame.type === "result") result = frame;
    }
    assert.ok(frames.includes("start"));
    assert.ok(frames.includes("done"));
    assert.equal(frames.at(-1), "result");
    assert.ok(result);
  });

  test("workflows.validate() accepts a valid definition and rejects a bad one", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const valid = await client().workflows.validate(
      "metadata:\n  name: sdk-test\n  version: 1.0.0\nspec:\n  activities:\n    - {id: a, type: function}\n",
    );
    assert.equal(valid.valid, true);
    assert.equal(valid.name, "sdk-test");
    assert.equal(valid.activity_count, 1);

    await assert.rejects(
      () => client().workflows.validate("not a workflow"),
      (err: unknown) => err instanceof ApexApiError && err.status === 400,
    );
  });

  test("workflows: submit then poll to completion", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    // A `function`-type activity needs a `name` naming a registered tool (the
    // server dispatches it through the ToolRegistry) — `echo` is a built-in.
    const manifest =
      "metadata:\n  name: sdk-submit-test\n  version: 1.0.0\nspec:\n  activities:\n    - {id: a, type: function, name: echo, inputs: {message: hi}}\n";
    const { execution_id, status } = await client().workflows.submit({ manifest, input: {} });
    assert.equal(status, "submitted");

    let completed = false;
    for (let i = 0; i < 20; i++) {
      const { execution } = await client().workflows.get(execution_id);
      // RM-GA-P4 API-702: WorkflowState now serializes snake_case ("completed",
      // "failed"), matching the `?status=` filter's casing (previously
      // PascalCase — a wart API-702 fixed by construction).
      if ((execution as { status?: string }).status === "completed") {
        completed = true;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.ok(completed, "workflow should reach completed status within the poll window");
  });

  test("memory: put then query round-trips a record", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const namespace = `sdk-test-${Date.now()}`;
    await client().memory.put({
      namespace,
      content: "The Apex TypeScript SDK integration test wrote this record.",
      tags: ["sdk-test"],
    });
    const { data } = await client().memory.query({
      text: "Apex TypeScript SDK integration test",
      namespace,
      strategy: "keyword",
    });
    assert.ok(data.length >= 1);
    assert.match(data[0]!.content, /Apex TypeScript SDK/);
  });

  test("secrets: create, get, rotate, delete round-trip (value never returned)", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const name = `sdk-test-secret-${Date.now()}`;
    const c = client();
    const created = await c.secrets.create(name, "s3cr3t-v1");
    assert.equal(created.version, 1);
    assert.equal((created as Record<string, unknown>).value, undefined);

    const fetched = await c.secrets.get(name);
    assert.equal(fetched.name, name);

    const rotated = await c.secrets.rotate(name, "s3cr3t-v2");
    assert.equal(rotated.version, 2);

    await c.secrets.delete(name);
    await assert.rejects(
      () => c.secrets.get(name),
      (err: unknown) => err instanceof ApexApiError && err.status === 404,
    );
  });

  test("projects: create with a stale If-Match is rejected 409", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const orgName = `sdk-test-org-${Date.now()}`;
    let org: { id: string };
    try {
      org = (await c.organizations.create(orgName)) as { id: string };
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }

    const { project, etag } = await c.projects.create(`sdk-test-project-${Date.now()}`, org.id);
    assert.ok(etag);

    // First update with the correct etag succeeds and bumps the version.
    const updated = await c.projects.update(project.id as string, { settings: { a: 1 } }, etag);
    assert.notEqual(updated.etag, etag);

    // Re-using the now-stale original etag must be rejected.
    await assert.rejects(
      () => c.projects.update(project.id as string, { settings: { a: 2 } }, etag),
      (err: unknown) => err instanceof ApexApiError && err.status === 409,
    );
  });

  test("pagination: agents.list() honors limit", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const page = await client().agents.list({ limit: 1 });
    assert.ok(page.data.length <= 1);
    assert.equal(typeof page.has_more, "boolean");
  });

  test("pagination: paginateAll() drains every stored agent across pages", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = client();
    const created: string[] = [];
    for (let i = 0; i < 3; i++) {
      const { id } = await c.agents.create(
        `apiVersion: agent.apex.io/v1\nkind: Agent\nmetadata:\n  name: paginate-test-${i}-${Date.now()}\nspec:\n  model_selector: { capability: chat, class: fast }\n  instructions: hi\n`,
      );
      created.push(id);
    }
    try {
      const seen = new Set<string>();
      for await (const id of paginateAll((params) => c.agents.list(params), { limit: 1 })) {
        seen.add(id);
      }
      for (const id of created) assert.ok(seen.has(id), `expected paginateAll to surface ${id}`);
    } finally {
      await Promise.all(created.map((id) => c.agents.delete(id).catch(() => {})));
    }
  });
});

describe("HttpClient retry (unit, mocked fetch)", () => {
  function flakyFetch(failCount: number, finalStatus = 200) {
    let calls = 0;
    const fetchImpl = (async () => {
      calls++;
      if (calls <= failCount) {
        return new Response("service unavailable", { status: 503 });
      }
      return new Response(JSON.stringify({ status: "ok", version: "test" }), {
        status: finalStatus,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;
    return { fetchImpl, callCount: () => calls };
  }

  test("GET retries a 503 and eventually succeeds", async () => {
    const { fetchImpl, callCount } = flakyFetch(2);
    const c = new ApexClient({
      baseUrl: "http://unit-test.invalid",
      fetchImpl,
      retry: { maxRetries: 2, baseDelayMs: 1 },
    });
    const health = await c.health();
    assert.equal(health.status, "ok");
    assert.equal(callCount(), 3);
  });

  test("GET gives up after exhausting retries", async () => {
    const { fetchImpl, callCount } = flakyFetch(5);
    const c = new ApexClient({
      baseUrl: "http://unit-test.invalid",
      fetchImpl,
      retry: { maxRetries: 2, baseDelayMs: 1 },
    });
    await assert.rejects(() => c.health(), ApexApiError);
    assert.equal(callCount(), 3); // 1 initial + 2 retries, then surfaces the error
  });

  test("POST is never auto-retried, even on a 503", async () => {
    const { fetchImpl, callCount } = flakyFetch(1);
    const c = new ApexClient({
      baseUrl: "http://unit-test.invalid",
      fetchImpl,
      retry: { maxRetries: 2, baseDelayMs: 1 },
    });
    await assert.rejects(() => c.agents.run({ manifest: "x" }), ApexApiError);
    assert.equal(callCount(), 1);
  });
});
