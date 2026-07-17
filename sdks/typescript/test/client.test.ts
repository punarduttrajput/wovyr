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
 * **Almost every mutating/tenant-scoped route needs
 * `APEX_PLATFORM_ADMINS=sdk-test-admin`** (SEC-105: nothing tenant-scoped is
 * reachable "for free" via anonymity alone — see
 * `crates/apex-server/src/tenancy.rs`'s `tenant_authorize` doc comment).
 * Concretely: `health()` and `tools.list()` are the only two tests in this
 * file that work fully anonymously; `workflows.validate()` (parse-only, no
 * side effects) also needs no credential. Every other test below —
 * `agents.run()`/`agents.stream()` (these two apparently tolerate an
 * anonymous caller for the run-only path, unlike listing/managing stored
 * agents), `workflows: submit then poll`, `memory:`, `secrets:`,
 * `projects:`, both `pagination:` tests, and the whole `ui:` suite — uses
 * `adminClient()` and skips gracefully (not a hard failure) if the server
 * wasn't started with that principal granted. Start the server with:
 *
 * ```bash
 * APEX_PLATFORM_ADMINS=sdk-test-admin APEX_ALLOW_ANONYMOUS=1 \
 *   cargo run -p apex-cli -- dev
 * ```
 *
 * The `ui:` suite's approve test additionally needs
 * `APEX_UI_POLICY=examples/policies/default-ui-policy.yaml` (GRD-207: absent
 * a policy, the hosted floor denies *every* interactive frame, including a
 * legitimate one). The block test needs no such setup: the hosted floor
 * already denies its frame the same way a real policy's sensitive-input
 * rule would.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { before, describe, test } from "node:test";
import { ApexClient, ApexApiError, ApexTimeoutError, SDK_VERSION, paginateAll, type PendingUiFrame } from "../src/index.js";

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
    const c = adminClient();
    // A `function`-type activity needs a `name` naming a registered tool (the
    // server dispatches it through the ToolRegistry) — `echo` is a built-in.
    const manifest =
      "metadata:\n  name: sdk-submit-test\n  version: 1.0.0\nspec:\n  activities:\n    - {id: a, type: function, name: echo, inputs: {message: hi}}\n";
    // An explicit, randomized execution id (matching every other test's own
    // convention below) rather than relying on the server's auto-incrementing
    // counter: that counter resets to 1 on every server boot, but the durable
    // `~/.apex/workflows` store persists forever — across repeated local `dev`
    // restarts, an un-randomized id collides with a prior run's real on-disk
    // event history and can surface as a stale-data deserialization error,
    // not a bug in this test or the route it exercises.
    let execution_id: string;
    let status: string;
    try {
      ({ execution_id, status } = await c.workflows.submit({
        manifest,
        input: {},
        execution_id: `sdk-submit-test-${Date.now()}`,
      }));
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }
    assert.equal(status, "submitted");

    // DX-301: the poll loop every caller used to hand-roll is now the SDK's
    // `waitForCompletion` — this exercises it against the real server.
    const { execution } = await c.workflows.waitForCompletion(execution_id, {
      intervalMs: 100,
      timeoutMs: 10_000,
    });
    // RM-GA-P4 API-702: WorkflowState serializes snake_case ("completed",
    // "failed"), matching the `?status=` filter's casing.
    assert.equal((execution as { status?: string }).status, "completed");
  });

  test("memory: put then query round-trips a record", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const namespace = `sdk-test-${Date.now()}`;
    try {
      await c.memory.put({
        namespace,
        content: "The Apex TypeScript SDK integration test wrote this record.",
        tags: ["sdk-test"],
      });
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }
    const { data } = await c.memory.query({
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
    const c = adminClient();
    let created: Awaited<ReturnType<typeof c.secrets.create>>;
    try {
      created = await c.secrets.create(name, "s3cr3t-v1");
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }
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
    let page: Awaited<ReturnType<ApexClient["agents"]["list"]>>;
    try {
      page = await adminClient().agents.list({ limit: 1 });
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }
    assert.ok(page.data.length <= 1);
    assert.equal(typeof page.has_more, "boolean");
  });

  test("pagination: paginateAll() drains every stored agent across pages", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const created: string[] = [];
    try {
      for (let i = 0; i < 3; i++) {
        const { id } = await c.agents.create(
          `apiVersion: agent.apex.io/v1\nkind: Agent\nmetadata:\n  name: paginate-test-${i}-${Date.now()}\nspec:\n  model_selector: { capability: chat, class: fast }\n  instructions: hi\n`,
        );
        created.push(id);
      }
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
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

describe("ui: generative-UI frames + decisions (PRD-005 RM-GUI-P1)", () => {
  // GRD-207: with no ui policy configured, the hosted floor denies *every*
  // interactive frame — including this legitimate approve flow. Point the
  // server at examples/policies/default-ui-policy.yaml (`APEX_UI_POLICY=...`)
  // to exercise the real policy path (allows this frame; blocks the
  // credential one below via the sensitive-input rule) rather than the floor.
  const APPROVE_MANIFEST =
    "metadata:\n  name: sdk-ui-approve-test\nspec:\n  activities:\n    - id: confirm\n      type: ui\n      inputs:\n        frame:\n          schema_version: 1.0.0\n          title: Confirm order\n          root:\n            type: column\n            children:\n              - {type: text, text: Reorder 3 boxes?}\n              - {type: text_input, name: po_number, label: PO number, required: true}\n              - type: row\n                children:\n                  - {type: button, action: approve, label: Approve, class: approve}\n                  - {type: button, action: cancel, label: Cancel, class: cancel}\n";

  const BLOCK_MANIFEST =
    "metadata:\n  name: sdk-ui-block-test\nspec:\n  activities:\n    - id: confirm\n      type: ui\n      inputs:\n        frame:\n          schema_version: 1.0.0\n          root:\n            type: column\n            children:\n              - {type: text_input, name: card_number, label: Card number}\n              - {type: button, action: pay, label: Continue, class: submit}\n";

  /** Polls until either a pending frame for `executionId` appears, or the
   * execution reaches a terminal status first — distinguishing "the frame is
   * on its way" from "policy denied it" (the hosted floor, absent
   * `APEX_UI_POLICY`) without a fixed race between the two. */
  async function waitForPendingFrameOrTerminal(
    c: ApexClient,
    executionId: string,
  ): Promise<{ frame: PendingUiFrame } | { terminalStatus: string }> {
    for (let i = 0; i < 100; i++) {
      const { data } = await c.ui.list();
      const found = data.find((f) => f.execution_id === executionId);
      if (found) return { frame: found };
      const { execution } = await c.workflows.get(executionId);
      const status = (execution as { status?: string }).status ?? "";
      if (!["created", "validated", "scheduled", "running", "resumed"].includes(status)) {
        return { terminalStatus: status };
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(`execution ${executionId} settled into neither a pending frame nor a terminal status`);
  }

  async function waitForWorkflowStatus(c: ApexClient, executionId: string, status: string): Promise<void> {
    for (let i = 0; i < 100; i++) {
      const { execution } = await c.workflows.get(executionId);
      if ((execution as { status?: string }).status === status) return;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(`execution ${executionId} did not reach ${status} in time`);
  }

  // `workflows:run` (SEC-105) has no anonymous-default-tenant back-compat
  // bypass, same as organizations/projects — these need a real platform-admin
  // credential (`adminClient()`), and skip gracefully without one.
  test("UC1: a pending frame renders, an out-of-vocabulary decision 400s, a valid approval resumes the workflow", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const executionId = `sdk-ui-approve-${Date.now()}`;
    let submitStatus: string;
    try {
      ({ status: submitStatus } = await c.workflows.submit({
        manifest: APPROVE_MANIFEST,
        execution_id: executionId,
      }));
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }
    assert.equal(submitStatus, "submitted");

    const outcome = await waitForPendingFrameOrTerminal(c, executionId);
    if ("terminalStatus" in outcome) {
      return t.skip(
        "no ui policy configured (server started without APEX_UI_POLICY) — the hosted " +
          "floor denies this legitimate frame too; see examples/policies/default-ui-policy.yaml",
      );
    }
    const pending = outcome.frame;
    assert.equal(pending.activity_id, "confirm");
    assert.ok(pending.frame_hash.length > 0);

    // The client can fetch the same frame by id (RDR-104's pull path).
    const fetched = await c.ui.get(pending.frame_id);
    assert.equal(fetched.frame_hash, pending.frame_hash);

    // HIL-302, fail-closed at the boundary: an undeclared action never
    // reaches the workflow.
    await assert.rejects(
      () => c.ui.decide(pending.frame_id, { action: "launch" }),
      (err: unknown) => err instanceof ApexApiError && err.status === 400,
    );

    // The valid approval resumes the execution to completion.
    const decided = await c.ui.decide(pending.frame_id, {
      action: "approve",
      values: { po_number: "PO-9" },
    });
    assert.equal(decided.status, "decided");
    await waitForWorkflowStatus(c, executionId, "completed");
  });

  test("UC4: a credential-harvesting frame is blocked and never appears as a pending frame", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const executionId = `sdk-ui-block-${Date.now()}`;
    try {
      await c.workflows.submit({ manifest: BLOCK_MANIFEST, execution_id: executionId });
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      throw err;
    }

    await waitForWorkflowStatus(c, executionId, "failed");
    const { data } = await c.ui.list();
    assert.ok(
      !data.some((f) => f.execution_id === executionId),
      "a blocked frame must never become visible on the pending-frames surface",
    );
  });

  test("deciding against an unknown frame id is a 404", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    await assert.rejects(
      () => adminClient().ui.decide("uif-does-not-exist", { action: "approve" }),
      (err: unknown) => {
        if (err instanceof ApexApiError && err.status === 403) {
          t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
          return true;
        }
        return err instanceof ApexApiError && err.status === 404;
      },
    );
  });

  // RM-GUI-P3 EMB-701: the standalone-middleware claim, proven from the SDK
  // side too — present a frame with zero workflow/agent involvement, decide
  // it, and retrieve the recorded outcome afterward.
  test("EMB-701: present/decide/getDecision works with no workflow at all", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const frame = {
      schema_version: "1.0.0",
      title: "Approve refund",
      root: {
        type: "column",
        children: [
          { type: "text", text: "Refund $42.00?" },
          { type: "button", action: "approve", label: "Approve", class: "approve" },
        ],
      },
    };
    let pending: Awaited<ReturnType<typeof c.ui.present>>;
    try {
      pending = await c.ui.present(frame);
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403 && err.code === "forbidden") {
        return t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
      }
      if (err instanceof ApexApiError && err.code === "blocked") {
        return t.skip(
          "no ui policy configured — see examples/policies/default-ui-policy.yaml",
        );
      }
      throw err;
    }
    assert.equal(pending.execution_id, null);
    assert.equal(pending.activity_id, null);

    await assert.rejects(
      () => c.ui.getDecision(pending.frame_id),
      (err: unknown) => err instanceof ApexApiError && err.status === 404,
    );

    const decided = await c.ui.decide(pending.frame_id, { action: "approve" });
    assert.equal(decided.status, "decided");
    assert.equal(decided.execution_id, null);

    const outcome = await c.ui.getDecision(pending.frame_id);
    assert.equal(outcome.action, "approve");
    assert.equal(outcome.frame_hash, pending.frame_hash);
  });
});

// PRD-006 / RM-MCX: persisted MCP connection management. The stdio lifecycle
// test additionally needs the server started with `APEX_ENABLE_MCP_STDIO=1`
// (ADR-0012's operator opt-in) and skips gracefully without it, the same way
// the `ui:` suite skips without a configured policy.
describe("mcp:", () => {
  const stdioEchoScript = `
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.method === 'initialize') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { serverInfo: { name: 'x', version: '1' } } }) + '\\n');
  } else if (msg.method === 'tools/list') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { tools: [{ name: 'echo', description: 'echoes' }] } }) + '\\n');
  }
});
`;

  test("an http connection pointed at a private address is refused (SEC-304 SSRF reuse)", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    await assert.rejects(
      () =>
        adminClient().mcp.create({
          name: "sdk-test-internal",
          transport: { kind: "http", url: "http://10.1.2.3:9/mcp" },
        }),
      (err: unknown) => {
        if (err instanceof ApexApiError && err.status === 403) {
          t.skip("server not started with APEX_PLATFORM_ADMINS=sdk-test-admin");
          return true;
        }
        return err instanceof ApexApiError && err.status === 502;
      },
    );
  });

  test("full lifecycle over a real stdio connection, and its tool shows up in tools.list()", async (t) => {
    if (!serverAvailable) return t.skip("no server");
    const c = adminClient();
    const name = `sdk-test-stdio-${Date.now()}`;
    let created: Awaited<ReturnType<typeof c.mcp.create>>;
    try {
      created = await c.mcp.create({
        name,
        transport: { kind: "stdio", command: "node", args: ["-e", stdioEchoScript] },
      });
    } catch (err) {
      if (err instanceof ApexApiError && err.status === 403) {
        return t.skip(
          "server not started with mcp:admin granted + APEX_ENABLE_MCP_STDIO=1",
        );
      }
      throw err;
    }
    assert.equal(created.name, name);
    assert.equal(created.tools[0]?.name, "echo");

    const listed = await c.mcp.list();
    assert.ok(listed.data.some((conn) => conn.name === name));

    const fetched = await c.mcp.get(name);
    assert.equal(fetched.name, name);
    assert.deepEqual(fetched.transport, { kind: "stdio", command: "node", args: ["-e", stdioEchoScript] });

    const refreshed = await c.mcp.refresh(name);
    assert.equal(refreshed.name, name);
    assert.equal(refreshed.tools[0]?.name, "echo");

    // MCX-202: the connection's tool now shows up alongside built-ins.
    const tools = await c.tools.list();
    assert.ok(tools.data.some((tool) => tool.id === `mcp__${name}__echo`));

    await c.mcp.delete(name);
    await assert.rejects(
      () => c.mcp.get(name),
      (err: unknown) => err instanceof ApexApiError && err.status === 404,
    );
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

describe("DX-301: idempotency-keyed mutation retry (unit, mocked fetch)", () => {
  function flakySubmit(failCount: number) {
    let calls = 0;
    const keys: Array<string | null> = [];
    const fetchImpl = (async (_url: unknown, init?: RequestInit) => {
      calls++;
      keys.push(new Headers(init?.headers).get("Idempotency-Key"));
      if (calls <= failCount) {
        return new Response("bad gateway", { status: 502 });
      }
      return new Response(JSON.stringify({ execution_id: "wf-1", status: "submitted" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;
    return { fetchImpl, callCount: () => calls, keys };
  }

  test("a keyed submit retries a 502 and succeeds", async () => {
    const { fetchImpl, callCount, keys } = flakySubmit(2);
    const c = new ApexClient({
      baseUrl: "http://unit-test.invalid",
      fetchImpl,
      retry: { maxRetries: 2, baseDelayMs: 1 },
    });
    const res = await c.workflows.submit(
      { manifest: "m", input: {} },
      { idempotencyKey: "sdk-key-1" },
    );
    assert.equal(res.status, "submitted");
    assert.equal(callCount(), 3);
    // Every attempt carried the same key — that's what makes the retry safe
    // (the server's replay middleware collapses duplicates).
    assert.deepEqual(keys, ["sdk-key-1", "sdk-key-1", "sdk-key-1"]);
  });

  test("the same submit without a key still never retries", async () => {
    const { fetchImpl, callCount } = flakySubmit(1);
    const c = new ApexClient({
      baseUrl: "http://unit-test.invalid",
      fetchImpl,
      retry: { maxRetries: 2, baseDelayMs: 1 },
    });
    await assert.rejects(() => c.workflows.submit({ manifest: "m" }), ApexApiError);
    assert.equal(callCount(), 1);
  });
});

describe("DX-301: workflows.waitForCompletion (unit, mocked fetch)", () => {
  function statusSequence(statuses: string[]) {
    let calls = 0;
    const fetchImpl = (async () => {
      const status = statuses[Math.min(calls, statuses.length - 1)];
      calls++;
      return new Response(JSON.stringify({ execution: { execution_id: "wf-1", status }, events: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;
    return { fetchImpl, callCount: () => calls };
  }

  test("polls until terminal and returns the final snapshot", async () => {
    const { fetchImpl, callCount } = statusSequence(["running", "running", "completed"]);
    const c = new ApexClient({ baseUrl: "http://unit-test.invalid", fetchImpl });
    const { execution } = await c.workflows.waitForCompletion("wf-1", { intervalMs: 1 });
    assert.equal(execution["status"], "completed");
    assert.equal(callCount(), 3);
  });

  test("failed is terminal too — the helper returns it rather than spinning", async () => {
    const { fetchImpl } = statusSequence(["failed"]);
    const c = new ApexClient({ baseUrl: "http://unit-test.invalid", fetchImpl });
    const { execution } = await c.workflows.waitForCompletion("wf-1", { intervalMs: 1 });
    assert.equal(execution["status"], "failed");
  });

  test("throws ApexTimeoutError once the deadline passes", async () => {
    const { fetchImpl } = statusSequence(["running"]);
    const c = new ApexClient({ baseUrl: "http://unit-test.invalid", fetchImpl });
    await assert.rejects(
      () => c.workflows.waitForCompletion("wf-1", { intervalMs: 5, timeoutMs: 20 }),
      ApexTimeoutError,
    );
  });
});

describe("DX-303: version handshake (unit, mocked fetch)", () => {
  function healthFetch(serverVersion: string) {
    return (async () =>
      new Response(JSON.stringify({ status: "ok", version: serverVersion }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;
  }

  test("SDK_VERSION stays in lockstep with package.json", async () => {
    const pkg = JSON.parse(
      await readFile(new URL("../../package.json", import.meta.url), "utf8"),
    ) as { version: string };
    assert.equal(SDK_VERSION, pkg.version);
  });

  test("matching major.minor is silent; a skew warns once per client", async (t) => {
    const warnings: string[] = [];
    t.mock.method(console, "warn", (msg: string) => warnings.push(msg));

    const same = new ApexClient({ baseUrl: "http://u.invalid", fetchImpl: healthFetch(SDK_VERSION) });
    await same.health();
    assert.equal(warnings.length, 0);

    const skewed = new ApexClient({ baseUrl: "http://u.invalid", fetchImpl: healthFetch("0.99.0") });
    await skewed.health();
    await skewed.health(); // once per client, not per call
    assert.equal(warnings.length, 1);
    assert.match(warnings[0], /0\.99\.0/);
  });

  test("an unparseable dev version stays silent", async (t) => {
    const warnings: string[] = [];
    t.mock.method(console, "warn", (msg: string) => warnings.push(msg));
    const c = new ApexClient({ baseUrl: "http://u.invalid", fetchImpl: healthFetch("dry-run") });
    await c.health();
    assert.equal(warnings.length, 0);
  });
});
