// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it } from 'vitest';

import { createWorkflowHandler } from '../handler.js';
import { FAKE_API_URL, FakeOrchestrator } from '../testing/fake-orchestrator.js';
import { createConformanceFixtures } from './fixtures.js';
import { conformanceCases, type ConformanceTarget } from './suite.js';

function fakeTarget(orchestrator: FakeOrchestrator): ConformanceTarget {
  const find = (runId: string) => {
    const run = orchestrator.runs.get(runId);
    if (!run) throw new Error(`no run ${runId}`);
    return run;
  };
  return {
    name: 'fake orchestrator',
    async start(workflow, input = null, idempotencyKey) {
      const { run, created } = orchestrator.start(workflow, input, idempotencyKey);
      return { runId: run.id, created };
    },
    async sendEvent(name, key, payload = null) {
      return orchestrator.sendEvent(name, key ?? null, payload).delivered;
    },
    async run(runId) {
      const run = find(runId);
      return { status: run.status, output: run.output, error: run.error, errorKind: run.errorKind };
    },
    async journal(runId) {
      return structuredClone(find(runId).journal);
    },
    async kick() {
      orchestrator.kick();
    },
    settle: () => orchestrator.settle(),
    advance: (ms) => orchestrator.advance(ms),
    async expireLease(runId) {
      orchestrator.expireLease(runId);
    },
  };
}

describe('executor protocol conformance (fake orchestrator)', () => {
  for (const testCase of conformanceCases) {
    it(testCase.name, async () => {
      const fixtures = createConformanceFixtures();
      const orchestrator = new FakeOrchestrator();
      const handler = createWorkflowHandler({
        workflows: fixtures.workflows,
        apiUrl: FAKE_API_URL,
        fetch: orchestrator.fetch,
        invocationKey: orchestrator.invocationKey,
        tracer: false,
      });
      orchestrator.attach(handler);
      await testCase.run(fakeTarget(orchestrator), fixtures, handler);
    });
  }
});
