// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The executor protocol conformance suite (version 1).
 *
 * Each case drives the SDK through an orchestrator: the in-memory fake
 * (`fake.test.ts`) or the real engine (`scripts/real-engine-conformance.ts`,
 * run by the engine's test). Both must pass: the fake proves the SDK against
 * the specification, the real engine proves the specification is the
 * engine's.
 */

import assert from 'node:assert/strict';
import { generateKeyPairSync, sign } from 'node:crypto';

import type { FetchHandler } from '../handler.js';
import { INVOKE_PATH, PROTOCOL_VERSION, TOKEN_VERSION, type JournalEntry, type JsonValue, type RunStatus } from '../protocol.js';
import { RETRY_BACKOFF_MS, SLEEP_MS, WAIT_TIMEOUT_MS, type ConformanceFixtures } from './fixtures.js';

export interface RunView {
  status: RunStatus;
  output: JsonValue | null | undefined;
  error: string | null | undefined;
  errorKind: string | null | undefined;
}

/** What a case needs from an orchestrator. */
export interface ConformanceTarget {
  readonly name: string;
  start(workflow: string, input?: JsonValue, idempotencyKey?: string): Promise<{ runId: string; created: boolean }>;
  sendEvent(name: string, key?: string, payload?: JsonValue): Promise<number>;
  run(runId: string): Promise<RunView>;
  journal(runId: string): Promise<JournalEntry[]>;
  /** Start due invocations without waiting for them. */
  kick(): Promise<void>;
  /** Until nothing is due and nothing is in flight. */
  settle(): Promise<void>;
  /** Let `ms` pass for timers (sleeps, retry backoff, wait timeouts), then settle. */
  advance(ms: number): Promise<void>;
  /** The lease of a running run expires, as when its replica dies. */
  expireLease(runId: string): Promise<void>;
}

export interface ConformanceCase {
  name: string;
  run(target: ConformanceTarget, fixtures: ConformanceFixtures, handler: FetchHandler): Promise<void>;
}

const unique = () => Math.random().toString(36).slice(2, 10);

async function until(what: string, check: () => boolean | Promise<boolean>, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!(await check())) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

export const conformanceCases: ConformanceCase[] = [
  {
    name: 'replay: recorded steps are not run again after a suspension',
    async run(target, fixtures) {
      const { runId } = await target.start('conf-replay', { case: unique() });
      await target.settle();
      assert.equal((await target.run(runId)).status, 'sleeping');
      assert.equal(fixtures.effects.get('conf-replay/a'), 1);

      await target.advance(SLEEP_MS + 500);
      const run = await target.run(runId);
      assert.equal(run.status, 'completed', `run ended ${run.status}: ${run.error}`);
      assert.equal(fixtures.effects.get('conf-replay/a'), 1, 'step a ran again on replay');
      assert.equal(fixtures.effects.get('conf-replay/b'), 1);

      const journal = await target.journal(runId);
      assert.deepEqual(
        journal.map((entry) => [entry.sequence, entry.name, entry.kind, entry.status]),
        [
          [0, 'a', 'step', 'completed'],
          [1, 'pause', 'sleep', 'completed'],
          [2, 'b', 'step', 'completed'],
          [3, 'now', 'now', 'completed'],
          [4, 'random', 'random', 'completed'],
        ],
      );
      const output = run.output as { now: string; random: number };
      assert.equal(new Date(String(journal[3]?.result)).toISOString(), output.now, 'now() was not the recorded value');
      assert.equal(journal[4]?.result, output.random, 'random() was not the recorded value');
    },
  },
  {
    name: 'retries: a failing step is retried with backoff and its attempts recorded',
    async run(target, fixtures) {
      const { runId } = await target.start('conf-retry', { case: unique() });
      await target.settle();
      assert.equal((await target.run(runId)).status, 'retrying');
      await target.advance(RETRY_BACKOFF_MS + 300);
      await target.advance(RETRY_BACKOFF_MS * 2 + 300);
      const run = await target.run(runId);
      assert.equal(run.status, 'completed', `run ended ${run.status}: ${run.error}`);
      assert.deepEqual(run.output, { attempt: 3 });
      assert.equal(fixtures.effects.get('conf-retry/flaky'), 3);
      const [entry] = await target.journal(runId);
      assert.equal(entry?.status, 'completed');
      assert.equal(entry?.attempts, 3);
    },
  },
  {
    name: 'retries: a step out of attempts fails the run with the step named',
    async run(target, fixtures) {
      const { runId } = await target.start('conf-retry-exhausted', { case: unique() });
      await target.settle();
      await target.advance(RETRY_BACKOFF_MS + 300);
      const run = await target.run(runId);
      assert.equal(run.status, 'failed');
      assert.equal(run.errorKind, 'step_failed');
      assert.match(String(run.error), /always-fails.*the upstream is down/);
      assert.equal(fixtures.effects.get('conf-retry-exhausted/always-fails'), 2);
      const [entry] = await target.journal(runId);
      assert.equal(entry?.status, 'failed');
      assert.equal(entry?.attempts, 2);
    },
  },
  {
    name: 'suspend/resume: a run waits for its event and resumes with the payload',
    async run(target, fixtures) {
      const key = `order-${unique()}`;
      const { runId } = await target.start('conf-suspend', { key });
      await target.settle();
      assert.equal((await target.run(runId)).status, 'waiting');
      assert.equal(await target.sendEvent('conf.approved', `${key}-other`, { by: 'nobody' }), 0, 'another key woke the run');
      assert.equal(await target.sendEvent('conf.approved', key, { by: 'operator' }), 1);
      await target.settle();
      const run = await target.run(runId);
      assert.equal(run.status, 'completed', `run ended ${run.status}: ${run.error}`);
      assert.deepEqual(run.output, { approval: { received: true, payload: { by: 'operator' } }, after: 1 });
      assert.equal(fixtures.effects.get('conf-suspend/after'), 1);
    },
  },
  {
    name: 'suspend/resume: an event sent before the run waits is not lost',
    async run(target) {
      const key = `early-${unique()}`;
      const { runId } = await target.start('conf-suspend', { key });
      assert.equal(await target.sendEvent('conf.approved', key, { early: true }), 0);
      await target.settle();
      const run = await target.run(runId);
      assert.equal(run.status, 'completed', `run ended ${run.status}: ${run.error}`);
      assert.deepEqual((run.output as { approval: unknown }).approval, { received: true, payload: { early: true } });
    },
  },
  {
    name: 'suspend/resume: a wait times out with received false',
    async run(target) {
      const { runId } = await target.start('conf-suspend', { key: `late-${unique()}`, timeoutMs: WAIT_TIMEOUT_MS });
      await target.settle();
      assert.equal((await target.run(runId)).status, 'waiting');
      await target.advance(WAIT_TIMEOUT_MS + 500);
      const run = await target.run(runId);
      assert.equal(run.status, 'completed', `run ended ${run.status}: ${run.error}`);
      assert.deepEqual((run.output as { approval: unknown }).approval, { received: false, payload: null });
    },
  },
  {
    name: 'incompatibility: code that no longer matches the journal fails the run, keeping the journal',
    async run(target, fixtures) {
      const { runId } = await target.start('conf-incompatible', { case: unique() });
      await target.settle();
      assert.equal((await target.run(runId)).status, 'sleeping');
      fixtures.variant.current = 'b';
      await target.advance(SLEEP_MS + 500);
      const run = await target.run(runId);
      assert.equal(run.status, 'failed');
      assert.equal(run.errorKind, 'incompatible');
      assert.match(String(run.error), /WorkflowIncompatible/);
      assert.match(String(run.error), /one/);
      assert.match(String(run.error), /uno/);
      assert.equal(fixtures.effects.get('conf-incompatible/uno'), undefined, 'the new step ran');
      assert.equal((await target.journal(runId)).length, 2, 'the journal was not kept');
    },
  },
  {
    name: 'stale lease: a write from an expired lease is rejected and the step is recorded once',
    async run(target, fixtures) {
      const { runId } = await target.start('conf-gated', { case: unique() });
      await target.kick();
      await until('the first invocation to enter the step', () => fixtures.effects.get('conf-gated/entered') === 1);
      await target.expireLease(runId);
      await target.kick();
      await until('the second invocation to enter the step', () => fixtures.effects.get('conf-gated/entered') === 2);
      fixtures.gated.gate.open();
      await target.settle();
      const run = await target.run(runId);
      assert.equal(run.status, 'completed', `run ended ${run.status}: ${run.error}`);
      const journal = await target.journal(runId);
      assert.equal(journal.length, 1);
      // Whichever invocation wrote: only the current lease's write is kept.
      assert.deepEqual(run.output, journal[0]?.result);
      assert.deepEqual(run.output, { entered: 2 });
    },
  },
  {
    name: 'stale lease: the handler refuses a token Temps did not sign, before any code runs',
    async run(_target, fixtures, handler) {
      const { privateKey } = generateKeyPairSync('ed25519');
      const body = JSON.stringify({
        protocol_version: PROTOCOL_VERSION,
        run_id: '00000000-0000-0000-0000-000000000000',
        workflow: 'conf-idempotent',
        attempt: 1,
        lease: 1,
        now: new Date().toISOString(),
        deadline_at: new Date(Date.now() + 60_000).toISOString(),
        lease_expires_at: new Date(Date.now() + 60_000).toISOString(),
        input: null,
        journal: [],
      });
      const claims = Buffer.from(
        JSON.stringify({ v: 1, typ: 'invoke', pid: 1, eid: 1, run: '00000000-0000-0000-0000-000000000000', lease: 1, exp: Math.floor(Date.now() / 1000) + 60 }),
      ).toString('base64url');
      const signature = sign(null, Buffer.from(`${TOKEN_VERSION}.${claims}`), privateKey).toString('base64url');
      const before = fixtures.effects.get('conf-idempotent/once') ?? 0;
      const response = await handler(
        new Request(`http://executor${INVOKE_PATH}`, {
          method: 'POST',
          headers: { authorization: `Bearer ${TOKEN_VERSION}.${claims}.${signature}`, 'content-type': 'application/json' },
          body,
        }),
      );
      assert.equal(response.status, 401);
      assert.equal(fixtures.effects.get('conf-idempotent/once') ?? 0, before, 'workflow code ran for a forged request');
    },
  },
  {
    name: 'idempotent start: the same key returns the same run',
    async run(target, fixtures) {
      const key = `idem-${unique()}`;
      const first = await target.start('conf-idempotent', { n: 1 }, key);
      const second = await target.start('conf-idempotent', { n: 2 }, key);
      assert.equal(first.created, true);
      assert.equal(second.created, false);
      assert.equal(second.runId, first.runId);
      await target.settle();
      assert.equal((await target.run(first.runId)).status, 'completed');
      assert.equal(fixtures.effects.get('conf-idempotent/once'), 1);
    },
  },
];
