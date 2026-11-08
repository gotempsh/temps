// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { runPlugin, createManifest, createEmbeddedUiHandler, extractAuthContext } from '@temps-sdk/plugin';
import type { TempsPlugin, EmbeddedAssets, RequestHandler } from '@temps-sdk/plugin';
import { page } from './page';

const assets: EmbeddedAssets = new Map([
  ['index.html', { content: Buffer.from(page), contentType: 'text/html; charset=utf-8', immutable: false }],
  ['app.js', { content: Buffer.from(''), contentType: 'application/javascript', immutable: false }],
]);
const messageSchema = { type: 'string', minLength: 1, maxLength: 200 };
const responses = { '200': { description: 'Echoed message' }, '400': { description: 'Invalid input' }, '401': { description: 'Authentication required' } };

export const plugin: TempsPlugin = {
  manifest: () => createManifest('temps-playground', '0.1.0')
    .displayName('Temps Playground')
    .description('Explore plugin AI tools and project chat in Temps')
    .addNav('Temps Playground', 'flask-conical', '/ui/', { section: 'platform', order: 50 })
    .ui({ entry_js: '/ui/app.js', css: [], routes: [{ path: '/ui/', title: 'Temps Playground' }] })
    .build(),
  embeddedUiAssets: () => assets,
  openapi: () => ({
    openapi: '3.1.0', info: { title: 'Temps Playground', version: '0.1.0' },
    paths: {
      '/tools/echo': { get: { operationId: 'echo', summary: 'Echo a short message to verify plugin reads', 'x-temps-ai-tool': {}, parameters: [{ name: 'message', in: 'query', required: true, schema: messageSchema }], responses } },
      '/tools/acknowledge': { post: { operationId: 'acknowledge', summary: 'Acknowledge a short message to test write approval; no data is stored', 'x-temps-ai-tool': { access: 'write' }, requestBody: { required: true, content: { 'application/json': { schema: { type: 'object', required: ['message'], properties: { message: messageSchema }, additionalProperties: false } } } }, responses } },
    },
  }),
  handler: (): RequestHandler => {
    const serveUi = createEmbeddedUiHandler(assets);
    return async (req, res) => {
      if (req.method === 'GET' && serveUi(req, res)) return;
      const send = (status: number, body: object) => {
        res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
        res.end(JSON.stringify(body));
      };
      if (!extractAuthContext(req)) return send(401, { error: 'Authentication required' });
      const url = new URL(req.url ?? '/', 'http://plugin.local');
      const read = req.method === 'GET' && url.pathname === '/tools/echo';
      const write = req.method === 'POST' && url.pathname === '/tools/acknowledge';
      if (!read && !write) return send(404, { error: 'Not found' });
      let message: unknown;
      if (read) message = url.searchParams.get('message');
      else {
        const chunks: Buffer[] = [];
        let size = 0;
        for await (const chunk of req) {
          size += chunk.length;
          if (size > 2048) return send(413, { error: 'Request exceeds 2048 bytes' });
          chunks.push(Buffer.from(chunk));
        }
        try { message = JSON.parse(Buffer.concat(chunks).toString()).message; }
        catch { return send(400, { error: 'Expected a JSON object with message' }); }
      }
      if (typeof message !== 'string' || message.length < 1 || message.length > 200)
        return send(400, { error: 'Message must contain 1–200 characters' });
      send(200, { message, acknowledged: write, stored: false });
    };
  },
};

if (import.meta.main) await runPlugin(plugin);
