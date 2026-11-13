// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createHash } from 'node:crypto';
import { afterEach, describe, expect, it } from 'vitest';

import { createWorkflowHandler } from './handler.js';
import { INVOKE_PATH, MANIFEST_PATH, type InvocationClaims } from './protocol.js';
import { FAKE_API_URL, FakeOrchestrator } from './testing/fake-orchestrator.js';
import { defineWorkflow } from './workflow.js';

let ran = 0;
const hello = defineWorkflow(
  'hello',
  async (ctx) => {
    ran += 1;
    return ctx.step('greet', () => `hello ${(ctx.input as { name: string }).name}`);
  },
  { version: '2', concurrency: 3 },
);

function setup() {
  const temps = new FakeOrchestrator();
  const handler = createWorkflowHandler({
    workflows: [hello],
    apiUrl: FAKE_API_URL,
    fetch: temps.fetch,
    invocationKey: temps.invocationKey,
    tracer: false,
  });
  temps.attach(handler);
  return { temps, handler };
}

function invokeBody(lease = 1, version = 1) {
  return JSON.stringify({
    protocol_version: version,
    run_id: 'run-1',
    workflow: 'hello',
    attempt: 1,
    lease,
    now: new Date().toISOString(),
    deadline_at: new Date(Date.now() + 900_000).toISOString(),
    lease_expires_at: new Date(Date.now() + 120_000).toISOString(),
    input: { name: 'ada' },
    journal: [],
  });
}

function claimsFor(body: string, overrides: Partial<InvocationClaims> = {}): InvocationClaims {
  return {
    v: 1,
    typ: 'invoke',
    pid: 1,
    eid: 1,
    run: 'run-1',
    lease: 1,
    exp: Math.floor(Date.now() / 1000) + 60,
    body: createHash('sha256').update(body).digest('hex'),
    ...overrides,
  };
}

function post(temps: FakeOrchestrator, body: string, claims: InvocationClaims) {
  return new Request(`http://app${INVOKE_PATH}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${temps.signToken(claims)}` },
    body,
  });
}

afterEach(() => {
  ran = 0;
});

describe('workflow handler', () => {
  it('serves the manifest only with a manifest token', async () => {
    const { temps, handler } = setup();
    expect(await temps.manifest()).toEqual({
      protocol_version: 1,
      sdk: '@temps-sdk/workflows/0.1.0',
      workflows: [{ name: 'hello', version: '2', concurrency: 3 }],
    });
    const anonymous = await handler(new Request(`http://app${MANIFEST_PATH}`));
    expect(anonymous.status).toBe(401);
    expect(await anonymous.json()).toMatchObject({ title: 'Invalid Invocation Token', detail: 'missing bearer token' });
    const body = invokeBody();
    const withInvokeToken = await handler(
      new Request(`http://app${MANIFEST_PATH}`, { headers: { authorization: `Bearer ${temps.signToken(claimsFor(body))}` } }),
    );
    expect(withInvokeToken.status).toBe(401);
  });

  it('refuses an invocation whose token was issued for another body, run or lease, before running code', async () => {
    const { temps, handler } = setup();
    const body = invokeBody();
    const tampered = invokeBody().replace('ada', 'eve');
    for (const request of [
      post(temps, tampered, claimsFor(body)),
      post(temps, body, claimsFor(body, { lease: 2 })),
      post(temps, body, claimsFor(body, { run: 'run-2' })),
      post(temps, body, claimsFor(body, { exp: Math.floor(Date.now() / 1000) - 1 })),
      post(temps, body, claimsFor(body, { typ: 'manifest' })),
    ]) {
      const response = await handler(request);
      expect(response.status).toBe(401);
    }
    expect(ran).toBe(0);
  });

  it('refuses another protocol version and answers an unknown workflow with a failed outcome', async () => {
    const { temps, handler } = setup();
    const v2 = invokeBody(1, 2);
    const response = await handler(post(temps, v2, claimsFor(v2)));
    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ title: 'Unsupported Protocol Version' });

    const other = invokeBody().replace('"workflow":"hello"', '"workflow":"goodbye"');
    const reply = await handler(post(temps, other, claimsFor(other)));
    expect(reply.status).toBe(200);
    expect(await reply.json()).toEqual({
      outcome: { type: 'failed', error: "Workflow 'goodbye' is not defined in this deployment (it defines: hello)" },
    });
    expect(ran).toBe(0);
  });

  it('runs an invocation end to end and answers with its outcome', async () => {
    const { temps } = setup();
    const { run } = temps.start('hello', { name: 'ada' });
    await temps.settle();
    expect(run.status).toBe('completed');
    expect(run.output).toBe('hello ada');
    expect(temps.invocations).toEqual([{ runId: run.id, lease: 1, status: 200 }]);
  });

  it('rejects wrong methods and unknown paths', async () => {
    const { handler } = setup();
    expect((await handler(new Request(`http://app${INVOKE_PATH}`))).status).toBe(405);
    expect((await handler(new Request(`http://app${MANIFEST_PATH}`, { method: 'POST' }))).status).toBe(405);
    expect((await handler(new Request('http://app/.well-known/temps/workflows/other'))).status).toBe(404);
  });

  it('fetches the invocation key from Temps once and caches it', async () => {
    const temps = new FakeOrchestrator();
    let keyReads = 0;
    const handler = createWorkflowHandler({
      workflows: [hello],
      apiUrl: FAKE_API_URL,
      tracer: false,
      fetch: (input, init) => {
        if (String(input).endsWith('/workflows/v1/invocation-key')) keyReads += 1;
        return temps.fetch(input, init);
      },
    });
    temps.attach(handler);
    await temps.manifest();
    await temps.manifest();
    expect(keyReads).toBe(1);
  });

  it('explains a missing TEMPS_API_URL and refuses duplicate workflows', () => {
    const saved = process.env.TEMPS_API_URL;
    delete process.env.TEMPS_API_URL;
    try {
      expect(() => createWorkflowHandler({ workflows: [hello] })).toThrow(/TEMPS_API_URL is not set/);
    } finally {
      if (saved !== undefined) process.env.TEMPS_API_URL = saved;
    }
    expect(() => createWorkflowHandler({ workflows: [hello, hello], apiUrl: 'http://t' })).toThrow(/defined twice/);
  });
});
