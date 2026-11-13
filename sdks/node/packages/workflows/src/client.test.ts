// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';

import { WorkflowsClient } from './client.js';
import { TempsApiError } from './errors.js';
import { FAKE_API_URL } from './testing/fake-orchestrator.js';
import { createTestRunner } from './testing/index.js';
import { defineWorkflow } from './workflow.js';

const approval = defineWorkflow('approval', async (ctx) => {
  const decision = await ctx.waitForEvent('decision', { event: 'approval.decided', key: (ctx.input as { id: string }).id });
  return decision.payload;
});

describe('WorkflowsClient', () => {
  it('starts a run idempotently, sends its event and reads its result', async () => {
    const runner = createTestRunner({ workflows: [approval] });
    const first = await runner.client.start('approval', { id: 'req-1' }, { idempotencyKey: 'req-1' });
    const again = await runner.client.start('approval', { id: 'req-1' }, { idempotencyKey: 'req-1' });
    expect(first.created).toBe(true);
    expect(again).toMatchObject({ created: false, run: { id: first.run.id } });
    await runner.settle();
    expect((await runner.client.getRun(first.run.id)).status).toBe('waiting');
    expect(await runner.client.sendEvent('approval.decided', { key: 'req-1', payload: { ok: true } })).toMatchObject({ delivered: 1 });
    await runner.settle();
    const done = await runner.client.waitForRun(first.run.id, { timeoutMs: 1000, intervalMs: 10 });
    expect(done).toMatchObject({ status: 'completed', output: { ok: true } });
  });

  it('reports API errors with their problem title', async () => {
    const runner = createTestRunner({ workflows: [approval] });
    const error = await runner.client.getRun('missing').catch((e: unknown) => e);
    expect(error).toBeInstanceOf(TempsApiError);
    expect(error).toMatchObject({ status: 404, title: 'Workflow Run Not Found' });

    const stranger = new WorkflowsClient({ apiUrl: FAKE_API_URL, token: 'twf_wrong', fetch: runner.orchestrator.fetch });
    await expect(stranger.start('approval')).rejects.toMatchObject({ status: 401, title: 'Invalid Workflow Credential' });
  });

  it('says where to get the credential when it is missing', () => {
    const saved = process.env.TEMPS_WORKFLOWS_TOKEN;
    delete process.env.TEMPS_WORKFLOWS_TOKEN;
    try {
      expect(() => new WorkflowsClient({ apiUrl: 'http://t/api' })).toThrow(/Issue a workflow credential in the console/);
    } finally {
      if (saved !== undefined) process.env.TEMPS_WORKFLOWS_TOKEN = saved;
    }
  });
});

it('sends granted starts and events to the explicit destination without changing read scope', async () => {
  const urls: string[] = [];
  const client = new WorkflowsClient({ apiUrl: 'https://control.example.test/api', token: 'synthetic', fetch: async (url) => {
    urls.push(String(url));
    return Response.json(String(url).endsWith('/events') ? { event_id: 1, delivered: 0 } : { run: { id: 'r', status: 'queued' }, created: true });
  } });
  const target = { projectId: 4, environmentId: 8 };
  await client.start('orders', {}, { target });
  await client.sendEvent('paid', { target, payload: 1 });
  await client.getRun('r');
  expect(urls).toEqual([
    'https://control.example.test/api/workflows/v1/projects/4/environments/8/runs',
    'https://control.example.test/api/workflows/v1/projects/4/environments/8/events',
    'https://control.example.test/api/workflows/v1/runs/r',
  ]);
  await expect(client.start('orders', null, { target: { projectId: -1, environmentId: 8 } })).rejects.toThrow('positive integers');
});
