// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';
import { aesGcmCodec, decodeEntry, decodeError, encodeError, PayloadCodecError } from './codec.js';
import { WorkflowsClient } from './client.js';
import { createTestRunner, FAKE_API_URL, FAKE_CREDENTIAL } from './testing/index.js';
import { defineWorkflow } from './workflow.js';

const key = new Uint8Array(32).fill(7);
const codec = () => aesGcmCodec({ activeKeyId: 'one', keys: { one: key } });
const secret = 'synthetic-private-payload';

describe('app-held payload codec', () => {
  it('authenticates ciphertext, supports retained keys and rejects plaintext and unknown keys', async () => {
    const old = codec();
    const newer = aesGcmCodec({ activeKeyId: 'two', keys: { one: key, two: new Uint8Array(32).fill(8) } });
    const encrypted = await old.encode({ secret });
    expect(JSON.stringify(encrypted)).not.toContain(secret);
    expect(await newer.decode(encrypted)).toEqual({ secret });
    expect(await old.encode({ secret })).not.toEqual(encrypted);
    await expect(old.decode(await newer.encode(secret))).rejects.toBeInstanceOf(PayloadCodecError);
    await expect(old.decode({ secret })).rejects.toBeInstanceOf(PayloadCodecError);
    await expect(old.decode({ ...(encrypted as object), tag: Buffer.alloc(16).toString('base64') })).rejects.toBeInstanceOf(PayloadCodecError);
  });

  it('encrypts inputs, steps, events and outputs through suspension and paged replay', async () => {
    let calls = 0;
    const workflow = defineWorkflow('private', async (ctx) => {
      expect(ctx.input).toEqual({ secret });
      const value = await ctx.step('value', () => { calls++; return { secret }; });
      await ctx.now();
      await ctx.random();
      await ctx.sleep('nap', 1000);
      await ctx.sendEvent('emit', 'ready', { payload: value });
      const event = await ctx.waitForEvent('receive', { event: 'ready' });
      return event.payload;
    });
    const runner = createTestRunner({ workflows: [workflow], codec: codec(), journalPageSize: 1 });
    const started = await runner.client.start('private', { secret });
    await runner.settle();
    await runner.advance(2000);
    await runner.settle();
    expect(await runner.client.getRun(started.run.id)).toMatchObject({ status: 'completed', output: { secret } });
    expect(calls).toBe(1);
    expect(JSON.stringify(await runner.run(started.run.id))).not.toContain(secret);
    expect(JSON.stringify(runner.orchestrator.runs.get(started.run.id)?.input)).not.toContain(secret);
  });

  it('keeps retry errors and final failures encrypted while replay returns their original messages', async () => {
    const workflow = defineWorkflow('fails', async (ctx) => ctx.step('fail', () => { throw new Error(secret); }, { retries: 1 }));
    const runner = createTestRunner({ workflows: [workflow], codec: codec() });
    const started = await runner.client.start('fails');
    await runner.settle();
    expect((await runner.run(started.run.id)).status).toBe('retrying');
    expect(JSON.stringify(await runner.run(started.run.id))).not.toContain(secret);
    await runner.advance(2000);
    expect(await runner.client.getRun(started.run.id)).toMatchObject({ status: 'failed', error: expect.stringContaining(secret) });
    expect(JSON.stringify(await runner.run(started.run.id))).not.toContain(secret);
  });

  it('keeps large errors decryptable and prevents telemetry from leaking their contents', async () => {
    const c = codec();
    const error = await encodeError(c, '\u0001'.repeat(16000));
    expect(Buffer.byteLength(JSON.stringify(error))).toBeLessThan(64 * 1024);
    expect(await decodeError(c, error)).toContain('\u0001');
    const captured: unknown[] = [];
    const child = defineWorkflow('child', () => { throw new Error(secret); });
    const parent = defineWorkflow('parent', async (ctx) => {
      await ctx.step('failed-attempt', () => { throw new Error(secret); }, { retries: 0 }).catch(() => {});
      return await ctx.child('child-run', 'child', { secret });
    });
    const runner = createTestRunner({ workflows: [parent, child], codec: c,
      telemetry: { errorTracker: { captureException(error) { captured.push(String(error)); } } } });
    const started = await runner.client.start('parent');
    await runner.settle();
    expect(await runner.client.getRun(started.run.id)).toMatchObject({ status: 'completed', output: { status: 'failed', error: secret } });
    expect(captured).toHaveLength(1);
    expect(JSON.stringify(captured)).not.toContain(secret);
    expect(JSON.stringify([...runner.orchestrator.runs.values()])).not.toContain(secret);
  });

  it('decodes child results without encrypting their scheduling metadata', async () => {
    const c = codec();
    const entry = await decodeEntry(c, { sequence: 0, name: 'child', kind: 'child', status: 'completed', attempts: 1,
      result: { run_id: 'child-id', status: 'failed', output: await c.encode({ secret }), error: await encodeError(c, secret) } });
    expect(entry.result).toEqual({ run_id: 'child-id', status: 'failed', output: { secret }, error: secret });
  });

  it('fails closed before running user code when the app has the wrong key', async () => {
    let executed = false;
    const workflow = defineWorkflow('private', async () => { executed = true; return secret; });
    const runner = createTestRunner({ workflows: [workflow], codec: codec() });
    const client = new WorkflowsClient({ apiUrl: FAKE_API_URL, token: FAKE_CREDENTIAL, fetch: runner.orchestrator.fetch,
      codec: aesGcmCodec({ activeKeyId: 'one', keys: { one: new Uint8Array(32).fill(9) } }) });
    const started = await client.start('private', secret);
    // Drive one invocation: a 503 deliberately remains retryable.
    await runner.orchestrator.settle();
    expect(executed).toBe(false);
    expect((await runner.run(started.run.id)).status).not.toBe('completed');
    expect(JSON.stringify(await runner.run(started.run.id))).not.toContain(secret);
  });
});
