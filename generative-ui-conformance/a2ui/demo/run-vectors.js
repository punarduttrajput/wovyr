// Feed every A2UI vector through the wasm trust layer and report what policy
// catches, what it misses, and why. This is the demo's data, in text form.
const fs = require('fs');
const path = require('path');

const WASM = path.join(__dirname, 'target', 'wasm32-unknown-unknown', 'release', 'a2ui_trust_demo.wasm');
const VECTORS = path.join(__dirname, '..', 'vectors-a2ui.json');

(async () => {
  const bytes = fs.readFileSync(WASM);
  const mod = await WebAssembly.compile(bytes);
  const imports = WebAssembly.Module.imports(mod);
  console.log(`wasm ${bytes.length.toLocaleString()} bytes, ${imports.length} imports\n`);
  const { exports: x } = await WebAssembly.instantiate(mod, {});

  const evaluate = (messages) => {
    const buf = Buffer.from(JSON.stringify({ messages }), 'utf8');
    const p = x.alloc(buf.length);
    new Uint8Array(x.memory.buffer, p, buf.length).set(buf);
    const packed = BigInt(x.evaluate_a2ui(p, buf.length));
    x.dealloc(p, buf.length);
    const op = Number(packed >> 32n), ol = Number(packed & 0xffffffffn);
    return JSON.parse(Buffer.from(x.memory.buffer, op, ol).toString('utf8'));
  };

  const doc = JSON.parse(fs.readFileSync(VECTORS, 'utf8'));
  const vectors = [...doc.ported_vectors, ...doc.a2ui_specific_vectors];

  const rows = [];
  for (const v of vectors) {
    const got = evaluate(v.a2ui.messages);
    rows.push({ name: v.name, expected: v.expected, expectedRule: v.rule || null, got });
  }

  const pad = (s, n) => String(s).padEnd(n);
  console.log(pad('vector', 52) + pad('want', 7) + pad('got', 7) + 'rule');
  console.log('-'.repeat(100));
  let caught = 0, missed = 0, other = 0;
  for (const r of rows) {
    const g = r.got.verdict;
    const rule = r.got.rule || '';
    const mark = g === r.expected ? ' ' : '!';
    console.log(mark + ' ' + pad(r.name, 50) + pad(r.expected, 7) + pad(g, 7) + rule);
    if (r.expected === 'block' && g === 'block') caught++;
    else if (r.expected === 'block' && g !== 'block') missed++;
    else other++;
  }

  console.log('\n=== what the missing action class costs ===');
  console.log(`policy caught          : ${caught}`);
  console.log(`policy blind           : ${missed}`);
  console.log(`must-allow (unchanged) : ${other}`);

  const notes = new Map();
  for (const r of rows) for (const n of (r.got.adapter_notes || [])) notes.set(n, (notes.get(n) || 0) + 1);
  console.log('\n=== adapter notes (lossy mappings) ===');
  for (const [n, c] of [...notes].sort((a, b) => b[1] - a[1])) console.log(`  [x${c}] ${n}`);

  fs.writeFileSync(path.join(__dirname, 'result.json'), JSON.stringify(rows, null, 2));
})();
