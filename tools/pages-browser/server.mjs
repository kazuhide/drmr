// Test-only loopback server: no directory listing, proxy, upload or arbitrary file reads.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const files = new Map([
  ['index.html', 'text/html; charset=utf-8'],
  ['ja/index.html', 'text/html; charset=utf-8'],
  ['guide/index.html', 'text/html; charset=utf-8'],
  ['ja/guide/index.html', 'text/html; charset=utf-8'],
  ['assets/guide.css', 'text/css; charset=utf-8'],
  ['assets/site.css', 'text/css; charset=utf-8'],
  ['assets/drmr.svg', 'image/svg+xml'],
  ['assets/fonts/LICENSE-ibm-plex.txt', 'text/plain; charset=utf-8'],
  ['assets/fonts/README.txt', 'text/plain; charset=utf-8'],
  ['assets/fonts/ibm-plex-sans-latin-400-normal.woff2', 'font/woff2'],
  ['assets/fonts/ibm-plex-sans-latin-600-normal.woff2', 'font/woff2'],
  ['assets/fonts/ibm-plex-mono-latin-400-normal.woff2', 'font/woff2'],
]);

export function resource(raw) {
  try {
    const url = new URL(raw, 'http://127.0.0.1');
    const path = decodeURIComponent(url.pathname);
    if (url.search || path.includes('\\') || path.split('/').includes('..')) return null;
    let relative = (path.startsWith('/drmr/') ? path.slice(6) : path.slice(1)) || 'index.html';
    if (relative.endsWith('/')) relative += 'index.html';
    return files.has(relative) ? { relative, type: files.get(relative) } : null;
  } catch {
    return null;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const server = createServer(async (request, response) => {
    const selected = resource(request.url);
    if (!selected || !['GET', 'HEAD'].includes(request.method)) {
      response.writeHead(404).end();
      return;
    }
    try {
      const bytes = await readFile(new URL('../../site/' + selected.relative, import.meta.url));
      response.writeHead(200, { 'Content-Type': selected.type, 'Content-Length': bytes.length,
        'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
      response.end(request.method === 'HEAD' ? undefined : bytes);
    } catch {
      response.writeHead(500).end();
    }
  });
  server.requestTimeout = 5000;
  server.headersTimeout = 5000;
  server.listen(4173, '127.0.0.1');
  process.once('SIGTERM', () => {
    server.close();
    server.closeAllConnections();
  });
}
