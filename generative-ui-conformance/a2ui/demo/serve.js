// Tiny static server for local preview. GitHub Pages needs none of this --
// it's only here because fetch() of a .wasm does not work over file://.
const http = require('http');
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..', '..'); // serve from generative-ui-conformance/
const PORT = Number(process.env.PORT || 4173);
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.css': 'text/css; charset=utf-8',
};

http.createServer((req, res) => {
  let rel = decodeURIComponent(req.url.split('?')[0]);
  // Redirect rather than rewrite: the page fetches ./a2ui_trust_demo.wasm
  // relative to its own URL, so serving it at "/" would 404 the module.
  if (rel === '/' || rel === '/a2ui/demo' || rel === '/a2ui/demo/') {
    res.writeHead(302, { location: '/a2ui/demo/index.html' }).end();
    return;
  }
  const file = path.join(ROOT, rel);
  if (!file.startsWith(ROOT)) { res.writeHead(403).end('forbidden'); return; }
  fs.readFile(file, (err, buf) => {
    if (err) { res.writeHead(404).end('not found: ' + rel); return; }
    res.writeHead(200, { 'content-type': TYPES[path.extname(file)] || 'application/octet-stream' });
    res.end(buf);
  });
}).listen(PORT, '127.0.0.1', () => {
  console.log(`serving ${ROOT} at http://127.0.0.1:${PORT}/`);
});
