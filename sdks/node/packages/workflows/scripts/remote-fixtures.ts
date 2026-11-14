// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ConformanceFixtures } from '../src/conformance/fixtures.js';
import type { FetchHandler } from '../src/handler.js';

/** Test-only bridge: run the identical conformance cases against another SDK. */
export async function remoteFixtures(base: string) {
  const effects = new Map<string, number>();
  let pending = Promise.resolve();
  let variant: 'a' | 'b' = 'a';
  async function call(path: string, method = 'POST') {
    const response = await fetch(`${base}/__fixture/${path}`, { method, signal: AbortSignal.timeout(5000) });
    if (!response.ok) throw new Error(`fixture ${path}: HTTP ${response.status}`);
    return response.json();
  }
  const ready = Date.now() + 60_000;
  for (;;) {
    try { await call('state', 'GET'); break; }
    catch (error) { if (Date.now() >= ready) throw error; await Bun.sleep(100); }
  }
  async function refresh() {
    await pending;
    const state = await call('state', 'GET') as Record<string, number>;
    effects.clear();
    for (const [key, value] of Object.entries(state)) effects.set(key, value);
  }
  const timer = setInterval(() => { void refresh().catch(() => {}); }, 20);
  const fixtures: ConformanceFixtures = {
    workflows: [], effects,
    variant: {
      get current() { return variant; },
      set current(value) { variant = value; pending = pending.then(() => call(`variant/${value}`)).then(() => {}); },
    },
    gated: { gate: { wait: async () => {}, open() { pending = pending.then(() => call('gate')).then(() => {}); } } },
    async reset() { await pending; await call('reset'); effects.clear(); variant = 'a'; },
  };
  const handler: FetchHandler = async (request) => {
    const path = new URL(request.url).pathname;
    return fetch(`${base}${path}`, { method: request.method, headers: request.headers, body: await request.arrayBuffer() });
  };
  return { fixtures, handler, refresh, flush: () => pending, close: () => clearInterval(timer) };
}
