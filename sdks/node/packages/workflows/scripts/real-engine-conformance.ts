// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Run the conformance suite against the real engine.
 *
 * Started by the engine's test (`crates/temps-workflows/src/conformance_tests.rs`),
 * which serves the real public API and a test-only control API:
 *
 * - `TEMPS_API_URL`, `TEMPS_WORKFLOWS_TOKEN`: the engine, as an app sees it;
 * - `TEMPS_TEST_CONTROL_URL`: dispatch, settle, let time pass, expire a lease, read a journal;
 * - `EXECUTOR_PORT`: where to serve the fixture workflows; the engine invokes them there.
 *
 * Prints one line per case and `conformance: passed=N failed=M`; exits 1 on any failure.
 */

import { createServer } from 'node:http';

import { toNodeListener } from '../src/adapters.js';
import { WorkflowsClient } from '../src/client.js';
import { createConformanceFixtures } from '../src/conformance/fixtures.js';
import { conformanceCases, type ConformanceTarget } from '../src/conformance/suite.js';
import { createWorkflowHandler } from '../src/handler.js';
import type { JournalEntry } from '../src/protocol.js';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
}

const control = required('TEMPS_TEST_CONTROL_URL');
const port = Number(required('EXECUTOR_PORT'));
const client = new WorkflowsClient();

async function controlCall<T>(path: string, method: 'GET' | 'POST' = 'POST'): Promise<T> {
  const response = await fetch(`${control}${path}`, { method });
  if (!response.ok) throw new Error(`control ${path} failed with ${response.status}: ${await response.text()}`);
  return (await response.json()) as T;
}

const target: ConformanceTarget = {
  name: 'real engine',
  async start(workflow, input = null, idempotencyKey) {
    const started = await client.start(workflow, input, { idempotencyKey });
    return { runId: started.run.id, created: started.created };
  },
  async sendEvent(name, key, payload = null) {
    return (await client.sendEvent(name, { key, payload })).delivered;
  },
  async run(runId) {
    const run = await client.getRun(runId);
    return { status: run.status, output: run.output, error: run.error, errorKind: run.error_kind };
  },
  journal: (runId) => controlCall<JournalEntry[]>(`/runs/${runId}/journal`, 'GET'),
  async kick() {
    await controlCall('/kick');
  },
  async settle() {
    await controlCall('/settle');
  },
  async advance(ms) {
    await controlCall(`/advance?ms=${ms}`);
  },
  async expireLease(runId) {
    await controlCall(`/runs/${runId}/expire-lease`);
  },
};

const fixtures = createConformanceFixtures();
const handler = createWorkflowHandler({ workflows: fixtures.workflows, tracer: false, heartbeatMs: 1000 });
const server = createServer((request, response) => void toNodeListener(handler)(request, response));
await new Promise<void>((resolve) => server.listen(port, '127.0.0.1', resolve));

let passed = 0;
let failed = 0;
for (const testCase of conformanceCases) {
  fixtures.reset();
  const started = Date.now();
  try {
    await testCase.run(target, fixtures, handler);
    passed += 1;
    console.log(`ok   ${testCase.name} (${Date.now() - started} ms)`);
  } catch (error) {
    failed += 1;
    console.log(`FAIL ${testCase.name}: ${error instanceof Error ? (error.stack ?? error.message) : String(error)}`);
  }
}
console.log(`conformance: passed=${passed} failed=${failed}`);
server.close();
process.exit(failed === 0 ? 0 : 1);
