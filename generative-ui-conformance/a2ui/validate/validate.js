// Validate every A2UI surface in ../vectors-a2ui.json against A2UI's official
// JSON Schemas, using ajv -- the same validator A2UI's own conformance harness
// (specification/v0_9_1/test/run_tests.py) drives via ajv-cli.
//
//   node fetch-schemas.js && npm install && node validate.js
//
// Exits non-zero if any surface fails. Run negative-control.js too: a validator
// that passes everything proves nothing.
const fs = require('fs');
const path = require('path');
const Ajv2020 = require('ajv/dist/2020');
const addFormats = require('ajv-formats');

const SCHEMA_DIR = path.join(__dirname, 'schemas');
const VECTORS = path.join(__dirname, '..', 'vectors-a2ui.json');
const MESSAGE_SCHEMA = 'https://a2ui.org/specification/v0_9/server_to_client.json';
// server_to_client.json refs "catalog.json#/$defs/anyComponent", which resolves against
// its own $id base to this id. The basic catalog's real $id is .../catalogs/basic/catalog.json,
// so it is registered under both. This is the substitution run_tests.py performs.
const CATALOG_REF_ID = 'https://a2ui.org/specification/v0_9/catalog.json';

if (!fs.existsSync(SCHEMA_DIR)) {
  console.error('schemas/ not found -- run: node fetch-schemas.js');
  process.exit(2);
}

const ajv = new Ajv2020({ strict: false, allErrors: true, validateFormats: true });
addFormats(ajv);

for (const f of fs.readdirSync(SCHEMA_DIR)) {
  if (!f.endsWith('.json') || f === 'sample.json') continue;
  const s = JSON.parse(fs.readFileSync(path.join(SCHEMA_DIR, f), 'utf8'));
  if (s.$id) { try { ajv.addSchema(s, s.$id); } catch (e) { console.log(`skip ${f}: ${e.message}`); } }
}
const aliased = JSON.parse(fs.readFileSync(path.join(SCHEMA_DIR, 'catalog.json'), 'utf8'));
aliased.$id = CATALOG_REF_ID;
ajv.addSchema(aliased, CATALOG_REF_ID);

const validate = ajv.getSchema(MESSAGE_SCHEMA);
if (!validate) { console.error('could not compile ' + MESSAGE_SCHEMA); process.exit(2); }

const doc = JSON.parse(fs.readFileSync(VECTORS, 'utf8'));
const all = [...doc.ported_vectors, ...doc.a2ui_specific_vectors];

let pass = 0, fail = 0, messages = 0;
console.log(`validating ${all.length} A2UI surfaces against ${MESSAGE_SCHEMA}\n`);

for (const v of all) {
  const errs = [];
  v.a2ui.messages.forEach((m, i) => {
    messages++;
    if (!validate(m)) {
      const key = Object.keys(m).find(k => k !== 'version') || '?';
      for (const e of validate.errors) {
        if (e.keyword === 'oneOf') continue; // noise from the 3 non-matching branches
        errs.push(`msg[${i}] (${key}) ${e.instancePath || '/'} ${e.keyword}: ${e.message}`);
      }
    }
  });
  if (errs.length) {
    console.log(`FAIL  ${v.name}`);
    [...new Set(errs)].slice(0, 10).forEach(e => console.log('        ' + e));
    fail++;
  } else {
    console.log(`PASS  ${v.name}  (${v.a2ui.messages.length} messages)`);
    pass++;
  }
}

console.log(`\nsurfaces: ${all.length}   messages: ${messages}   PASS: ${pass}   FAIL: ${fail}`);
process.exit(fail === 0 ? 0 : 1);
