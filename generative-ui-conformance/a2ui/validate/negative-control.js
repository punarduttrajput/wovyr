// Negative control: deliberately malformed surfaces that MUST be rejected.
// If any of these pass, the "9/9 pass" result is vacuous.
const fs = require('fs');
const path = require('path');
const Ajv2020 = require('ajv/dist/2020');
const addFormats = require('ajv-formats');

const SCHEMA_DIR = path.join(__dirname, 'schemas');
const ajv = new Ajv2020({ strict: false, allErrors: true });
addFormats(ajv);
for (const f of fs.readdirSync(SCHEMA_DIR)) {
  if (!f.endsWith('.json') || f === 'sample.json') continue;
  const s = JSON.parse(fs.readFileSync(path.join(SCHEMA_DIR, f), 'utf8'));
  if (s.$id) try { ajv.addSchema(s, s.$id); } catch {}
}
const REF_ID = 'https://a2ui.org/specification/v0_9/catalog.json';
const cat = JSON.parse(fs.readFileSync(path.join(SCHEMA_DIR, 'catalog.json'), 'utf8'));
cat.$id = REF_ID; ajv.addSchema(cat, REF_ID);
const validate = ajv.getSchema('https://a2ui.org/specification/v0_9/server_to_client.json');

const surface = (components) => ({
  version: 'v0.9',
  updateComponents: { surfaceId: 's', components },
});

const CASES = [
  ['unknown component type', surface([{ id: 'root', component: 'EvilWidget', text: 'x' }])],
  ['Button missing required action', surface([
    { id: 'root', component: 'Button', child: 'lbl' },
    { id: 'lbl', component: 'Text', text: 'Go' }])],
  ['Button variant outside enum', surface([
    { id: 'root', component: 'Button', child: 'lbl', variant: 'destructive', action: { event: { name: 'x' } } },
    { id: 'lbl', component: 'Text', text: 'Go' }])],
  ['Text missing required text', surface([{ id: 'root', component: 'Text', variant: 'body' }])],
  ['TextField missing required label', surface([
    { id: 'root', component: 'TextField', value: { path: '/a' } }])],
  ['Image missing required url', surface([{ id: 'root', component: 'Image', description: 'x' }])],
  ['bogus extra property on component', surface([
    { id: 'root', component: 'Text', text: 'hi', role: 'destructive' }])],
  ['bad version string', { version: 'v9.9', updateComponents: { surfaceId: 's', components: [{ id: 'root', component: 'Text', text: 'hi' }] } }],
  ['empty components array', surface([])],
  ['two message keys at once', {
    version: 'v0.9',
    createSurface: { surfaceId: 's', catalogId: 'c' },
    deleteSurface: { surfaceId: 's' },
  }],
  ['createSurface missing catalogId', { version: 'v0.9', createSurface: { surfaceId: 's' } }],
];

console.log('=== negative control: every case MUST be rejected ===\n');
let caught = 0, missed = 0;
for (const [name, doc] of CASES) {
  const ok = validate(doc);
  if (ok) { console.log(`  LEAKED  ${name}  <-- validator did not catch this`); missed++; }
  else {
    const e = validate.errors.find(x => x.keyword !== 'oneOf') || validate.errors[0];
    console.log(`  caught  ${name.padEnd(34)} (${e.keyword} @ ${e.instancePath || '/'})`);
    caught++;
  }
}
console.log(`\ncaught: ${caught}   LEAKED: ${missed}`);
if (missed > 0) console.log('\nHARNESS IS NOT TRUSTWORTHY — the positive result is vacuous.');
process.exit(missed === 0 ? 0 : 1);
