// The demo's actual argument: feed A2UI surfaces whose *intent* cannot be
// declared, and show what policy can and cannot say about them.
const fs = require('fs');
const path = require('path');

const WASM = path.join(__dirname, 'target', 'wasm32-unknown-unknown', 'release', 'a2ui_trust_demo.wasm');

(async () => {
  const mod = await WebAssembly.compile(fs.readFileSync(WASM));
  const { exports: x } = await WebAssembly.instantiate(mod, {});
  const evaluate = (messages, policy) => {
    const buf = Buffer.from(JSON.stringify({ messages, policy }), 'utf8');
    const p = x.alloc(buf.length);
    new Uint8Array(x.memory.buffer, p, buf.length).set(buf);
    const packed = BigInt(x.evaluate_a2ui(p, buf.length));
    x.dealloc(p, buf.length);
    return JSON.parse(Buffer.from(x.memory.buffer,
      Number(packed >> 32n), Number(packed & 0xffffffffn)).toString('utf8'));
  };

  const doc = JSON.parse(fs.readFileSync(path.join(__dirname, 'inexpressible.json'), 'utf8'));
  console.log(doc.title + '\n' + '='.repeat(doc.title.length) + '\n');

  for (const c of doc.cases) {
    const got = evaluate(c.messages, 'default');
    console.log(`## ${c.name}`);
    console.log(`   ${c.what_it_is}`);
    console.log(`   wovyr protocol : ${c.in_wovyr.split(' -- ')[0]}`);
    console.log(`   through A2UI   : ${got.verdict.toUpperCase()}${got.rule ? ' (' + got.rule + ')' : ''}`);
    console.log('');
  }

  console.log('--- floor path check (regression for the earlier harness bug) ---');
  const floorInteractive = evaluate([
    { version: 'v0.9', updateComponents: { surfaceId: 's', components: [
      { id: 'root', component: 'Column', children: ['b'] },
      { id: 'b', component: 'Button', child: 'l', action: { event: { name: 'go' } } },
      { id: 'l', component: 'Text', text: 'Go' }] } }], 'floor');
  const floorDisplay = evaluate([
    { version: 'v0.9', updateComponents: { surfaceId: 's', components: [
      { id: 'root', component: 'Text', text: 'All queues nominal.' }] } }], 'floor');
  console.log(`  interactive under no-policy floor : ${floorInteractive.verdict}` +
    `${floorInteractive.rule ? ' (' + floorInteractive.rule + ')' : ''}   [expect block/hosted_floor]`);
  console.log(`  display-only under no-policy floor: ${floorDisplay.verdict}   [expect allow]`);
})();
