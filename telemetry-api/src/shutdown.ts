// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Graceful shutdown on a platform stop (SIGTERM) or Ctrl-C (SIGINT).
//
// Order matters: stop accepting connections and WAIT for in-flight requests
// first — an ingest request enqueues its country backfill only after its
// inserts finish — then flush the backfill queue, then exit. Exiting earlier
// would cut requests off mid-write and lose what they queued.
//
// The whole sequence is bounded by `budgetMs`, kept under the platform's stop
// grace period (10s) so we exit on our own terms instead of being SIGKILLed.

import { errorFields, log } from "./log.js";

export const SHUTDOWN_BUDGET_MS = 8_000;

export interface ShutdownDeps {
  // Stop accepting connections; resolves once in-flight requests finished.
  stopServer: () => Promise<void>;
  // Stop the backfill timer and flush whatever is queued. Never throws.
  stopBackfill: () => Promise<void>;
  pendingBackfill: () => number;
  exit: (code: number) => void;
  budgetMs?: number;
}

export async function gracefulShutdown(deps: ShutdownDeps, signal: string): Promise<void> {
  const budgetMs = deps.budgetMs ?? SHUTDOWN_BUDGET_MS;
  log("info", "server", "shutting down: draining in-flight requests", { signal, budget_ms: budgetMs });

  let timedOut = false;
  const force = setTimeout(() => {
    timedOut = true;
    log("warn", "server", "shutdown budget exceeded; exiting before drain/flush finished", {
      budget_ms: budgetMs,
      pending: deps.pendingBackfill(),
    });
    deps.exit(1);
  }, budgetMs);

  let code = 0;
  try {
    await deps.stopServer();
    log("info", "server", "requests drained; flushing country backfill", {
      pending: deps.pendingBackfill(),
    });
    await deps.stopBackfill();
  } catch (err) {
    code = 1;
    log("error", "server", "shutdown step failed", errorFields(err));
  } finally {
    clearTimeout(force);
  }
  if (!timedOut) deps.exit(code);
}
