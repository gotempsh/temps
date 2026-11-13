// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Wire types of the executor protocol, version 1.
 *
 * Normative description: `docs/reference/workflows-executor-protocol.md` in
 * the Temps repository; the Rust side is `crates/temps-workflows/src/protocol.rs`.
 * Fields are additive within a version: unknown fields are ignored.
 */

export const PROTOCOL_VERSION = 1;
export const PROTOCOL_HEADER = 'temps-workflows-protocol';
export const INVOKE_PATH = '/.well-known/temps/workflows/invoke';
export const MANIFEST_PATH = '/.well-known/temps/workflows/manifest';
export const TOKEN_VERSION = 'twi1';

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type StepKind = 'step' | 'sleep' | 'wait' | 'now' | 'random' | 'send_event';
export type StepStatus = 'pending' | 'completed' | 'failed';
export type RunStatus =
  | 'queued'
  | 'running'
  | 'sleeping'
  | 'waiting'
  | 'retrying'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'stuck';

export interface JournalEntry {
  sequence: number;
  name: string;
  kind: StepKind;
  status: StepStatus;
  attempts: number;
  result?: JsonValue | null;
  error?: string | null;
  wake_at?: string | null;
}

export interface InvokeRequest {
  protocol_version: number;
  run_id: string;
  workflow: string;
  attempt: number;
  lease: number;
  now: string;
  deadline_at: string;
  lease_expires_at: string;
  input?: JsonValue | null;
  journal: JournalEntry[];
  journal_next_sequence?: number | null;
}

export interface ManifestWorkflow {
  name: string;
  version?: string | null;
  concurrency?: number | null;
}

export interface Manifest {
  protocol_version: number;
  sdk?: string | null;
  workflows: ManifestWorkflow[];
}

export interface EmittedEvent {
  name: string;
  key?: string | null;
  payload?: JsonValue | null;
}

export interface StepWriteRequest {
  sequence: number;
  name: string;
  kind: StepKind;
  status: StepStatus;
  attempts: number;
  result?: JsonValue | null;
  error?: string | null;
  wake_at?: string | null;
  duration_ms?: number | null;
  trace_id?: string | null;
  error_ref?: string | null;
  event?: EmittedEvent | null;
}

export interface StepWriteResponse {
  entry: JournalEntry;
  already_recorded: boolean;
  lease_expires_at: string;
}

export interface JournalPage {
  entries: JournalEntry[];
  next_sequence?: number | null;
}

export interface HeartbeatResponse {
  lease_expires_at: string;
  cancelled: boolean;
}

export type Outcome =
  | { type: 'completed'; output?: JsonValue | null }
  | { type: 'failed'; error: string; sequence?: number | null }
  | { type: 'sleep'; sequence: number; until: string }
  | {
      type: 'wait';
      sequence: number;
      event: string;
      key?: string | null;
      timeout_at?: string | null;
    }
  | { type: 'retry'; sequence: number; at: string }
  | { type: 'incompatible'; sequence: number; expected: string; found: string };

export interface OutcomeRequest {
  outcome: Outcome;
}

export interface InvocationKey {
  algorithm: string;
  public_key: string;
  key_id: string;
  /** The project the credential belongs to: tokens must carry it as `pid`. */
  project_id: number;
  /** The environment the credential belongs to: tokens must carry it as `eid`. */
  environment_id: number;
}

/** Claims of an invocation token. */
export interface InvocationClaims {
  v: number;
  typ: 'invoke' | 'manifest';
  pid: number;
  eid: number;
  run?: string;
  lease?: number;
  exp: number;
  body?: string;
}

/** What Temps records for a resolved `waitForEvent`. */
export interface WaitResult<T = JsonValue> {
  received: boolean;
  payload: T | null;
}
