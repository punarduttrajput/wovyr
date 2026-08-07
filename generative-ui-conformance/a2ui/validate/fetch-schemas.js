const fs = require('fs');
const path = require('path');

const REPO = 'https://raw.githubusercontent.com/a2ui-project/a2ui/main/specification/v0_9_1/';
const BASE = REPO + 'json/';
// The component catalog lives outside json/, but server_to_client.json refs it as
// "catalog.json#/$defs/anyComponent" -- without it, ajv cannot resolve the reference
// and validate.js fails to compile. Fetched into the same schemas/ directory.
const CATALOG = { url: REPO + 'catalogs/basic/catalog.json', as: 'catalog.json' };
const FILES = [
  'client_capabilities.json',
  'client_data_model.json',
  'client_to_server.json',
  'client_to_server_list.json',
  'client_to_server_list_wrapper.json',
  'common_types.json',
  'sample.json',
  'server_capabilities.json',
  'server_to_client.json',
  'server_to_client_list.json',
  'server_to_client_list_wrapper.json',
];

const outDir = path.join(__dirname, 'schemas');
fs.mkdirSync(outDir, { recursive: true });

(async () => {
  let failed = 0;
  const targets = [
    ...FILES.map(f => ({ url: BASE + f, as: f })),
    CATALOG,
  ];
  for (const { url, as: f } of targets) {
    const res = await fetch(url);
    if (!res.ok) { console.log(`FAIL ${f}: HTTP ${res.status}`); failed++; continue; }
    const text = await res.text();
    fs.writeFileSync(path.join(outDir, f), text);
    let id = '(no $id)', draft = '(no $schema)', refs = 0;
    try {
      const j = JSON.parse(text);
      id = j.$id || '(no $id)';
      draft = j.$schema || '(no $schema)';
      refs = (text.match(/"\$ref"/g) || []).length;
    } catch (e) { id = 'PARSE ERROR: ' + e.message; }
    console.log(`ok   ${f.padEnd(38)} ${String(text.length).padStart(7)}b  refs=${String(refs).padStart(3)}  $id=${id}`);
    if (f === 'server_to_client.json') console.log(`     draft: ${draft}`);
  }
  if (!fs.existsSync(path.join(outDir, 'catalog.json'))) {
    console.log('\nERROR: catalog.json missing -- validate.js cannot resolve component refs.');
    failed++;
  }
  console.log(`\n${targets.length - failed}/${targets.length} fetched into ${outDir}`);
  process.exit(failed === 0 ? 0 : 1);
})();
