import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../apps/desktop/ui/', import.meta.url));
const port = Number(process.env.RHYTHM_PREVIEW_PORT || 8767);
const types = { '.html': 'text/html', '.mjs': 'text/javascript', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.jpg': 'image/jpeg', '.webmanifest': 'application/manifest+json' };
http.createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const file = path.resolve(root, '.' + (pathname === '/' ? '/index.html' : pathname));
    const relative = path.relative(root, file);
    if (relative.startsWith('..') || path.isAbsolute(relative) || !types[path.extname(file)] || /(?:\.test\.mjs)$/.test(file)) {
      response.writeHead(404).end(); return;
    }
    response.writeHead(200, { 'Content-Type': types[path.extname(file)] + '; charset=utf-8', 'Cache-Control': 'no-store' });
    response.end(await readFile(file));
  } catch { response.writeHead(404).end(); }
}).listen(port, '127.0.0.1', () => console.log(`Rhythm preview: http://127.0.0.1:${port}`));
