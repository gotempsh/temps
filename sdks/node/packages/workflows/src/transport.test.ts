// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';

import { StaleLeaseError, TempsApiError, WorkflowIncompatibleError } from './errors.js';
import { ExecutorTransport } from './transport.js';

function problem(status: number, title: string, detail: string): Response {
  return new Response(JSON.stringify({ status, title, detail }), { status });
}

const write = { sequence: 2, name: 'charge', kind: 'step' as const, status: 'completed' as const, attempts: 1 };

describe('executor transport', () => {
  it('retries a transient failure and sends the token and protocol version', async () => {
    const seen: RequestInit[] = [];
    let calls = 0;
    const transport = new ExecutorTransport({
      apiUrl: 'http://temps.test/api/',
      backoffMs: 1,
      fetch: async (url, init) => {
        expect(url).toBe('http://temps.test/api/workflow-runs/r1/heartbeat');
        seen.push(init ?? {});
        calls += 1;
        if (calls === 1) throw new TypeError('fetch failed');
        if (calls === 2) return problem(503, 'Unavailable', 'restarting');
        return new Response(JSON.stringify({ lease_expires_at: 'x', cancelled: false }));
      },
    });
    await expect(transport.heartbeat('tok', 'r1')).resolves.toEqual({ lease_expires_at: 'x', cancelled: false });
    expect(calls).toBe(3);
    const headers = seen[0]?.headers as Record<string, string>;
    expect(headers.authorization).toBe('Bearer tok');
    expect(headers['temps-workflows-protocol']).toBe('1');
  });

  it('turns a stale lease and an incompatible step into typed errors', async () => {
    const stale = new ExecutorTransport({
      apiUrl: 'http://t/api',
      fetch: async () => problem(409, 'Stale Lease', 'Run r1 is not held by lease 3'),
    });
    await expect(stale.recordStep('tok', 'r1', write)).rejects.toBeInstanceOf(StaleLeaseError);

    const incompatible = new ExecutorTransport({
      apiUrl: 'http://t/api',
      fetch: async () =>
        problem(409, 'Workflow Incompatible', 'WorkflowIncompatible { run_id: r1, step: 2, expected: "charge", found: "refund" }'),
    });
    const error = await incompatible.recordStep('tok', 'r1', write).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(WorkflowIncompatibleError);
    expect(error).toMatchObject({ sequence: 2, expected: 'charge', found: 'refund' });
  });

  it('does not retry a client error and reports it with its problem title', async () => {
    let calls = 0;
    const transport = new ExecutorTransport({
      apiUrl: 'http://t/api',
      fetch: async () => {
        calls += 1;
        return problem(429, 'Workflow Quota Exceeded', 'max_steps_per_run');
      },
    });
    const error = await transport.recordStep('tok', 'r1', write).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(TempsApiError);
    expect(error).toMatchObject({ status: 429, title: 'Workflow Quota Exceeded' });
    expect(calls).toBe(1);
  });

  it('times out an attempt that Temps never answers', async () => {
    const transport = new ExecutorTransport({
      apiUrl: 'http://t/api',
      attempts: 1,
      timeoutMs: 50,
      fetch: (_input, init) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => reject(init.signal?.reason));
        }),
    });
    await expect(transport.heartbeat('t', 'r')).rejects.toMatchObject({ status: 0, title: 'Unreachable' });
  });

  it('gives up after its attempts when Temps stays unreachable', async () => {
    const transport = new ExecutorTransport({
      apiUrl: 'http://t/api',
      attempts: 2,
      backoffMs: 1,
      fetch: async () => {
        throw new TypeError('connection refused');
      },
    });
    await expect(transport.invocationKey('twf_x')).rejects.toMatchObject({ status: 0, title: 'Unreachable' });
  });
});
