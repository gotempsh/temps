// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The app's client for its environment's workflows: start a run, read it,
 * send an event. Authenticated with the environment's workflow credential
 * (`TEMPS_WORKFLOWS_TOKEN`, issued in the console under the environment's
 * Workflows tab).
 */

import { TempsApiError } from './errors.js';
import type { JsonValue, RunStatus } from './protocol.js';
import type { FetchLike } from './transport.js';

export interface WorkflowsClientOptions {
  /** Default `process.env.TEMPS_API_URL`. */
  apiUrl?: string;
  /** Default `process.env.TEMPS_WORKFLOWS_TOKEN`. */
  token?: string;
  fetch?: FetchLike;
  /** Per-request timeout; default 30 s. */
  timeoutMs?: number;
}

export interface WorkflowRun<Output = JsonValue> {
  id: string;
  workflow: string;
  status: RunStatus;
  error_kind?: string | null;
  output?: Output | null;
  error?: string | null;
  created_at: string;
  finished_at?: string | null;
}

export interface StartOptions {
  /** Starting twice with the same key returns the first run. */
  idempotencyKey?: string;
}

export interface StartedRun<Output = JsonValue> {
  run: WorkflowRun<Output>;
  /** `false` when the idempotency key matched an existing run. */
  created: boolean;
}

export interface SendEventOptions {
  key?: string;
  payload?: JsonValue;
}

const TERMINAL = new Set<RunStatus>(['completed', 'failed', 'cancelled']);

export class WorkflowsClient {
  private readonly apiUrl: string;
  private readonly token: string;
  private readonly fetchImpl: FetchLike;
  private readonly timeoutMs: number;

  constructor(options: WorkflowsClientOptions = {}) {
    const apiUrl = options.apiUrl ?? process.env.TEMPS_API_URL;
    const token = options.token ?? process.env.TEMPS_WORKFLOWS_TOKEN;
    if (!apiUrl) throw new Error('WorkflowsClient: TEMPS_API_URL is not set; pass apiUrl');
    if (!token) {
      throw new Error(
        'WorkflowsClient: TEMPS_WORKFLOWS_TOKEN is not set. Issue a workflow credential in the console (environment → Workflows) and set it in the app',
      );
    }
    this.apiUrl = apiUrl.replace(/\/+$/, '');
    this.token = token;
    this.fetchImpl = options.fetch ?? ((input, init) => fetch(input, init));
    this.timeoutMs = options.timeoutMs ?? 30_000;
  }

  /** Start a run of `workflow` with `input`. */
  async start<Output = JsonValue>(
    workflow: string,
    input?: JsonValue,
    options: StartOptions = {},
  ): Promise<StartedRun<Output>> {
    return this.call<StartedRun<Output>>('start run', 'POST', '/workflows/v1/runs', {
      workflow,
      input: input ?? null,
      idempotency_key: options.idempotencyKey ?? null,
    });
  }

  async getRun<Output = JsonValue>(runId: string): Promise<WorkflowRun<Output>> {
    return this.call<WorkflowRun<Output>>('read run', 'GET', `/workflows/v1/runs/${encodeURIComponent(runId)}`);
  }

  /** Wake the runs waiting for `name` (and `key`); returns how many woke. */
  async sendEvent(name: string, options: SendEventOptions = {}): Promise<{ event_id: number; delivered: number }> {
    return this.call('send event', 'POST', '/workflows/v1/events', {
      name,
      key: options.key ?? null,
      payload: options.payload ?? null,
    });
  }

  /** Poll until the run finishes or `timeoutMs` passes; returns the last state read. */
  async waitForRun<Output = JsonValue>(
    runId: string,
    options: { timeoutMs?: number; intervalMs?: number } = {},
  ): Promise<WorkflowRun<Output>> {
    const deadline = Date.now() + (options.timeoutMs ?? 60_000);
    for (;;) {
      const run = await this.getRun<Output>(runId);
      if (TERMINAL.has(run.status) || Date.now() >= deadline) return run;
      await new Promise((resolve) => setTimeout(resolve, options.intervalMs ?? 1000));
    }
  }

  private async call<T>(operation: string, method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    const response = await this.fetchImpl(`${this.apiUrl}${path}`, {
      method,
      headers: {
        authorization: `Bearer ${this.token}`,
        ...(body === undefined ? {} : { 'content-type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    if (response.ok) return (await response.json()) as T;
    let title: string | undefined;
    let detail: string | undefined;
    try {
      const problem = (await response.json()) as { title?: string; detail?: string };
      title = problem.title;
      detail = problem.detail;
    } catch {
      // Not a problem document.
    }
    throw new TempsApiError(operation, response.status, title, detail);
  }
}
