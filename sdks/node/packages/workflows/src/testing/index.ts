// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Test workflows without a Temps server: an in-memory orchestrator drives
 * the real handler, on a virtual clock.
 *
 * ```ts
 * import { createTestRunner } from '@temps-sdk/workflows/testing';
 *
 * const runner = createTestRunner({ workflows: [sendInvoice] });
 * const run = await runner.start('send-invoice', { orderId: 1 });
 * await runner.settle();                       // runs until it sleeps or waits
 * await runner.advance('3d');                  // the grace period passes
 * await runner.sendEvent('invoice.paid', { key: 'inv_1' });
 * expect((await runner.run(run.id)).status).toBe('completed');
 * ```
 */

import { WorkflowsClient } from '../client.js';
import { createWorkflowHandler, type FetchHandler, type WorkflowHandlerOptions } from '../handler.js';
import type { JournalEntry, JsonValue } from '../protocol.js';
import { durationMs, type Duration } from '../workflow.js';
import { FAKE_API_URL, FAKE_CREDENTIAL, FakeOrchestrator, type FakeOrchestratorOptions, type FakeRun } from './fake-orchestrator.js';

export { FakeOrchestrator, FAKE_API_URL, FAKE_CREDENTIAL } from './fake-orchestrator.js';
export type { FakeRun, FakeOrchestratorOptions } from './fake-orchestrator.js';

export interface TestRunnerOptions extends FakeOrchestratorOptions {
  codec?: WorkflowHandlerOptions['codec'];
  workflows: WorkflowHandlerOptions['workflows'];
  telemetry?: Pick<WorkflowHandlerOptions, 'tracer' | 'errorTracker'>;
}

export interface TestRun {
  id: string;
  status: FakeRun['status'];
  output: JsonValue | null;
  error: string | null;
  errorKind: string | null;
  journal: JournalEntry[];
}

export interface TestRunner {
  readonly orchestrator: FakeOrchestrator;
  readonly handler: FetchHandler;
  /** A `WorkflowsClient` wired to the in-memory orchestrator. */
  readonly client: WorkflowsClient;
  start(workflow: string, input?: JsonValue, options?: { idempotencyKey?: string }): Promise<TestRun>;
  run(runId: string): Promise<TestRun>;
  sendEvent(name: string, options?: { key?: string; payload?: JsonValue }): Promise<number>;
  /** Run invocations until every run is finished, sleeping or waiting. */
  settle(): Promise<void>;
  /** Move the virtual clock forward, then settle. */
  advance(duration: Duration): Promise<void>;
  cancel(runId: string): void;
  retryFrom(runId: string, sequence: number): Promise<void>;
}

export function createTestRunner(options: TestRunnerOptions): TestRunner {
  const orchestrator = new FakeOrchestrator(options);
  const handler = createWorkflowHandler({
    workflows: options.workflows,
    codec: options.codec,
    apiUrl: FAKE_API_URL,
    fetch: orchestrator.fetch,
    invocationKey: orchestrator.invocationKey,
    tracer: options.telemetry?.tracer ?? false,
    errorTracker: options.telemetry?.errorTracker,
  });
  orchestrator.attach(handler);
  const client = new WorkflowsClient({ apiUrl: FAKE_API_URL, token: FAKE_CREDENTIAL, fetch: orchestrator.fetch, codec: options.codec });
  const view = (run: FakeRun): TestRun => ({
    id: run.id,
    status: run.status,
    output: run.output,
    error: run.error,
    errorKind: run.errorKind,
    journal: structuredClone(run.journal),
  });
  const find = (runId: string): FakeRun => {
    const run = orchestrator.runs.get(runId);
    if (!run) throw new Error(`No run ${runId}`);
    return run;
  };
  return {
    orchestrator,
    handler,
    client,
    async start(workflow, input = null, startOptions = {}) {
      return view(orchestrator.start(workflow, input, startOptions.idempotencyKey).run);
    },
    async run(runId) {
      return view(find(runId));
    },
    async sendEvent(name, eventOptions = {}) {
      return orchestrator.sendEvent(name, eventOptions.key ?? null, eventOptions.payload ?? null).delivered;
    },
    settle: () => orchestrator.settle(),
    advance: (duration) => orchestrator.advance(durationMs(duration)),
    cancel: (runId) => orchestrator.cancel(runId),
    async retryFrom(runId, sequence) {
      orchestrator.retryFrom(runId, sequence);
      await orchestrator.settle();
    },
  };
}
