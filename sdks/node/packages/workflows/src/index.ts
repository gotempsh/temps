// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Durable workflows on Temps.
 *
 * Define workflows with {@link defineWorkflow}, serve them from the app with
 * {@link createWorkflowHandler} (mounted with an adapter), and start runs and
 * send events with {@link WorkflowsClient}. Temps orchestrates: it stores
 * every step's result, sleeps and waits without holding a connection, and
 * invokes the app again when a run is due, so a run survives restarts,
 * crashes and deploys. In-memory testing: `@temps-sdk/workflows/testing`.
 */

export { defineWorkflow, durationMs } from './workflow.js';
export type {
  Duration,
  RetryPolicy,
  SendEventOptions,
  StepAttempt,
  StepOptions,
  WaitOptions,
  WorkflowContext,
  WorkflowDefinition,
  WorkflowOptions,
} from './workflow.js';
export { createWorkflowHandler, SDK_NAME } from './handler.js';
export type { FetchHandler, WorkflowHandlerOptions } from './handler.js';
export { toNodeListener, nextRouteHandlers, honoHandler, WORKFLOWS_PREFIX } from './adapters.js';
export { WorkflowsClient } from './client.js';
export type { StartOptions, StartedRun, WorkflowRun, WorkflowsClientOptions } from './client.js';
export {
  RunCancelledError,
  StaleLeaseError,
  StepFailedError,
  TempsApiError,
  TempsUnavailableError,
  WorkflowIncompatibleError,
} from './errors.js';
export type { ErrorTracker, TelemetryOptions, TracerLike, SpanLike } from './telemetry.js';
export { PROTOCOL_VERSION } from './protocol.js';
export type { JsonValue, RunStatus, WaitResult, Outcome, JournalEntry } from './protocol.js';
