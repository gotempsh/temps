// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The worker the engine's end-to-end Docker test runs in a container
 * (`crates/temps-workflows/src/e2e_docker_tests.rs`), bundled for Node with
 * `bun build --target=node`.
 *
 * Every execution of a step body is counted on the test's host
 * (`E2E_CONTROL_URL/effects/<step>`), so the counts survive the container
 * being killed. The `charge` step holds until the test opens the gate, so
 * the test can kill the container while the step is in flight.
 *
 * - `TEMPS_API_URL`: the engine's public API, as the app sees it;
 * - `E2E_CONTROL_URL`: the test's effect counters and gate;
 * - `PORT`: where to serve the workflows (default 8080).
 */

import { createServer } from 'node:http';

import { toNodeListener } from '../src/adapters.js';
import { createWorkflowHandler } from '../src/handler.js';
import { defineWorkflow } from '../src/workflow.js';

const control = process.env.E2E_CONTROL_URL;
if (!control) throw new Error('E2E_CONTROL_URL is not set');
const port = Number(process.env.PORT ?? 8080);

async function effect(step: string): Promise<number> {
  const response = await fetch(`${control}/effects/${step}`, { method: 'POST' });
  if (!response.ok) throw new Error(`counting ${step} failed with ${response.status}`);
  return Number(await response.json());
}

async function gateOpen(): Promise<boolean> {
  const response = await fetch(`${control}/gate`);
  return response.ok && (await response.json()) === true;
}

const order = defineWorkflow('e2e-order', async (ctx) => {
  const { order: orderId } = ctx.input as { order: string };
  const reservation = await ctx.step('reserve', () => effect('reserve'));
  const charge = await ctx.step('charge', async () => {
    const count = await effect('charge');
    while (!(await gateOpen())) await new Promise((resolve) => setTimeout(resolve, 100));
    return count;
  });
  await ctx.sleep('cool-off', '1h');
  const shipped = await ctx.waitForEvent('shipped', { event: 'order.shipped', key: orderId, timeout: '1d' });
  const notified = await ctx.step('notify', () => effect('notify'));
  return { reservation, charge, shipped, notified };
});

const handler = createWorkflowHandler({ workflows: [order], tracer: false, heartbeatMs: 1000 });
const server = createServer((request, response) => void toNodeListener(handler)(request, response));
server.listen(port, '0.0.0.0', () => console.log(`e2e worker listening on ${port}`));
