// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The executor's calls back to Temps during an invocation: journal pages,
 * step writes, heartbeats and the callback outcome, all with the invocation
 * token. Transient failures (network, 5xx) are retried briefly; a stale lease
 * and an incompatible step are typed errors the replay loop acts on.
 */

import { StaleLeaseError, TempsApiError, WorkflowIncompatibleError } from './errors.js';
import {
  PROTOCOL_HEADER,
  PROTOCOL_VERSION,
  type HeartbeatResponse,
  type InvocationKey,
  type JournalPage,
  type OutcomeRequest,
  type StepWriteRequest,
  type StepWriteResponse,
} from './protocol.js';

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface TransportOptions {
  /** Temps' API root, `TEMPS_API_URL` (ends in `/api`). */
  apiUrl: string;
  fetch?: FetchLike;
  /** Attempts for a transient failure; default 4. */
  attempts?: number;
  /** First backoff in ms, doubled per attempt; default 250. */
  backoffMs?: number;
  /** Per-attempt timeout in ms; default 30 s. */
  timeoutMs?: number;
}

interface Problem {
  title?: string;
  detail?: string;
}

const TRANSIENT = new Set([502, 503, 504]);

export class ExecutorTransport {
  private readonly apiUrl: string;
  private readonly fetchImpl: FetchLike;
  private readonly attempts: number;
  private readonly backoffMs: number;
  private readonly timeoutMs: number;

  constructor(options: TransportOptions) {
    this.apiUrl = options.apiUrl.replace(/\/+$/, '');
    this.fetchImpl = options.fetch ?? ((input, init) => fetch(input, init));
    this.attempts = Math.max(1, options.attempts ?? 4);
    this.backoffMs = options.backoffMs ?? 250;
    this.timeoutMs = options.timeoutMs ?? 30_000;
  }

  /** The invocation key and the environment it binds tokens to, read with the workflow credential. */
  async invocationKey(credential: string): Promise<InvocationKey> {
    return this.call<InvocationKey>('invocation key', 'GET', '/workflows/v1/invocation-key', { token: credential });
  }

  async journal(token: string, runId: string, from: number): Promise<JournalPage> {
    return this.call<JournalPage>(
      'journal read',
      'GET',
      `/workflow-runs/${encodeURIComponent(runId)}/journal?from=${from}`,
      { token, runId },
    );
  }

  async recordStep(token: string, runId: string, write: StepWriteRequest): Promise<StepWriteResponse> {
    return this.call<StepWriteResponse>(
      `step ${write.sequence} write`,
      'POST',
      `/workflow-runs/${encodeURIComponent(runId)}/steps`,
      { token, runId, body: write },
    );
  }

  async heartbeat(token: string, runId: string): Promise<HeartbeatResponse> {
    return this.call<HeartbeatResponse>(
      'heartbeat',
      'POST',
      `/workflow-runs/${encodeURIComponent(runId)}/heartbeat`,
      { token, runId, body: {} },
    );
  }

  async reportOutcome(token: string, runId: string, outcome: OutcomeRequest): Promise<void> {
    await this.call<unknown>('outcome report', 'POST', `/workflow-runs/${encodeURIComponent(runId)}/outcome`, {
      token,
      runId,
      body: outcome,
    });
  }

  private async call<T>(
    operation: string,
    method: 'GET' | 'POST',
    path: string,
    options: { token?: string; runId?: string; body?: unknown } = {},
  ): Promise<T> {
    const headers: Record<string, string> = { [PROTOCOL_HEADER]: String(PROTOCOL_VERSION) };
    if (options.token) headers.authorization = `Bearer ${options.token}`;
    if (options.body !== undefined) headers['content-type'] = 'application/json';
    let lastError: unknown;
    for (let attempt = 1; attempt <= this.attempts; attempt += 1) {
      let response: Response;
      let body: T | undefined;
      try {
        // Bounded: a Temps that accepts the connection and never answers
        // must not hold the invocation (and its lease) forever. The body is
        // read inside the same attempt: a connection that drops, or a
        // timeout that fires, after the headers is a transport failure to
        // retry, not a workflow failure. Every call is idempotent (a step
        // already recorded answers `already_recorded`).
        response = await this.fetchImpl(`${this.apiUrl}${path}`, {
          method,
          headers,
          body: options.body === undefined ? undefined : JSON.stringify(options.body),
          signal: AbortSignal.timeout(this.timeoutMs),
        });
        if (response.ok) body = (await response.json()) as T;
      } catch (error) {
        lastError = error;
        await this.pause(attempt);
        continue;
      }
      if (response.ok) return body as T;
      const problem = await readProblem(response);
      if (response.status === 409 && problem.title === 'Stale Lease') {
        throw new StaleLeaseError(options.runId ?? '', problem.detail);
      }
      if (response.status === 409 && problem.title === 'Workflow Incompatible') {
        throw parseIncompatible(options.runId ?? '', problem.detail);
      }
      lastError = new TempsApiError(operation, response.status, problem.title, problem.detail);
      if (!TRANSIENT.has(response.status)) throw lastError;
      await this.pause(attempt);
    }
    if (lastError instanceof TempsApiError) throw lastError;
    throw new TempsApiError(operation, 0, 'Unreachable', String(lastError));
  }

  private async pause(attempt: number): Promise<void> {
    if (attempt >= this.attempts) return;
    await new Promise((resolve) => setTimeout(resolve, this.backoffMs * 2 ** (attempt - 1)));
  }
}

async function readProblem(response: Response): Promise<Problem> {
  try {
    const body = (await response.json()) as Problem;
    return { title: body.title, detail: body.detail };
  } catch {
    return {};
  }
}

function parseIncompatible(runId: string, detail: string | undefined): WorkflowIncompatibleError {
  // `WorkflowIncompatible { run_id: …, step: 3, expected: "a", found: "b" }`
  const match = /step: (\d+), expected: "((?:[^"\\]|\\.)*)", found: "((?:[^"\\]|\\.)*)"/.exec(detail ?? '');
  return new WorkflowIncompatibleError(
    runId,
    match ? Number(match[1]) : -1,
    match?.[2] ?? 'unknown',
    match?.[3] ?? 'unknown',
  );
}
