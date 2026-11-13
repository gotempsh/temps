// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';

import { gate } from './conformance/fixtures.js';
import { StepFailedError } from './errors.js';
import type { InvokeRequest } from './protocol.js';
import { backoffMs, runInvocation } from './replay.js';
import { Telemetry } from './telemetry.js';
import { createTestRunner } from './testing/index.js';
import { ExecutorTransport } from './transport.js';
import { defineWorkflow } from './workflow.js';

function invokeRequest(workflow: string, windowMs: number): InvokeRequest {
  const now = new Date();
  return {
    protocol_version: 1,
    run_id: 'r',
    workflow,
    attempt: 1,
    lease: 1,
    now: now.toISOString(),
    deadline_at: new Date(now.getTime() + windowMs).toISOString(),
    lease_expires_at: new Date(now.getTime() + 120_000).toISOString(),
    input: null,
    journal: [],
  };
}

/** A Temps that records every step write and echoes it back as the journal entry. */
function recordingTemps(writes: number[]) {
  return async (_input: string, init?: RequestInit): Promise<Response> => {
    const write = JSON.parse(String(init?.body)) as { sequence: number; name: string; kind: string; status: string; attempts: number; result?: unknown };
    writes.push(write.sequence);
    return new Response(
      JSON.stringify({
        entry: { ...write, result: write.result ?? null, error: null, wake_at: null },
        already_recorded: false,
        lease_expires_at: new Date(Date.now() + 120_000).toISOString(),
      }),
      { status: 200 },
    );
  };
}

describe('replay', () => {
  it('pages through a journal larger than one invoke request', async () => {
    const counts = new Map<string, number>();
    const many = defineWorkflow('many', async (ctx) => {
      const values: number[] = [];
      for (let index = 0; index < 5; index += 1) {
        values.push(await ctx.step(`s${index}`, () => {
          counts.set(`s${index}`, (counts.get(`s${index}`) ?? 0) + 1);
          return index;
        }));
      }
      await ctx.sleep('rest', 1000);
      return values;
    });
    const runner = createTestRunner({ workflows: [many], journalPageSize: 2 });
    const run = await runner.start('many');
    await runner.settle();
    await runner.advance(2000);
    const done = await runner.run(run.id);
    expect(done.status).toBe('completed');
    expect(done.output).toEqual([0, 1, 2, 3, 4]);
    expect([...counts.values()]).toEqual([1, 1, 1, 1, 1]);
  });

  it('keeps a run suspended even when the workflow catches the suspension', async () => {
    let after = 0;
    const sneaky = defineWorkflow('sneaky', async (ctx) => {
      try {
        await ctx.sleep('nap', 60_000);
      } catch {
        // A catch-all must not let the workflow run past its sleep.
      }
      await ctx.step('after', () => (after += 1));
      return 'done';
    });
    const runner = createTestRunner({ workflows: [sneaky] });
    const run = await runner.start('sneaky');
    await runner.settle();
    expect((await runner.run(run.id)).status).toBe('sleeping');
    expect(after).toBe(0);
    await runner.advance(60_000);
    expect((await runner.run(run.id)).status).toBe('completed');
    expect(after).toBe(1);
  });

  it('lets a workflow handle a failed step and carry on', async () => {
    const handled = defineWorkflow('handled', async (ctx) => {
      try {
        await ctx.step('charge', () => {
          throw new Error('card declined');
        }, { retries: 0 });
      } catch (error) {
        if (!(error instanceof StepFailedError)) throw error;
        return ctx.step('notify', () => `notified: ${error.reason}`);
      }
      return 'charged';
    });
    const runner = createTestRunner({ workflows: [handled] });
    const run = await runner.start('handled');
    await runner.settle();
    const done = await runner.run(run.id);
    expect(done.status).toBe('completed');
    expect(done.output).toBe('notified: card declined');
    expect(done.journal.map((entry) => entry.status)).toEqual(['failed', 'completed']);
  });

  it('fails an attempt that exceeds its timeout and retries it', async () => {
    let attempts = 0;
    const slow = defineWorkflow('slow', async (ctx) =>
      ctx.step('hang', async ({ attempt, signal }) => {
        attempts += 1;
        if (attempt === 1) {
          await new Promise((_, reject) => signal.addEventListener('abort', () => reject(new Error('aborted'))));
        }
        return attempt;
      }, { timeout: 30, retries: 1, initialBackoff: 10 }),
    );
    const runner = createTestRunner({ workflows: [slow] });
    const run = await runner.start('slow');
    await runner.settle();
    const retrying = await runner.run(run.id);
    expect(retrying.status).toBe('retrying');
    expect(retrying.journal[0]?.error).toMatch(/timed out after 30 ms/);
    await runner.advance(50);
    expect((await runner.run(run.id)).output).toBe(2);
    expect(attempts).toBe(2);
  });

  it('stops a cancelled run at its next step', async () => {
    const held = gate();
    let second = 0;
    const cancellable = defineWorkflow('cancellable', async (ctx) => {
      await ctx.step('first', () => held.wait());
      await ctx.step('second', () => (second += 1));
    });
    const runner = createTestRunner({ workflows: [cancellable] });
    const run = await runner.start('cancellable');
    runner.orchestrator.kick();
    await new Promise((resolve) => setTimeout(resolve, 20));
    runner.cancel(run.id);
    held.open();
    await runner.settle();
    expect((await runner.run(run.id)).status).toBe('cancelled');
    expect(second).toBe(0);
    expect(runner.orchestrator.invocations.at(-1)?.status).toBe(202);
  });

  it('sends an event once, even when replayed, and it wakes the waiting run', async () => {
    const waiter = defineWorkflow('waiter', async (ctx) => (await ctx.waitForEvent('go', { event: 'go', key: 'k' })).payload);
    const sender = defineWorkflow('sender', async (ctx) => {
      await ctx.sendEvent('notify', 'go', { key: 'k', payload: { n: 1 } });
      await ctx.sleep('linger', 1000);
    });
    const runner = createTestRunner({ workflows: [waiter, sender] });
    const waiting = await runner.start('waiter');
    await runner.settle();
    const sending = await runner.start('sender');
    await runner.settle();
    await runner.advance(1000);
    expect((await runner.run(sending.id)).status).toBe('completed');
    expect((await runner.run(waiting.id)).output).toEqual({ n: 1 });
    expect(runner.orchestrator.sendEvent('go', 'k').delivered).toBe(0);
  });

  it('sleeps until a date, and not at all for a date in the past', async () => {
    const dated = defineWorkflow('dated', async (ctx) => {
      await ctx.sleepUntil('past', new Date(0));
      await ctx.sleepUntil('future', (ctx.input as { at: string }).at);
      return 'woke';
    });
    const runner = createTestRunner({ workflows: [dated] });
    const run = await runner.start('dated', { at: new Date(runner.orchestrator.now + 5000).toISOString() });
    await runner.settle();
    expect((await runner.run(run.id)).status).toBe('sleeping');
    await runner.advance(5000);
    expect((await runner.run(run.id)).output).toBe('woke');
  });

  it('re-runs only the steps after the cut on retry from a step', async () => {
    const counts = { a: 0, b: 0 };
    const twoSteps = defineWorkflow('two-steps', async (ctx) => {
      await ctx.step('a', () => (counts.a += 1));
      return ctx.step('b', () => (counts.b += 1));
    });
    const runner = createTestRunner({ workflows: [twoSteps] });
    const run = await runner.start('two-steps');
    await runner.settle();
    await runner.retryFrom(run.id, 1);
    expect((await runner.run(run.id)).output).toBe(2);
    expect(counts).toEqual({ a: 1, b: 2 });
  });

  it('fails the run with a clear reason for an invalid step name or output', async () => {
    const badName = defineWorkflow('bad-name', async (ctx) => ctx.step('has space', () => 1));
    const badOutput = defineWorkflow('bad-output', async () => ({ big: 1n }));
    const runner = createTestRunner({ workflows: [badName, badOutput] });
    const one = await runner.start('bad-name');
    const two = await runner.start('bad-output');
    await runner.settle();
    expect((await runner.run(one.id)).error).toMatch(/Step name 'has space' is invalid/);
    expect((await runner.run(two.id)).error).toMatch(/The workflow output is not JSON-serializable/);
  });

  it('works within a short deadline and yields before work it cannot finish by it', async () => {
    let ranNext = false;
    const twoSteps = defineWorkflow('two-steps', async (ctx) => {
      await ctx.step('slow', async () => {
        await new Promise((resolve) => setTimeout(resolve, 350));
        return 1;
      });
      return ctx.step('next', () => (ranNext = true));
    });
    // A 400 ms window: a fixed 30 s margin would never let the first step start.
    const writes: number[] = [];
    const transport = new ExecutorTransport({ apiUrl: 'http://t', fetch: recordingTemps(writes) });
    const result = await runInvocation(twoSteps, invokeRequest('two-steps', 400), {
      transport,
      telemetry: new Telemetry({ tracer: false }),
      token: 't',
    });
    expect(writes).toEqual([0]);
    expect(result).toMatchObject({ type: 'outcome', outcome: { type: 'retry', sequence: 1 } });
    expect(ranNext).toBe(false);
  });

  it('suspends as unavailable, not failed, when Temps cannot record a step', async () => {
    const quick = defineWorkflow('quick', async (ctx) => ctx.step('work', () => 1));
    for (const status of [500, 503]) {
      const transport = new ExecutorTransport({
        apiUrl: 'http://t',
        attempts: 1,
        fetch: async () => new Response(JSON.stringify({ title: 'Server Error' }), { status }),
      });
      const result = await runInvocation(quick, invokeRequest('quick', 60_000), {
        transport,
        telemetry: new Telemetry({ tracer: false }),
        token: 't',
      });
      expect(result).toMatchObject({ type: 'unavailable' });
    }
    // A refusal about the run itself still fails it, with the reason.
    const refusing = new ExecutorTransport({
      apiUrl: 'http://t',
      fetch: async () =>
        new Response(JSON.stringify({ title: 'Payload Too Large', detail: 'step result over 4194304 bytes' }), { status: 413 }),
    });
    const failed = await runInvocation(quick, invokeRequest('quick', 60_000), {
      transport: refusing,
      telemetry: new Telemetry({ tracer: false }),
      token: 't',
    });
    expect(failed).toMatchObject({ type: 'outcome', outcome: { type: 'failed', error: expect.stringMatching(/Payload Too Large/) } });
  });

  it('sends one heartbeat at a time while Temps is slow to answer', async () => {
    let heartbeats = 0;
    const slow = defineWorkflow('slow', async (ctx) =>
      ctx.step('wait', () => new Promise((resolve) => setTimeout(() => resolve(1), 200))),
    );
    const writes: number[] = [];
    const record = recordingTemps(writes);
    const transport = new ExecutorTransport({
      apiUrl: 'http://t',
      timeoutMs: 5_000,
      fetch: async (input, init) => {
        if (String(input).endsWith('/heartbeat')) {
          heartbeats += 1;
          return new Promise<Response>(() => {});
        }
        return record(input, init);
      },
    });
    const result = await runInvocation(slow, invokeRequest('slow', 60_000), {
      transport,
      telemetry: new Telemetry({ tracer: false }),
      token: 't',
      heartbeatMs: 10,
    });
    expect(result).toMatchObject({ type: 'outcome', outcome: { type: 'completed' } });
    expect(heartbeats).toBe(1);
  });

  it('backs off exponentially up to its cap', () => {
    expect([1, 2, 3, 4].map((attempt) => backoffMs({}, attempt))).toEqual([1000, 2000, 4000, 8000]);
    expect(backoffMs({ initialBackoff: '1m', maxBackoff: '2m' }, 5)).toBe(120_000);
  });
});
