#!/usr/bin/env node
/**
 * Assembles demo/out/player.html — a self-contained, offline replay of the
 * capture in demo/out/demo.cast.
 *
 * Everything is inlined (fonts as data URIs, the cast as JSON) because the
 * page has to work with no network at all: dropped into an Artifact, opened
 * from disk, or embedded on wovyr.com behind a strict CSP.
 *
 * Design follows website/landing/DESIGN-system.md as adopted 2026-07-31 —
 * the same tokens, the same two faces, and in particular the same rule that
 * **madder is never decorative**: in this page it appears only on the two
 * beats where policy cut something. Everything sound is indigo.
 *
 *   node demo/build-player.mjs
 */

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");
const OUT = join(HERE, "out");
const FONTS = join(REPO, "website", "node_modules");

/** The canonical mark: 22 straight segments on a 100×100 grid, symmetrical
 * about x=50. Copied verbatim from website/public/favicon.svg — DESIGN-system
 * §4 keeps all five copies in sync by hand, so this must match exactly. */
const WOLF =
  "M50 26 L34 30 L18 6 L22 38 L12 52 L22 60 L16 72 L30 74 L36 64 L40 86 " +
  "L44 94 L50 97 L56 94 L60 86 L64 64 L70 74 L84 72 L78 60 L88 52 L78 38 " +
  "L82 6 L66 30 Z";

async function dataUri(relPath) {
  const buf = await readFile(join(FONTS, relPath));
  return `data:font/woff2;base64,${buf.toString("base64")}`;
}

/** Escape for embedding inside a <script> block: `<` can otherwise close the
 * element, and U+2028/U+2029 are literal line terminators in a JS source text.
 * Both are built by codepoint so no invisible character sits in this file. */
const LS = String.fromCharCode(0x2028), PS = String.fromCharCode(0x2029);
const jsonForScript = (v) =>
  JSON.stringify(v)
    .replace(/</g, "\\u003c")
    .replace(new RegExp(LS, "g"), "\\u2028")
    .replace(new RegExp(PS, "g"), "\\u2029");

const esc = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/* ── the cast ───────────────────────────────────────────────────────────── */

async function loadCast() {
  const raw = await readFile(join(OUT, "demo.cast"), "utf8");
  const lines = raw.split("\n").filter(Boolean);
  const header = JSON.parse(lines[0]);
  const events = lines.slice(1).map((l) => JSON.parse(l));
  // Each emit() wrote exactly one line, so an event is a line.
  const out = events.map(([t, , data]) => ({ t, text: data.replace(/\r?\n$/, "") }));
  return { header, lines: out, duration: out.length ? out[out.length - 1].t : 0 };
}

/** Strip SGR sequences — used to read structure out of the narration. */
const plain = (s) => s.replace(/\x1b\[[0-9;]*m/g, "");

/**
 * The act index comes from the driver's own declaration in transcript.json,
 * not from scanning the rendered output. Whether a beat *cut* something is a
 * claim about what the runtime did, and madder is reserved for exactly that
 * (DESIGN-system §2) — inferring it from a ✗ glyph mislabels Act 3, which
 * kills the server, and Act 5, which merely lists an earlier block.
 *
 * Only the timestamp is taken from the capture: the driver records `t` just
 * before printing the act's header, so seeking to it would land a frame short
 * of the header being visible.
 */
async function loadActs(lines) {
  let declared;
  try {
    declared = JSON.parse(await readFile(join(OUT, "transcript.json"), "utf8")).acts;
  } catch { /* handled below */ }
  if (!Array.isArray(declared) || declared.length === 0) {
    throw new Error(
      "demo/out/transcript.json has no `acts` — re-record with the current " +
      "driver (node demo/killer-demo.mjs); the beat outcomes are not inferable.",
    );
  }
  return declared.map((a) => {
    const header = lines.find((l) => {
      const m = plain(l.text).match(/^\s*ACT (\d+)\b/);
      return m && Number(m[1]) === a.n;
    });
    return { n: a.n, title: a.title, cut: a.outcome === "cut", t: header ? header.t : a.t };
  });
}

/* ── ANSI → brand palette ───────────────────────────────────────────────── */

/**
 * The capture's colours are remapped onto the design system rather than shown
 * as generic terminal colours. `31` is the only code that becomes madder,
 * because it is the only one the driver uses for a denial.
 */
const COLOR = {
  31: "cut",     // madder — a thread policy cut
  32: "sound",   // indigo — a sound thread
  36: "lead",    // indigo — the narrator's own voice
  35: "struct",  // act rules
  33: "thread",  // indigo-soft — node/type names
  34: "wire",    // indigo-soft — HTTP lines
  90: "note",    // ink-2 — annotations
};

function ansiToHtml(text) {
  let html = "";
  let bold = false, dim = false, color = null;
  const re = /\x1b\[([0-9;]*)m/g;
  let last = 0, m;

  const open = () => {
    const cls = [];
    if (bold) cls.push("b");
    if (dim) cls.push("dim");
    if (color) cls.push("c-" + color);
    return cls.length ? `<span class="${cls.join(" ")}">` : "";
  };
  const push = (chunk) => {
    if (!chunk) return;
    const o = open();
    html += o ? o + esc(chunk) + "</span>" : esc(chunk);
  };

  while ((m = re.exec(text)) !== null) {
    push(text.slice(last, m.index));
    for (const codeStr of (m[1] === "" ? "0" : m[1]).split(";")) {
      const code = Number(codeStr);
      if (code === 0) { bold = false; dim = false; color = null; }
      else if (code === 1) bold = true;
      else if (code === 2) dim = true;
      else if (COLOR[code]) color = COLOR[code];
    }
    last = re.lastIndex;
  }
  push(text.slice(last));
  return html || "&nbsp;";
}

/* ── build ──────────────────────────────────────────────────────────────── */

const mmss = (s) =>
  `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

const CLAIMS = [
  { cut: true,  text: "A credential input never reached a human — the component does not exist in the protocol." },
  { cut: false, text: "A legitimate frame rendered, content-hashed, and pended durably on a human." },
  { cut: false, text: "kill -9 changed nothing: same frame id, same frame hash, same decision outstanding." },
  { cut: true,  text: "An action the frame never offered was refused before the workflow saw it." },
  { cut: false, text: "Every step is replayable from a keyed hash chain, in order, tamper-evident." },
];

async function main() {
  const cast = await loadCast();
  const chapters = await loadActs(cast.lines);
  const [archivo, mono400, mono500] = await Promise.all([
    dataUri("@fontsource-variable/archivo/files/archivo-latin-wght-normal.woff2"),
    dataUri("@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-400-normal.woff2"),
    dataUri("@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-500-normal.woff2"),
  ]);

  const recordedAt = new Date(cast.header.timestamp * 1000)
    .toISOString().replace("T", " ").slice(0, 16) + " UTC";

  const lineHtml = cast.lines
    .map((l, i) => `<div class="ln is-hidden" data-i="${i}">${ansiToHtml(l.text)}</div>`)
    .join("");

  const chapterHtml = chapters
    .map(
      (c) => `<button class="ch" data-t="${c.t}" data-n="${c.n}" type="button">
        <span class="ch-n">${String(c.n).padStart(2, "0")}</span>
        <span class="ch-t">${esc(c.title)}</span>
        <span class="ch-o ${c.cut ? "is-cut" : "is-sound"}">${c.cut ? "cut" : "sound"}</span>
        <span class="ch-time">${mmss(c.t)}</span>
      </button>`,
    )
    .join("");

  const claimHtml = CLAIMS.map(
    (c) => `<li class="${c.cut ? "is-cut" : "is-sound"}">${esc(c.text)}</li>`,
  ).join("");

  // WEB-305: the doctype and <html lang> were absent, so every browser parsed
  // this page in quirks mode (`document.compatMode === "BackCompat"`) — a
  // different box model, different table line-height inheritance, and no
  // guarantee the layout holds as engines diverge. It went unnoticed while the
  // file was only ever opened from disk; it is now served at /demo/player.html
  // on the site (website/scripts/sync-demo.mjs), which is where a rendering
  // difference actually costs something. `lang` also gives screen readers a
  // pronunciation to use for the narration.
  const html = `<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>Wovyr — the checkout that was cut before anyone saw it</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
@font-face{font-family:Archivo;src:url(${archivo}) format("woff2");font-weight:100 900;font-display:swap}
@font-face{font-family:"Plex Mono";src:url(${mono400}) format("woff2");font-weight:400;font-display:swap}
@font-face{font-family:"Plex Mono";src:url(${mono500}) format("woff2");font-weight:500;font-display:swap}

/* Tokens: website/landing/DESIGN-system.md §2. Light is the primary theme. */
:root{
  --paper:#DEE3E7; --paper-2:#EBEEF0; --sunk:#D2D9DE;
  --ink:#0E141A; --ink-2:#53616D;
  --warp:#C3CCD3; --warp-2:#AEB9C2;
  --indigo:#27386B; --indigo-soft:#8FA0CE; --madder:#A63A26;
  --term-ink:#1B242C;
  color-scheme:light;
}
@media (prefers-color-scheme:dark){
  :root{
    --paper:#0C1115; --paper-2:#131920; --sunk:#080B0E;
    --ink:#E7EBEF; --ink-2:#8B98A3;
    --warp:#1D252C; --warp-2:#2C3641;
    --indigo:#8AA2E8; --indigo-soft:#3C4C7E; --madder:#E5735C;
    --term-ink:#C8D2DB;
    color-scheme:dark;
  }
}
:root[data-theme="light"]{
  --paper:#DEE3E7; --paper-2:#EBEEF0; --sunk:#D2D9DE;
  --ink:#0E141A; --ink-2:#53616D;
  --warp:#C3CCD3; --warp-2:#AEB9C2;
  --indigo:#27386B; --indigo-soft:#8FA0CE; --madder:#A63A26;
  --term-ink:#1B242C;
  color-scheme:light;
}
:root[data-theme="dark"]{
  --paper:#0C1115; --paper-2:#131920; --sunk:#080B0E;
  --ink:#E7EBEF; --ink-2:#8B98A3;
  --warp:#1D252C; --warp-2:#2C3641;
  --indigo:#8AA2E8; --indigo-soft:#3C4C7E; --madder:#E5735C;
  --term-ink:#C8D2DB;
  color-scheme:dark;
}

*{box-sizing:border-box}
body{
  margin:0; background:var(--paper); color:var(--ink);
  font-family:Archivo,system-ui,sans-serif; font-weight:400;
  -webkit-font-smoothing:antialiased;
  /* The warp: a vertical thread grid, the ground every plate sits on. */
  background-image:repeating-linear-gradient(90deg,var(--warp) 0 1px,transparent 1px 34px);
  background-position:center top;
}
.plate{max-width:1000px;margin:0 auto;padding:clamp(20px,4vw,52px) clamp(16px,4vw,40px) 64px}

/* ── annotations: the marks a draftsman leaves ─────────────────────────── */
.ann{
  font-family:"Plex Mono",ui-monospace,monospace; font-weight:500;
  font-size:11px; letter-spacing:.13em; text-transform:uppercase;
  color:var(--ink-2); font-variant-numeric:tabular-nums;
}

header{display:flex;align-items:center;gap:14px;flex-wrap:wrap;
  padding-bottom:14px;border-bottom:1px solid var(--warp-2)}
.mark{width:26px;height:26px;flex:none;display:block}
.mark path{fill:var(--indigo)}
.wordmark{font-weight:700;font-size:17px;letter-spacing:-.01em}
header .ann{margin-left:auto}

h1{
  font-weight:700; letter-spacing:-.021em; line-height:1.06;
  font-size:clamp(28px,4.6vw,50px); text-wrap:balance;
  margin:clamp(26px,4vw,42px) 0 0; max-width:22ch;
}
.standfirst{
  margin:16px 0 0; max-width:62ch; font-size:clamp(15px,1.5vw,17.5px);
  line-height:1.55; color:var(--ink-2);
}
.standfirst strong{color:var(--ink);font-weight:600}

/* ── the figure: a sunk terminal well ─────────────────────────────────── */
.fig{margin-top:clamp(26px,3.6vw,40px)}
.fig-head{display:flex;align-items:baseline;gap:12px;margin-bottom:9px}
.fig-head .ann:last-child{margin-left:auto}
.well{
  background:var(--sunk); border:1px solid var(--warp-2);
  padding:16px clamp(12px,2vw,20px);
  height:clamp(360px,52vh,520px); overflow:auto;
  font-family:"Plex Mono",ui-monospace,"Cascadia Mono",Menlo,Consolas,monospace;
  font-size:clamp(10px,1.32vw,13px); line-height:1.62;
  color:var(--term-ink); scroll-behavior:smooth;
}
.well .ln{white-space:pre;min-height:1.62em}
.well .ln.is-hidden{display:none}
.b{font-weight:500;color:var(--ink)}
.dim{opacity:.62}
.c-cut{color:var(--madder)}
.c-sound,.c-lead,.c-struct{color:var(--indigo)}
.c-thread,.c-wire{color:var(--indigo-soft)}
.c-note{color:var(--ink-2)}
@media (prefers-color-scheme:dark){.c-thread,.c-wire{color:#7C8FCB}}
:root[data-theme="dark"] .c-thread,:root[data-theme="dark"] .c-wire{color:#7C8FCB}

/* ── transport ────────────────────────────────────────────────────────── */
.transport{
  display:flex;align-items:center;gap:14px;flex-wrap:wrap;
  padding:11px 0;border-bottom:1px solid var(--warp-2);
}
button{font:inherit;color:inherit;cursor:pointer}
.tbtn{
  background:none;border:1px solid var(--warp-2);color:var(--ink);
  width:34px;height:30px;display:grid;place-items:center;padding:0;
  transition:border-color .15s,background .15s;
}
.tbtn:hover{border-color:var(--indigo);background:var(--paper-2)}
.tbtn svg{width:12px;height:12px;fill:currentColor}
.scrub{
  flex:1 1 180px;height:30px;border:none;background:none;padding:0;
  position:relative;cursor:pointer;min-width:120px;
}
.scrub-track{position:absolute;inset:50% 0 auto;transform:translateY(-50%);
  height:3px;background:var(--warp-2)}
.scrub-fill{position:absolute;inset:50% auto auto 0;transform:translateY(-50%);
  height:3px;background:var(--indigo);width:0}
.scrub-head{position:absolute;top:50%;left:0;width:2px;height:13px;
  transform:translate(-1px,-50%);background:var(--indigo)}
.clock{font-family:"Plex Mono",monospace;font-size:12px;
  font-variant-numeric:tabular-nums;color:var(--ink-2)}
.clock b{color:var(--ink);font-weight:500}
.rate{background:none;border:1px solid var(--warp-2);padding:5px 9px;
  font-family:"Plex Mono",monospace;font-size:11px;letter-spacing:.06em;color:var(--ink-2)}
.rate:hover{border-color:var(--indigo);color:var(--ink)}
:focus-visible{outline:2px solid var(--indigo);outline-offset:2px}

/* ── act index: a real sequence, so it is numbered ────────────────────── */
.acts{margin-top:26px}
.acts h2,.proof h2{margin:0 0 10px;font:inherit}
.ch{
  display:grid;grid-template-columns:2.4em 1fr auto auto;gap:12px;align-items:baseline;
  width:100%;text-align:left;background:none;border:none;
  border-bottom:1px solid var(--warp);padding:9px 2px;
}
.ch:hover{background:var(--paper-2)}
.ch[aria-current="true"] .ch-t{color:var(--ink);font-weight:600}
.ch[aria-current="true"] .ch-n{color:var(--indigo)}
.ch-n{font-family:"Plex Mono",monospace;font-size:11px;letter-spacing:.1em;
  color:var(--ink-2);font-variant-numeric:tabular-nums}
.ch-t{font-size:14.5px;line-height:1.35;color:var(--ink-2)}
.ch-o{font-family:"Plex Mono",monospace;font-size:10px;letter-spacing:.13em;
  text-transform:uppercase;padding:2px 7px;border:1px solid currentColor}
.ch-o.is-cut{color:var(--madder)}
.ch-o.is-sound{color:var(--indigo)}
.ch-time{font-family:"Plex Mono",monospace;font-size:11px;color:var(--ink-2);
  font-variant-numeric:tabular-nums}

/* ── proof ────────────────────────────────────────────────────────────── */
.proof{margin-top:32px;padding-top:22px;border-top:1px solid var(--warp-2)}
.proof ul{list-style:none;margin:0;padding:0;display:grid;gap:9px;max-width:70ch}
.proof li{padding-left:26px;position:relative;font-size:14.5px;line-height:1.5;color:var(--ink-2)}
.proof li::before{position:absolute;left:0;top:0;font-family:"Plex Mono",monospace;font-size:13px}
.proof li.is-cut::before{content:"\\2717";color:var(--madder)}
.proof li.is-sound::before{content:"\\2713";color:var(--indigo)}
.repro{
  margin-top:24px;padding:13px 15px;background:var(--paper-2);
  border-left:2px solid var(--indigo);
}
.repro code{font-family:"Plex Mono",monospace;font-size:12.5px;color:var(--ink);
  display:block;margin-top:6px;overflow-x:auto}
footer{margin-top:34px;padding-top:16px;border-top:1px solid var(--warp)}
@media (prefers-reduced-motion:reduce){.well{scroll-behavior:auto}}
@media (max-width:620px){
  .ch{grid-template-columns:2.2em 1fr auto;row-gap:4px}
  .ch-time{display:none}
}
</style>

<div class="plate">
  <header>
    <svg class="mark" viewBox="0 0 100 100" role="img" aria-label="Wovyr"><path d="${WOLF}"/></svg>
    <span class="wordmark">Wovyr</span>
    <span class="ann">Recording &middot; uncut &middot; ${cast.duration.toFixed(1)}s</span>
  </header>

  <h1>The checkout that was cut before anyone saw it.</h1>
  <p class="standfirst">
    A procurement agent is asked to reorder lab supplies, and composes a form for a
    human to approve. Twice. <strong>Take one is poisoned</strong> — it asks for a card
    number, and never reaches a screen. <strong>Take two is sound</strong> — it renders,
    survives a <code>kill -9</code> unchanged, and the approval it collects is bound to
    exactly what was shown.
  </p>

  <figure class="fig">
    <div class="fig-head">
      <span class="ann">Fig. 1 &mdash; one unedited session</span>
      <span class="ann">${recordedAt}</span>
    </div>
    <div class="well" id="well" role="log" aria-label="Terminal recording">${lineHtml}</div>
    <div class="transport">
      <button class="tbtn" id="play" type="button" aria-label="Pause">
        <svg viewBox="0 0 12 12" id="glyph"><rect x="1" y="1" width="3.4" height="10"/><rect x="7.6" y="1" width="3.4" height="10"/></svg>
      </button>
      <button class="tbtn" id="restart" type="button" aria-label="Restart">
        <svg viewBox="0 0 12 12"><path d="M6 1.4V0L2.6 2.4 6 4.8V3.4a2.9 2.9 0 1 1-2.9 2.9H1.7A4.3 4.3 0 1 0 6 1.4z"/></svg>
      </button>
      <button class="scrub" id="scrub" type="button" aria-label="Seek">
        <span class="scrub-track"></span><span class="scrub-fill" id="fill"></span><span class="scrub-head" id="head"></span>
      </button>
      <span class="clock"><b id="now">00:00</b> / ${mmss(cast.duration)}</span>
      <button class="rate" id="rate" type="button">1&times;</button>
    </div>
  </figure>

  <section class="acts">
    <h2 class="ann">Act index</h2>
    ${chapterHtml}
  </section>

  <section class="proof">
    <h2 class="ann">What the session proves</h2>
    <ul>${claimHtml}</ul>
    <div class="repro">
      <span class="ann">Reproduce it yourself &mdash; no API key, no cloud account</span>
      <code>cargo build -p wovyr-cli &amp;&amp; node demo/killer-demo.mjs</code>
    </div>
  </section>

  <footer><span class="ann">Self-hosted &middot; one Rust binary &middot; runs air-gapped &middot; wovyr.com</span></footer>
</div>

<script>
(() => {
  const LINES = ${jsonForScript(cast.lines.map((l) => l.t))};
  const DUR = ${JSON.stringify(cast.duration)};
  const well = document.getElementById("well");
  const nodes = [...well.querySelectorAll(".ln")];
  const chapters = [...document.querySelectorAll(".ch")];
  const glyph = document.getElementById("glyph");
  const playBtn = document.getElementById("play");
  const fill = document.getElementById("fill");
  const head = document.getElementById("head");
  const nowEl = document.getElementById("now");
  const rateBtn = document.getElementById("rate");
  const scrub = document.getElementById("scrub");

  const PAUSE = '<rect x="1" y="1" width="3.4" height="10"/><rect x="7.6" y="1" width="3.4" height="10"/>';
  const PLAY = '<path d="M2 1l9 5-9 5z"/>';
  const mmss = (s) => String(Math.floor(s/60)).padStart(2,"0")+":"+String(Math.floor(s%60)).padStart(2,"0");

  let t = 0, playing = false, rate = 1, shown = -1, last = 0;

  function render(seek) {
    // How many lines have landed by now.
    let n = 0;
    while (n < LINES.length && LINES[n] <= t) n++;
    if (n - 1 !== shown) {
      if (n - 1 > shown) {
        for (let i = shown + 1; i < n; i++) nodes[i].classList.remove("is-hidden");
      } else {
        for (let i = shown; i >= n; i--) nodes[i].classList.add("is-hidden");
      }
      shown = n - 1;
      const target = well.scrollHeight - well.clientHeight;
      if (seek) { well.style.scrollBehavior = "auto"; well.scrollTop = target; well.style.scrollBehavior = ""; }
      else well.scrollTop = target;
    }
    const pct = DUR ? Math.min(1, t / DUR) : 0;
    fill.style.width = (pct * 100) + "%";
    head.style.left = (pct * 100) + "%";
    nowEl.textContent = mmss(t);

    let current = null;
    for (const c of chapters) if (Number(c.dataset.t) <= t) current = c;
    for (const c of chapters) c.setAttribute("aria-current", String(c === current));
  }

  function frame(ts) {
    if (!playing) return;
    if (!last) last = ts;
    t += ((ts - last) / 1000) * rate;
    last = ts;
    if (t >= DUR) { t = DUR; setPlaying(false); render(false); return; }
    render(false);
    requestAnimationFrame(frame);
  }

  function setPlaying(on) {
    playing = on; last = 0;
    glyph.innerHTML = on ? PAUSE : PLAY;
    playBtn.setAttribute("aria-label", on ? "Pause" : "Play");
    if (on) requestAnimationFrame(frame);
  }

  function seek(to) {
    t = Math.max(0, Math.min(DUR, to));
    render(true);
  }

  playBtn.onclick = () => { if (t >= DUR) t = 0; setPlaying(!playing); };
  document.getElementById("restart").onclick = () => { seek(0); setPlaying(true); };
  rateBtn.onclick = () => {
    rate = rate === 1 ? 2 : rate === 2 ? 4 : 1;
    rateBtn.innerHTML = rate + "&times;";
  };
  scrub.onclick = (e) => {
    const r = scrub.getBoundingClientRect();
    seek(((e.clientX - r.left) / r.width) * DUR);
  };
  for (const c of chapters) c.onclick = () => { seek(Number(c.dataset.t)); };

  addEventListener("keydown", (e) => {
    if (e.target.closest("button") && e.key === " ") return;
    if (e.key === " ") { e.preventDefault(); if (t >= DUR) t = 0; setPlaying(!playing); }
    else if (e.key === "ArrowRight") { e.preventDefault(); seek(t + 5); }
    else if (e.key === "ArrowLeft") { e.preventDefault(); seek(t - 5); }
  });

  // Someone who asked for less motion gets the finished transcript, not an
  // animation they have to sit through.
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) { seek(DUR); setPlaying(false); }
  else { render(true); setPlaying(true); }
})();
</script>
`;

  await mkdir(OUT, { recursive: true });
  await writeFile(join(OUT, "player.html"), html);
  const kb = (Buffer.byteLength(html) / 1024).toFixed(0);
  process.stdout.write(
    `[player] demo/out/player.html — ${kb} KB, ${cast.lines.length} lines, ` +
    `${cast.duration.toFixed(1)}s, ${chapters.length} acts ` +
    `(${chapters.filter((c) => c.cut).length} containing a cut)\n`,
  );
}

main().catch((e) => { process.stderr.write(`[player] FAILED: ${e.message}\n`); process.exit(1); });
