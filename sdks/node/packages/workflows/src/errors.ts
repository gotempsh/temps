// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** A call to the Temps API failed. `title` is the RFC 7807 problem title. */
export class TempsApiError extends Error {
  constructor(
    public readonly operation: string,
    public readonly status: number,
    public readonly title: string | undefined,
    public readonly detail: string | undefined,
  ) {
    super(
      `Temps API ${operation} failed with ${status}${title ? ` ${title}` : ''}${detail ? `: ${detail}` : ''}`,
    );
    this.name = 'TempsApiError';
  }
}

/**
 * Temps could not be reached, or answered with a server error, while the
 * invocation read its journal or recorded an entry. Nothing about the run is
 * wrong: the handler answers 503 so Temps retries the invocation with
 * backoff, instead of reporting the run failed.
 */
export class TempsUnavailableError extends Error {
  constructor(
    public readonly runId: string,
    public readonly cause: TempsApiError,
  ) {
    super(`Temps was unavailable during run ${runId}: ${cause.message}`);
    this.name = 'TempsUnavailableError';
  }
}

/**
 * The run was leased to another invocation (a lease expired, an operator
 * cancelled or retried it). This invocation must stop without reporting.
 */
export class StaleLeaseError extends Error {
  constructor(
    public readonly runId: string,
    detail?: string,
  ) {
    super(`Run ${runId} is held by another invocation${detail ? `: ${detail}` : ''}`);
    this.name = 'StaleLeaseError';
  }
}

/**
 * Replay found a different step than the journal recorded: the workflow's
 * code changed in an incompatible way since the run started.
 */
export class WorkflowIncompatibleError extends Error {
  constructor(
    public readonly runId: string,
    public readonly sequence: number,
    public readonly expected: string,
    public readonly found: string,
  ) {
    super(
      `Workflow run ${runId} is incompatible with the deployed code at step ${sequence}: the journal recorded '${expected}', the code reached '${found}'`,
    );
    this.name = 'WorkflowIncompatibleError';
  }
}

/** A step exhausted its attempts. Catch it to continue the workflow. */
export class StepFailedError extends Error {
  constructor(
    public readonly step: string,
    public readonly sequence: number,
    public readonly attempts: number,
    public readonly reason: string,
  ) {
    super(`Step '${step}' failed after ${attempts} attempt(s): ${reason}`);
    this.name = 'StepFailedError';
  }
}

/** An operator cancelled the run; the workflow stops at the next step. */
export class RunCancelledError extends Error {
  constructor(public readonly runId: string) {
    super(`Workflow run ${runId} was cancelled`);
    this.name = 'RunCancelledError';
  }
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message || error.name;
  if (typeof error === 'string') return error;
  try {
    return JSON.stringify(error) ?? String(error);
  } catch {
    return String(error);
  }
}
