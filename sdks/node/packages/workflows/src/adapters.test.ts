// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterEach, describe, expect, it } from 'vitest';

import { honoHandler, nextRouteHandlers, toNodeListener } from './adapters.js';
import { createWorkflowHandler } from './handler.js';
import { MANIFEST_PATH } from './protocol.js';
import { FAKE_API_URL, FakeOrchestrator } from './testing/fake-orchestrator.js';
import { defineWorkflow } from './workflow.js';

const ping = defineWorkflow('ping', async (ctx) => ctx.step('pong', () => 'pong'));
let server: Server | undefined;

afterEach(async () => {
  await new Promise<void>((resolve) => (server ? server.close(() => resolve()) : resolve()));
  server = undefined;
});

function setup() {
  const temps = new FakeOrchestrator();
  const handler = createWorkflowHandler({
    workflows: [ping],
    apiUrl: FAKE_API_URL,
    fetch: temps.fetch,
    invocationKey: temps.invocationKey,
    tracer: false,
  });
  return { temps, handler };
}

async function listen(listener: Parameters<typeof createServer>[1]): Promise<string> {
  server = createServer(listener);
  await new Promise<void>((resolve) => server?.listen(0, '127.0.0.1', resolve));
  return `http://127.0.0.1:${(server?.address() as AddressInfo).port}`;
}

describe('adapters', () => {
  it('serves the handler from a Node http server and runs a workflow through it', async () => {
    const { temps, handler } = setup();
    const node = toNodeListener(handler);
    const base = await listen((request, response) => void node(request, response));
    // Temps calls the app over HTTP, as in production.
    temps.attach((request) => {
      const url = new URL(request.url);
      return fetch(`${base}${url.pathname}`, {
        method: request.method,
        headers: request.headers,
        body: request.method === 'POST' ? request.body : undefined,
        duplex: 'half',
      } as RequestInit);
    });
    expect((await temps.manifest()).workflows).toEqual([{ name: 'ping', version: null, concurrency: null }]);
    const { run } = temps.start('ping');
    await temps.settle();
    expect(run.output).toBe('pong');
    expect((await fetch(`${base}/elsewhere`)).status).toBe(404);
  });

  it('hands requests outside the workflow prefix to the next middleware', async () => {
    const { handler } = setup();
    const node = toNodeListener(handler);
    const base = await listen((request, response) =>
      void node(request, response, () => {
        response.statusCode = 299;
        response.end('next');
      }),
    );
    const response = await fetch(`${base}/api/users`);
    expect(response.status).toBe(299);
    expect(await response.text()).toBe('next');
    expect((await fetch(`${base}${MANIFEST_PATH}`)).status).toBe(401);
  });

  it('exposes Next.js route handlers and a Hono handler', async () => {
    const { handler } = setup();
    const { GET, POST } = nextRouteHandlers(handler);
    expect(GET).toBe(handler);
    expect(POST).toBe(handler);
    const response = await honoHandler(handler)({ req: { raw: new Request(`http://app${MANIFEST_PATH}`) } });
    expect(response.status).toBe(401);
  });
});
