// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test';
import { createServer } from 'node:http';
import { plugin } from './index';
import { page } from './page';
import { verifyProxyHeaders } from '@temps-sdk/plugin';
import { attachVerifiedCaller } from '../node_modules/@temps-sdk/plugin/dist/auth.js';

test('declares separate read and approval-gated write tools', () => {
  const schema = plugin.openapi!() as any;
  expect(plugin.manifest().name).toBe('temps-playground');
  expect(schema.paths['/tools/echo'].get['x-temps-ai-tool']).toEqual({});
  expect(schema.paths['/tools/acknowledge'].post['x-temps-ai-tool']).toEqual({ access: 'write' });
  expect(page).toContain('/api/projects/');
  expect(page).toContain('/chat?chat=');
});

test('serves authenticated reads and writes and rejects malformed input', async () => {
  const handler = await plugin.handler({} as never);
  const server = createServer((req, res) => {
    const headers = new Headers();
    for (const [name, value] of Object.entries(req.headers)) if (typeof value === 'string') headers.set(name, value);
    const verification = verifyProxyHeaders(headers, 'fixture-assertion');
    if (verification.verified && verification.caller) attachVerifiedCaller(req, verification.caller);
    void handler(req, res);
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw Error('Missing fixture port');
  const root = `http://127.0.0.1:${address.port}`;
  const headers = { 'x-temps-auth-signature': 'fixture-assertion', 'x-temps-user-id': '1', 'x-temps-user-email': 'tester@example.com', 'x-temps-user-role': 'admin', 'x-temps-request-id': 'fixture-request', 'Content-Type': 'application/json' };
  try {
    expect((await fetch(root + '/tools/echo?message=hello')).status).toBe(401);
    const echo = await fetch(root + '/tools/echo?message=hello', { headers });
    expect(await echo.json()).toEqual({ message: 'hello', acknowledged: false, stored: false });
    const ack = await fetch(root + '/tools/acknowledge', { method: 'POST', headers, body: JSON.stringify({ message: 'approved' }) });
    expect(await ack.json()).toEqual({ message: 'approved', acknowledged: true, stored: false });
    for (const body of ['null', '{}', '{', JSON.stringify({ message: 'x'.repeat(201) })]) {
      expect((await fetch(root + '/tools/acknowledge', { method: 'POST', headers, body })).status).toBe(400);
    }
    expect((await fetch(root + '/tools/acknowledge', { method: 'POST', headers, body: 'x'.repeat(2049) })).status).toBe(413);
    const ui = await fetch(root + '/ui/');
    expect(await ui.text()).toContain('Temps Playground');
  } finally { await new Promise<void>((resolve,reject) => server.close(error => error ? reject(error) : resolve())); }
});
