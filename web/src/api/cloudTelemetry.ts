// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written client for the Cloud telemetry write-mode endpoints (ADR-041).
 *
 * These routes (`/otel/cloud-telemetry/*`) are served by the OTel plugin. The
 * interfaces below mirror the server DTOs in
 * `crates/temps-otel/src/handlers/cloud_telemetry_handler.rs` and call the
 * shared generated `client` object directly — the same call shape every
 * generated SDK function uses, just without codegen. Keep them in sync with
 * that file when the DTOs change.
 */

import { client } from '@/api/client/client.gen'

export type CloudTelemetryWriteMode = 'local' | 'cloud'
export type CloudTelemetryFidelity = 'metered' | 'queryable'

/**
 * Why a project's spans stopped going where its write mode says.
 *
 * `operator` and `cloud_recovered` are deliberate; the rest happened *to* the
 * instance and each names a different fix.
 */
export type TelemetryWriteIntervalReason =
  | 'operator'
  | 'cloud_disconnected'
  | 'quota_exhausted'
  | 'credential_rejected'
  | 'queue_overflow_spill'
  | 'cloud_recovered'

export interface TelemetryGapWindow {
  project_id: number
  started_at: string
  ended_at: string
  dropped_spans: number
  dropped_bytes: number
  reason: TelemetryWriteIntervalReason
  /** Prose written by the server, so the client never renders a bare enum. */
  message: string
}

export interface TelemetryWriteInterval {
  mode: CloudTelemetryWriteMode
  effective_from: string
  effective_to?: string
  reason: TelemetryWriteIntervalReason
  message: string
}

export interface ProjectCloudTelemetry {
  project_id: number
  fidelity: CloudTelemetryFidelity
  attribute_allowlist: string[]
  /** The operator's declared intent. */
  write_mode: CloudTelemetryWriteMode
  /** Where spans are going right now, which differs during a fallback. */
  effective_write_mode: CloudTelemetryWriteMode
  effective_reason?: TelemetryWriteIntervalReason
  effective_reason_message?: string
  /**
   * Whether `cloud` could be selected right now. `false` is the normal state on
   * an unlinked instance and must render as onboarding, never as an error.
   */
  cloud_write_mode_available: boolean
  reason?: string
  setup_path?: string
  queued_spans: number
  /** Spans this instance accepted for the project and gave up on delivering. */
  dead_lettered_spans: number
  /**
   * Why the most recent give-up happened. Delivery metadata only — the
   * instance's own bounded error string, never span content.
   */
  last_dead_letter_error?: string
  last_dead_letter_at?: string
  gap_windows: TelemetryGapWindow[]
  intervals: TelemetryWriteInterval[]
}

export interface CloudTelemetryWriteStatus {
  configured: boolean
  reason?: string
  setup_path?: string
  cloud_primary_projects: number
  local_mode_projects: number
  local_span_store_required: boolean
  local_span_store_reason?: string
  local_history_until?: string
  queue_depth: number
  queue_bytes: number
  queue_max_bytes: number
  oldest_unshipped_age_secs?: number
  dead_lettered_rows: number
  write_suspension?: string
  gap_windows: TelemetryGapWindow[]
  can_decommission_local_span_store: boolean
}

export interface UpdateProjectCloudTelemetryRequest {
  fidelity?: CloudTelemetryFidelity
  attribute_allowlist?: string[]
  write_mode?: CloudTelemetryWriteMode
}

const SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

/** Query key for a project's Cloud telemetry settings. */
export const projectCloudTelemetryKey = (projectId: number) =>
  ['otel', 'cloud-telemetry', 'project', projectId] as const

/** Query key for the instance-wide write status. */
export const cloudTelemetryStatusKey = () =>
  ['otel', 'cloud-telemetry', 'status'] as const

export async function fetchProjectCloudTelemetry(
  projectId: number,
): Promise<ProjectCloudTelemetry> {
  const { data, error } = await client.get<ProjectCloudTelemetry>({
    security: [...SECURITY],
    url: `/otel/cloud-telemetry/projects/${projectId}`,
  })
  if (error || !data) throw error ?? new Error('No response body')
  return data
}

export async function fetchCloudTelemetryStatus(): Promise<CloudTelemetryWriteStatus> {
  const { data, error } = await client.get<CloudTelemetryWriteStatus>({
    security: [...SECURITY],
    url: '/otel/cloud-telemetry/status',
  })
  if (error || !data) throw error ?? new Error('No response body')
  return data
}

export async function updateProjectCloudTelemetry(
  projectId: number,
  body: UpdateProjectCloudTelemetryRequest,
): Promise<ProjectCloudTelemetry> {
  const { data, error } = await client.patch<ProjectCloudTelemetry>({
    security: [...SECURITY],
    url: `/otel/cloud-telemetry/projects/${projectId}`,
    body,
  })
  if (error || !data) throw error ?? new Error('No response body')
  return data
}

/**
 * Pull the human-readable sentence out of an RFC 7807 Problem body.
 *
 * The gate's refusals carry the *specific* missing prerequisite in `detail`,
 * and that sentence is the entire value of the error — collapsing it to
 * "Request failed" would leave a self-hosted operator with nothing to act on.
 */
export function problemDetail(error: unknown, fallback: string): string {
  if (error && typeof error === 'object') {
    const problem = error as { detail?: unknown; title?: unknown }
    if (typeof problem.detail === 'string' && problem.detail.length > 0) {
      return problem.detail
    }
    if (typeof problem.title === 'string' && problem.title.length > 0) {
      return problem.title
    }
  }
  return fallback
}

/** The console path an RFC 7807 gate refusal points at, when it carries one. */
export function problemSetupPath(error: unknown): string | undefined {
  if (error && typeof error === 'object') {
    const problem = error as { setup_path?: unknown }
    if (typeof problem.setup_path === 'string') return problem.setup_path
  }
  return undefined
}
