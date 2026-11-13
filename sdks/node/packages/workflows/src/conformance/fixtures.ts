// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The workflows the conformance suite runs. They count every execution of
 * a step body in `effects`, so the suite can tell a replayed step (not run
 * again) from a re-executed one.
 */

import { defineWorkflow, type WorkflowDefinition } from '../workflow.js';

export interface Gate {
  /** Resolves when the test opens the gate. */
  wait(): Promise<void>;
  open(): void;
}

export function gate(): Gate {
  let open!: () => void;
  const opened = new Promise<void>((resolve) => {
    open = resolve;
  });
  return { wait: () => opened, open };
}

export interface ConformanceFixtures {
  workflows: WorkflowDefinition[];
  /** Executions of each step body, by `<workflow>/<step>`. */
  effects: Map<string, number>;
  /** Which shape `conf-incompatible` has: `a` at start, `b` after a "deploy". */
  variant: { current: 'a' | 'b' };
  /** Held by `conf-gated`'s step until the test opens it. */
  gated: { gate: Gate };
  reset(): void;
}

/** Short enough for the real engine, which runs on wall-clock time. */
export const SLEEP_MS = 1500;
export const RETRY_BACKOFF_MS = 1000;
export const WAIT_TIMEOUT_MS = 2000;

export function createConformanceFixtures(): ConformanceFixtures {
  const effects = new Map<string, number>();
  const variant = { current: 'a' as 'a' | 'b' };
  const gated = { gate: gate() };
  const effect = (key: string) => {
    const count = (effects.get(key) ?? 0) + 1;
    effects.set(key, count);
    return count;
  };

  const replay = defineWorkflow('conf-replay', async (ctx) => {
    const a = await ctx.step('a', () => effect('conf-replay/a'));
    await ctx.sleep('pause', SLEEP_MS);
    const b = await ctx.step('b', () => effect('conf-replay/b'));
    const now = await ctx.now();
    const random = await ctx.random();
    return { a, b, now: now.toISOString(), random };
  });

  const retry = defineWorkflow('conf-retry', async (ctx) => {
    return ctx.step(
      'flaky',
      ({ attempt }) => {
        effect('conf-retry/flaky');
        if (attempt < 3) throw new Error(`attempt ${attempt} failed`);
        return { attempt };
      },
      { retries: 3, initialBackoff: RETRY_BACKOFF_MS },
    );
  });

  const exhausted = defineWorkflow('conf-retry-exhausted', async (ctx) => {
    await ctx.step(
      'always-fails',
      () => {
        effect('conf-retry-exhausted/always-fails');
        throw new Error('the upstream is down');
      },
      { retries: 1, initialBackoff: RETRY_BACKOFF_MS },
    );
    return 'unreachable';
  });

  const suspend = defineWorkflow('conf-suspend', async (ctx) => {
    const input = ctx.input as { key: string; timeoutMs?: number };
    const approval = await ctx.waitForEvent('approval', {
      event: 'conf.approved',
      key: input.key,
      timeout: input.timeoutMs ?? 3_600_000,
    });
    const after = await ctx.step('after', () => effect('conf-suspend/after'));
    return { approval, after };
  });

  const incompatible = defineWorkflow('conf-incompatible', async (ctx) => {
    if (variant.current === 'a') {
      await ctx.step('one', () => effect('conf-incompatible/one'));
    } else {
      await ctx.step('uno', () => effect('conf-incompatible/uno'));
    }
    await ctx.sleep('gap', SLEEP_MS);
    return 'done';
  });

  const gatedWorkflow = defineWorkflow('conf-gated', async (ctx) => {
    return ctx.step('gated', async () => {
      const entered = effect('conf-gated/entered');
      await gated.gate.wait();
      return { entered };
    });
  });

  const idempotent = defineWorkflow('conf-idempotent', async (ctx) => {
    return ctx.step('once', () => effect('conf-idempotent/once'));
  });

  const parent = defineWorkflow('conf-parent', async (ctx) => {
    const input = ctx.input as { n: number; fail?: boolean };
    const before = await ctx.step('before', () => effect('conf-parent/before'));
    const child = await ctx.child('square', 'conf-child', input);
    return { before, child };
  });

  const child = defineWorkflow('conf-child', async (ctx) => {
    const input = ctx.input as { n: number; fail?: boolean };
    return ctx.step('square', () => {
      effect('conf-child/square');
      if (input.fail) throw new Error('the child cannot square that');
      return input.n * input.n;
    }, { retries: 0 });
  });

  return {
    workflows: [replay, retry, exhausted, suspend, incompatible, gatedWorkflow, idempotent, parent, child],
    effects,
    variant,
    gated,
    reset() {
      effects.clear();
      variant.current = 'a';
      gated.gate = gate();
    },
  };
}
