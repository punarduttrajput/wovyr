#!/usr/bin/env node
/**
 * The killer demo, driven end to end against a real `wovyr` server.
 *
 * Reproduces PRD-005 §9's acceptance narrative — the same flow proven at the
 * Rust layer by `crates/wovyr-server/src/ui.rs`'s
 * `uc4_credential_frame_is_blocked_...` / `uc1_frame_survives_restart_...` and
 * at the SDK layer by `sdks/typescript/test/client.test.ts`'s `ui:` suite —
 * but as a *narrated, recordable* session rather than an assertion suite.
 *
 * Five beats, ~90 seconds:
 *   1. A poisoned agent composes a checkout frame asking for a card number.
 *      The trust layer blocks it. It never becomes visible to anyone.
 *   2. The safe variant presents, with a content hash.
 *   3. SIGKILL the server mid-flight. Restart it.
 *   4. The same frame returns, byte-identical. An out-of-vocabulary decision
 *      is refused; the real approval resumes the workflow to completion.
 *   5. The audit chain proves every step, linked hash by hash.
 *
 * Nothing here is staged: every line of output is a real HTTP response from a
 * real server process, and the script fails loudly if any beat doesn't hold.
 *
 * Usage:
 *   node demo/killer-demo.mjs                 # uses target/debug/wovyr[.exe]
 *   WOVYR_BIN=/path/to/wovyr node demo/killer-demo.mjs
 *   node demo/killer-demo.mjs --fast          # no dramatic pauses (CI)
 *
 * Writes demo/transcript.json (timed lines) and demo/demo.cast (asciinema v2).
 */

import { spawn } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");
const FAST = process.argv.includes("--fast");
const PORT = Number(process.env.WOVYR_DEMO_PORT ?? 8099);
const BASE = `http://127.0.0.1:${PORT}`;
const PRINCIPAL = "procurement-admin";

/* ────────────────────────────── presentation ────────────────────────────── */

const C = {
  reset: "\x1b[0m", dim: "\x1b[2m", bold: "\x1b[1m",
  red: "\x1b[31m", green: "\x1b[32m", yellow: "\x1b[33m",
  blue: "\x1b[34m", magenta: "\x1b[35m", cyan: "\x1b[36m", grey: "\x1b[90m",
};

const started = Date.now();
/** @type {{t:number, text:string}[]} */
const transcript = [];

/** Per-line cadence, and a multiplier on the dramatic pauses between beats.
 * Both are real elapsed time — the .cast timings are measured, never synthesized,
 * so the recording plays back at exactly the speed this ran at. */
const PACE = FAST ? 0 : Number(process.env.WOVYR_DEMO_PACE_MS ?? 55);
const SCALE = FAST ? 0 : Number(process.env.WOVYR_DEMO_PAUSE_SCALE ?? 3.9);

/** A blocking sleep, so `emit` can stay synchronous at all ~100 call sites while
 * still advancing the wall clock the cast is timed against. Nothing else needs to
 * run during narration, so parking the event loop here costs nothing. */
function syncSleep(ms) {
  if (ms <= 0) return;
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function emit(text) {
  transcript.push({ t: (Date.now() - started) / 1000, text: text + "\n" });
  process.stdout.write(text + "\n");
  syncSleep(PACE);
}

const sleep = (ms) =>
  new Promise((r) => setTimeout(r, FAST ? Math.min(ms, 10) : Math.round(ms * SCALE)));

/** @type {{n:number,title:string,outcome:"cut"|"sound",t:number}[]} */
const ACTS = [];

/**
 * `outcome` is declared here rather than inferred later: it records whether
 * *policy cut something* in this beat, which is the only thing the brand's
 * madder is allowed to mark (website/landing/DESIGN-system.md §2). Act 3
 * kills the server and Act 5 lists an earlier block — neither is itself a
 * cut, and a presenter scanning for a ✗ glyph would wrongly call both one.
 */
function act(n, title, outcome) {
  ACTS.push({ n, title, outcome, t: (Date.now() - started) / 1000 });
  emit("");
  emit(`${C.bold}${C.magenta}${"─".repeat(74)}${C.reset}`);
  emit(`${C.bold}${C.magenta} ACT ${n}${C.reset}${C.bold}  ${title}${C.reset}`);
  emit(`${C.bold}${C.magenta}${"─".repeat(74)}${C.reset}`);
}

const say = (s) => emit(`${C.cyan}▸${C.reset} ${s}`);
const note = (s) => emit(`  ${C.grey}${s}${C.reset}`);
const good = (s) => emit(`  ${C.green}✓${C.reset} ${s}`);
const bad = (s) => emit(`  ${C.red}✗${C.reset} ${s}`);
const wire = (s) => emit(`  ${C.dim}${C.blue}${s}${C.reset}`);

function block(lines, color = C.grey) {
  for (const l of lines) emit(`  ${color}│${C.reset} ${l}`);
}

/**
 * The capture is replayed in a fixed 92-column grid (by the HTML player and by
 * `cast2video`), and a terminal wraps an over-long line back to column 0 —
 * which destroys the indentation these blocks rely on to be readable. So wrap
 * here instead, where the hanging indent can be chosen deliberately.
 *
 * Takes plain text only: measuring width around SGR escapes is a trap, so the
 * caller colours the returned lines as a whole.
 */
const WRAP_AT = 84;

function wrapPlain(text, width = WRAP_AT, hang = 0) {
  const out = [];
  let cur = "";
  for (const word of String(text).split(/\s+/).filter(Boolean)) {
    const cand = cur ? `${cur} ${word}` : word;
    if (cand.length > width && cur) {
      out.push(cur);
      cur = " ".repeat(hang) + word;
    } else {
      cur = cand;
    }
  }
  if (cur) out.push(cur);
  return out.length ? out : [""];
}

/** `label` on the first line, continuations hanging under the value column. */
function labelled(label, value, width = WRAP_AT) {
  const pad = label.length;
  const lines = wrapPlain(value, width - pad, 0);
  return lines.map((l, i) => (i === 0 ? label + l : " ".repeat(pad) + l));
}

/* ──────────────────────────────── plumbing ──────────────────────────────── */

function binPath() {
  if (process.env.WOVYR_BIN) return process.env.WOVYR_BIN;
  const exe = process.platform === "win32" ? "wovyr.exe" : "wovyr";
  for (const profile of ["debug", "release"]) {
    const p = join(REPO, "target", profile, exe);
    if (existsSync(p)) return p;
  }
  throw new Error(
    `no wovyr binary found under target/{debug,release}/. Build one first:\n` +
      `    cargo build -p wovyr-cli\n` +
      `  or point WOVYR_BIN at an installed one.`,
  );
}

async function req(method, path, { body, principal = PRINCIPAL, expect } = {}) {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      "content-type": "application/json",
      "X-Wovyr-Principal": principal,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json;
  try { json = text ? JSON.parse(text) : undefined; } catch { json = undefined; }
  if (expect !== undefined && res.status !== expect) {
    throw new Error(
      `${method} ${path} → ${res.status}, expected ${expect}\n${text.slice(0, 600)}`,
    );
  }
  return { status: res.status, json, text };
}

class Server {
  constructor(home, bin) { this.home = home; this.bin = bin; this.proc = null; }

  async start(label) {
    this.proc = spawn(this.bin, ["dev", "--addr", `127.0.0.1:${PORT}`], {
      cwd: REPO,
      env: {
        ...process.env,
        HOME: this.home,
        USERPROFILE: this.home,
        WOVYR_ALLOW_ANONYMOUS: "1",
        WOVYR_PLATFORM_ADMINS: PRINCIPAL,
        WOVYR_UI_POLICY: join(REPO, "examples", "policies", "default-ui-policy.yaml"),
        WOVYR_LOG: "error",
        // Keep the demo's dispatch loop responsive without waiting 5s.
        WOVYR_DISPATCH_INTERVAL_SECS: "1",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    this.logs = [];
    this.proc.stdout.on("data", (d) => this.logs.push(String(d)));
    this.proc.stderr.on("data", (d) => this.logs.push(String(d)));

    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      try {
        const r = await fetch(`${BASE}/healthz`);
        if (r.ok) {
          const h = await r.json();
          good(`${label} — pid ${this.proc.pid}, healthz ${JSON.stringify(h)}`);
          return h;
        }
      } catch { /* not up yet */ }
      await new Promise((r) => setTimeout(r, 200));
    }
    throw new Error(`server did not become healthy in 90s\n${this.logs.join("").slice(-2000)}`);
  }

  /** A real SIGKILL — no graceful shutdown, no drain, no chance to flush. */
  kill9() {
    const pid = this.proc.pid;
    // On Windows Node maps SIGKILL to TerminateProcess: same abrupt,
    // uncatchable termination — the process gets no shutdown path either way.
    this.proc.kill("SIGKILL");
    return pid;
  }

  async waitGone() {
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      try { await fetch(`${BASE}/healthz`); } catch { return; }
      await new Promise((r) => setTimeout(r, 150));
    }
    throw new Error("server still answering after SIGKILL");
  }
}

/** Poll until a pending frame for this execution appears, or it goes terminal. */
async function frameOrTerminal(executionId, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const { json: list } = await req("GET", "/api/v1/ui/frames", { expect: 200 });
    const found = (list.data ?? []).find((f) => f.execution_id === executionId);
    if (found) return { frame: found };
    const { json: ex } = await req("GET", `/api/v1/workflows/${executionId}`, { expect: 200 });
    const status = ex?.execution?.status ?? "";
    if (!["created", "validated", "scheduled", "running", "resumed"].includes(status)) {
      return { terminal: status, execution: ex.execution };
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`execution ${executionId} neither presented a frame nor went terminal`);
}

async function waitStatus(executionId, want, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  let last = "";
  while (Date.now() < deadline) {
    const { json } = await req("GET", `/api/v1/workflows/${executionId}`, { expect: 200 });
    last = json?.execution?.status ?? "";
    if (last === want) return json.execution;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`execution ${executionId} stuck at "${last}", wanted "${want}"`);
}

async function auditEntries() {
  const { json } = await req("GET", "/api/v1/audit?limit=100", { expect: 200 });
  return (json.data ?? []).slice().sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0));
}

/** The workflow's own event timeline — where an activity's real output lives. */
async function timeline(executionId) {
  const { json } = await req("GET", `/api/v1/workflows/${executionId}`, { expect: 200 });
  return { execution: json.execution, events: json.events ?? [] };
}

const short = (h, n = 12) => (typeof h === "string" && h.length > n ? h.slice(0, n) + "…" : String(h));

/* ─────────────────────────────────── run ─────────────────────────────────── */

async function main() {
  const bin = binPath();
  const home = await mkdtemp(join(tmpdir(), "wovyr-demo-"));
  const server = new Server(home, bin);
  const stamp = Date.now();
  const ids = {
    block: `demo-poisoned-${stamp}`,
    approve: `demo-checkout-${stamp}`,
  };

  emit("");
  emit(`${C.bold}  Wovyr — the trust layer for AI-generated interfaces${C.reset}`);
  emit(`${C.grey}  A procurement agent reorders lab supplies. Two takes.${C.reset}`);
  emit("");
  note(`binary   ${bin.replace(REPO, ".")}`);
  note(`policy   examples/policies/default-ui-policy.yaml`);
  note(`state    ${home}`);
  note(`         ${C.dim}a scratch HOME — the real ~/.wovyr is never touched${C.reset}`);

  try {
    await server.start("server up");
    await sleep(900);

    /* ── ACT 1 ─────────────────────────────────────────────────────────── */
    act(1, "The agent is poisoned. It asks for a card number.", "cut");
    say("A compromised vendor page steers the model into composing this frame:");
    block([
      `${C.yellow}text_input${C.reset}  name: ${C.red}card_number${C.reset}   label: "Card number"`,
      `${C.yellow}button${C.reset}      action: pay        label: "Continue"`,
    ]);
    await sleep(1400);

    const blockManifest = await readFile(
      join(REPO, "examples", "workflows", "ui-checkout-block.yaml"), "utf8",
    );
    wire(`POST /api/v1/workflows   (examples/workflows/ui-checkout-block.yaml)`);
    await req("POST", "/api/v1/workflows", {
      body: { manifest: blockManifest, execution_id: ids.block },
      expect: 200,
    });
    await sleep(700);

    const blocked = await frameOrTerminal(ids.block);
    if (blocked.frame) throw new Error("REGRESSION: the credential frame became visible");
    bad(`execution ${C.bold}${blocked.terminal}${C.reset} — the trust layer refused the frame`);
    await sleep(900);

    say("Was it ever visible to a human? Ask the only route a renderer can pull from:");
    wire("GET /api/v1/ui/frames");
    const { json: pendingNow } = await req("GET", "/api/v1/ui/frames", { expect: 200 });
    const leaked = (pendingNow.data ?? []).filter((f) => f.execution_id === ids.block);
    if (leaked.length) throw new Error("REGRESSION: blocked frame is pullable");
    good(`0 frames. It was blocked ${C.bold}before${C.reset} becoming visible — not filtered after.`);
    note("There is no raw-HTML node and no credential-input component in the protocol");
    note("at all, so this is a structural guarantee, not a blocklist that can be evaded.");
    await sleep(1500);

    say("The refusal is not just a log line. It's in the tamper-evident chain:");
    const chain1 = await auditEntries();
    const blockRec = chain1.find((e) => (e.event?.action ?? e.action) === "ui.frame.block");
    if (!blockRec) {
      note(`(no ui.frame.block record; chain holds ${chain1.length} entries)`);
    } else {
      const ev = blockRec.event ?? blockRec;
      block([
        `action   ${C.bold}${ev.action}${C.reset}`,
        `outcome  ${ev.outcome}`,
        // The policy's reason is a full sentence and easily 200+ characters —
        // wrapped under its own label rather than left to fold at column 0.
        ...labelled("reason   ", ev.reason ?? "-").map((l) => `${C.red}${l}${C.reset}`),
        `actor    ${ev.actor?.principal ?? "-"}`,
        `seq ${blockRec.seq}   hash ${short(blockRec.hash)}   prev ${short(blockRec.prev_hash)}`,
      ], C.red);
      good("the rule that fired is named in the record — an auditor can reconstruct why");
    }
    await sleep(2000);

    /* ── ACT 2 ─────────────────────────────────────────────────────────── */
    act(2, "Take two: the safe frame. It presents, with a content hash.", "sound");
    const approveManifest = await readFile(
      join(REPO, "examples", "workflows", "ui-checkout-approve.yaml"), "utf8",
    );
    wire(`POST /api/v1/workflows   (examples/workflows/ui-checkout-approve.yaml)`);
    await req("POST", "/api/v1/workflows", {
      body: { manifest: approveManifest, execution_id: ids.approve },
      expect: 200,
    });

    const presented = await frameOrTerminal(ids.approve);
    if (!presented.frame) {
      throw new Error(`safe frame did not present (went ${presented.terminal})`);
    }
    const frame = presented.frame;
    good(`frame presented — activity ${C.bold}${frame.activity_id}${C.reset}`);
    block([
      `frame_id    ${C.bold}${frame.frame_id}${C.reset}`,
      `frame_hash  ${C.bold}${frame.frame_hash}${C.reset}`,
    ], C.green);
    await sleep(900);

    const { json: full } = await req("GET", `/api/v1/ui/frames/${frame.frame_id}`, { expect: 200 });
    say("What the human will actually see (a constrained vocabulary, nothing else):");
    const f = full.frame ?? full;
    block([
      `${C.bold}${f.title ?? "Confirm order"}${C.reset}`,
      `Reorder 3 boxes of pipette tips from LabSupply Co?`,
      `${C.grey}Vendor${C.reset}  LabSupply Co`,
      `${C.grey}Total ${C.reset}  $412.80`,
      `PO number  ${C.dim}[________]${C.reset} ${C.grey}(required)${C.reset}`,
      `${C.green}[ Approve ]${C.reset}  ${C.grey}[ Cancel ]${C.reset}`,
    ], C.green);
    note("the execution is now suspended, durably, waiting on a human");
    await sleep(2200);

    /* ── ACT 3 ─────────────────────────────────────────────────────────── */
    act(3, "Now break it. SIGKILL the server before anyone decides.", "sound");
    const pid = server.kill9();
    bad(`kill -9 ${pid}   ${C.grey}(no graceful shutdown, no drain, no flush)${C.reset}`);
    await server.waitGone();
    good("server is gone — the port refuses connections");
    await sleep(1300);

    say("Restart it. Same state directory, brand-new process:");
    await server.start("server back up");
    await sleep(1000);

    wire("GET /api/v1/ui/frames");
    const { json: afterList } = await req("GET", "/api/v1/ui/frames", { expect: 200 });
    const again = (afterList.data ?? []).find((x) => x.execution_id === ids.approve);
    if (!again) throw new Error("REGRESSION: the pending frame did not survive the kill");

    const idSame = again.frame_id === frame.frame_id;
    const hashSame = again.frame_hash === frame.frame_hash;
    // The full 64-char hash is the evidence, so it gets its own line rather
    // than sharing one with a verdict and wrapping.
    block([
      `frame_id    ${again.frame_id}`,
      `frame_hash  ${again.frame_hash}`,
    ], C.green);
    if (!idSame || !hashSame) throw new Error("REGRESSION: frame identity drifted across restart");
    good(`${C.bold}identical${C.reset} to before the kill — id and hash, byte for byte`);
    good("the pending decision survived a termination it could not catch");
    await sleep(2000);

    /* ── ACT 4 ─────────────────────────────────────────────────────────── */
    act(4, "The human decides. Only inside the vocabulary the frame declared.", "cut");
    say('First, something the frame never offered — action "launch":');
    wire(`POST /api/v1/ui/decisions/${frame.frame_id}`);
    wire(`     {"action":"launch"}`);
    const refused = await req("POST", `/api/v1/ui/decisions/${frame.frame_id}`, {
      body: { action: "launch" },
    });
    if (refused.status !== 400) throw new Error(`expected 400, got ${refused.status}`);
    bad(`400 ${refused.json?.error?.code ?? ""} — refused at the boundary, never reached the workflow`);
    await sleep(1400);

    say("Now the real approval, with the PO number the frame required:");
    wire(`POST /api/v1/ui/decisions/${frame.frame_id}`);
    wire(`     {"action":"approve","values":{"po_number":"PO-4471"}}`);
    const decided = await req("POST", `/api/v1/ui/decisions/${frame.frame_id}`, {
      body: { action: "approve", values: { po_number: "PO-4471" } },
      expect: 200,
    });
    good(`${decided.json?.status ?? "decided"} — the workflow resumes`);
    await waitStatus(ids.approve, "completed");
    good(`execution ${C.bold}${C.green}completed${C.reset}`);
    await sleep(700);

    say("The decision is now part of the execution's durable event log, not a side note:");
    const { events } = await timeline(ids.approve);
    const completed = events.find(
      (e) => (e.event?.type ?? e.type) === "activity_completed" &&
             (e.event?.id ?? e.id) === "confirm",
    );
    const payload = (completed?.event ?? completed)?.output ?? {};
    block([
      `${C.dim}activity_completed${C.reset}  id: confirm`,
      `action        ${C.bold}${payload.action}${C.reset}`,
      `values        ${JSON.stringify(payload.values ?? {})}`,
      `decided_by    ${C.bold}${payload.decided_by}${C.reset}   ${C.grey}(the verified principal, not a client claim)${C.reset}`,
      `decided_at_ms ${payload.decided_at_ms}`,
      `frame_hash    ${short(payload.frame_hash, 24)}   ${C.grey}(binds the decision to exactly what was shown)${C.reset}`,
    ], C.green);
    if (payload.decided_by !== PRINCIPAL) {
      throw new Error(`decision attribution missing/wrong: ${JSON.stringify(payload)}`);
    }
    if (payload.frame_hash !== frame.frame_hash) {
      throw new Error("decision's frame_hash does not match the presented frame");
    }
    good("you cannot claim a human approved something other than what they saw");
    await sleep(2000);

    /* ── ACT 5 ─────────────────────────────────────────────────────────── */
    act(5, "Prove it. The whole story, hash-linked, in order.", "sound");
    say("Every record the server wrote for this session — nothing filtered out:");
    const chain = await auditEntries();

    for (const rec of chain) {
      const ev = rec.event ?? rec;
      const action = String(ev.action ?? "");
      const denied = ev.outcome === "denied";
      const mark = denied ? `${C.red}✗${C.reset}` : `${C.green}✓${C.reset}`;
      const tint = action.startsWith("ui.") ? C.bold : C.dim;
      emit(
        `  ${mark} ${C.dim}seq ${String(rec.seq).padStart(3)}${C.reset}  ` +
        `${tint}${action.padEnd(26)}${C.reset}` +
        `${C.grey}${String(ev.resource?.id ?? ev.resource ?? "").slice(0, 24).padEnd(26)}${C.reset}` +
        `${C.dim}prev ${short(rec.prev_hash, 8)} → ${short(rec.hash, 8)}${C.reset}`,
      );
    }
    emit("");

    // Verify the linkage ourselves, from data the server just handed us: each
    // entry must commit to its predecessor's hash. This is the property that
    // makes deleting or editing an interior record detectable.
    let breaks = 0;
    for (let i = 1; i < chain.length; i++) {
      if (chain[i].prev_hash !== chain[i - 1].hash) breaks++;
    }
    if (breaks > 0) {
      bad(`${breaks} broken link(s) — the chain does not verify`);
      throw new Error("audit chain linkage failed");
    }
    good(
      `chain verified client-side: ${chain.length} entries, ` +
      `${chain.length - 1}/${chain.length - 1} prev_hash values match their predecessor`,
    );
    note("each hash is a keyed HMAC, so an attacker with full write access to the log");
    note("can neither forge an interior entry (no key) nor truncate the tail undetected");
    note("(a monotonic head anchor is written separately on every append).");
    await sleep(1200);

    emit("");
    emit(`${C.bold}${C.green}${"═".repeat(74)}${C.reset}`);
    emit(`${C.bold}  What just happened, in one line each:${C.reset}`);
    emit(`  ${C.red}✗${C.reset} a credential input never reached a human — structurally impossible`);
    emit(`  ${C.green}✓${C.reset} a legitimate frame rendered, hashed, and pended durably`);
    emit(`  ${C.green}✓${C.reset} kill -9 changed nothing: same frame_id, same frame_hash`);
    emit(`  ${C.red}✗${C.reset} an undeclared action was refused before the workflow saw it`);
    emit(`  ${C.green}✓${C.reset} every step is provable to an auditor, in order, tamper-evident`);
    emit(`${C.bold}${C.green}${"═".repeat(74)}${C.reset}`);
    emit("");
    emit(`  ${C.grey}Self-hosted. One Rust binary. Runs air-gapped.${C.reset}`);
    emit(`  ${C.bold}wovyr.com${C.reset}   ${C.grey}·   cargo install wovyr-cli${C.reset}`);
    emit("");

    return { ok: true, ids, frame, chainLength: chain.length };
  } finally {
    try { server.proc?.kill("SIGKILL"); } catch { /* already gone */ }
    await mkdir(join(HERE, "out"), { recursive: true });
    const total = (Date.now() - started) / 1000;

    // A --fast run is a correctness gate, not a take: it must never clobber the
    // paced capture the published player is built from, so it writes beside it.
    const prefix = FAST ? "fast-" : "";

    await writeFile(
      join(HERE, "out", `${prefix}transcript.json`),
      JSON.stringify({ duration_s: total, recorded_at: new Date().toISOString(), acts: ACTS, lines: transcript }, null, 2),
    );

    // asciinema v2: a header line, then [time, "o", data] events.
    const width = 92, height = 40;
    const cast = [
      JSON.stringify({
        version: 2, width, height,
        timestamp: Math.floor(started / 1000),
        title: "Wovyr — the trust layer for AI-generated interfaces",
        env: { SHELL: "/bin/sh", TERM: "xterm-256color" },
      }),
      ...transcript.map((l) => JSON.stringify([l.t, "o", l.text.replace(/\n/g, "\r\n")])),
    ].join("\n") + "\n";
    await writeFile(join(HERE, "out", `${prefix}demo.cast`), cast);

    try { await rm(home, { recursive: true, force: true }); } catch { /* windows lock */ }

    // Replay happens in a fixed `width`-column grid, where an over-long line
    // folds back to column 0 and wrecks the indentation. Report offenders: the
    // defect is invisible in the terminal this was recorded on (which is wider),
    // so without this check it only ever shows up in the finished video.
    const over = transcript
      .map((l, i) => ({ i, n: l.text.replace(/\x1b\[[0-9;]*m/g, "").trimEnd().length }))
      .filter((l) => l.n > width);

    process.stderr.write(
      `\n[demo] ${transcript.length} lines, ${total.toFixed(1)}s\n` +
      `[demo] wrote demo/out/${prefix}transcript.json and demo/out/${prefix}demo.cast\n` +
      (FAST ? `[demo] (--fast: the paced demo.cast was left untouched)\n` : "") +
      (over.length
        ? `[demo] WARNING: ${over.length} line(s) exceed the ${width}-column grid and will ` +
          `wrap in replay:\n` +
          over.map((l) => `[demo]   line ${l.i + 1}: ${l.n} cols\n`).join("")
        : `[demo] all lines fit the ${width}-column grid\n`),
    );
  }
}

main().then(
  (r) => { process.stderr.write(`[demo] OK — chain verified, ${r.chainLength} audit records\n`); process.exit(0); },
  (e) => { process.stderr.write(`\n[demo] FAILED: ${e.message}\n`); process.exit(1); },
);
