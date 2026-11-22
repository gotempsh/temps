// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';

import { createWorkflowHandler } from './handler.js';
import type { StepWriteRequest } from './protocol.js';
import type { SpanLike, TracerLike } from './telemetry.js';
import { FAKE_API_URL, FakeOrchestrator } from './testing/fake-orchestrator.js';
import { createTestRunner } from './testing/index.js';
import type { FetchLike } from './transport.js';
import { defineWorkflow } from './workflow.js';

class RecordingTracer implements TracerLike {
  spans: { name: string; attributes: Record<string, unknown>; status?: number; ended: boolean }[] = [];
  startActiveSpan<T>(name: string, fn: (span: SpanLike) => T): T {
    const record = { name, attributes: {} as Record<string, unknown>, status: undefined as number | undefined, ended: false };
    this.spans.push(record);
    return fn({
      setAttribute: (key, value) => (record.attributes[key] = value),
      recordException: () => undefined,
      setStatus: (status) => (record.status = status.code),
      spanContext: () => ({ traceId: '4bf92f3577b34da6a3ce929d0e0e4736' }),
      end: () => (record.ended = true),
    });
  }
}

describe('telemetry', () => {
  it('traces each attempt, links trace and error ids, and keeps payloads out of spans', async () => {
    const tracer = new RecordingTracer();
    const captured: unknown[] = [];
    const flaky = defineWorkflow('flaky', async (ctx) =>
      ctx.step('charge', ({ attempt }) => {
        if (attempt === 1) throw new Error('declined');
        return { secret: 'card-4242' };
      }, { retries: 1, initialBackoff: 10 }),
    );
    const temps = new FakeOrchestrator();
    const writes: StepWriteRequest[] = [];
    // Observe step writes on their way to Temps.
    const spy: FetchLike = async (input, init) => {
      if (String(input).endsWith('/steps')) writes.push(JSON.parse(String(init?.body)) as StepWriteRequest);
      return temps.fetch(input, init);
    };
    temps.attach(
      createWorkflowHandler({
        workflows: [flaky],
        apiUrl: FAKE_API_URL,
        fetch: spy,
        invocationKey: temps.invocationKey,
        tracer,
        errorTracker: {
          captureException: (error) => {
            captured.push(error);
            return 'evt_1';
          },
        },
      }),
    );
    const { run } = temps.start('flaky', { card: 'card-4242' });
    await temps.settle();
    await temps.advance(20);

    expect(run.status).toBe('completed');
    expect(tracer.spans.map((span) => [span.name, span.status, span.ended])).toEqual([
      ['workflow.step charge', 2, true],
      ['workflow.step charge', undefined, true],
    ]);
    expect(tracer.spans[0]?.attributes).toEqual({
      'workflow.run_id': run.id,
      'workflow.name': 'flaky',
      'workflow.step': 'charge',
      'workflow.step.sequence': 0,
      'workflow.step.attempt': 1,
    });
    expect(JSON.stringify(tracer.spans)).not.toContain('card-4242');
    expect(captured).toHaveLength(1);
    expect(writes.map((write) => [write.status, write.trace_id, write.error_ref ?? null])).toEqual([
      ['pending', '4bf92f3577b34da6a3ce929d0e0e4736', 'evt_1'],
      ['completed', '4bf92f3577b34da6a3ce929d0e0e4736', null],
    ]);
  });

  it('never fails a step because the error tracker failed', async () => {
    const failing = defineWorkflow('failing', async (ctx) =>
      ctx.step('x', () => {
        throw new Error('boom');
      }, { retries: 0 }),
    );
    const runner = createTestRunner({
      workflows: [failing],
      telemetry: {
        tracer: false,
        errorTracker: {
          captureException: () => {
            throw new Error('tracker down');
          },
        },
      },
    });
    const run = await runner.start('failing');
    await runner.settle();
    const done = await runner.run(run.id);
    expect(done.status).toBe('failed');
    expect(done.error).toMatch(/boom/);
  });
});
