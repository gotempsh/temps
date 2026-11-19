// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  CreationStep,
  HostImagePreparation,
  ImagePrepareStatus,
  SandboxCreation,
} from '@/api/client'
import type { QueryClient } from '@tanstack/react-query'
import { getSandboxImagePreparationQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { httpStatusOf } from '@/lib/http-error-status'

/**
 * Mark the sandbox image status stale so the card showing it reads it again
 * now, as part of a page's Refresh, instead of at its next poll.
 */
export function refreshSandboxImageStatus(
  queryClient: QueryClient
): Promise<void> {
  return queryClient.invalidateQueries({
    queryKey: getSandboxImagePreparationQueryKey(),
  })
}

/** How often a running create is polled for its steps. */
export const CREATION_POLL_MS = 1_000

/**
 * A fresh id for one create, sent as `creation_id` so its steps can be read
 * while `POST /v1/sandboxes` is still waiting for the sandbox. 8–64 of
 * `[A-Za-z0-9_-]`; a UUID fits.
 */
export function newCreationId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID()
  }
  return `crt-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`
}

/** Milliseconds a step took, or has taken so far while it runs. */
export function stepDurationMs(step: CreationStep, now: number): number | null {
  if (!step.started_at) return null
  const start = Date.parse(step.started_at)
  if (Number.isNaN(start)) return null
  const end = step.finished_at ? Date.parse(step.finished_at) : now
  if (Number.isNaN(end)) return null
  return Math.max(0, end - start)
}

/** `0.4s`, `12s`, `3m 05s`. */
export function formatDuration(ms: number): string {
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`
  const seconds = Math.round(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  return `${minutes}m ${String(seconds % 60).padStart(2, '0')}s`
}

/** `512 MB`, `3.4 GB`. */
export function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000
    unit += 1
  }
  return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`
}

/** Download progress in percent, when the registry reported sizes. */
export function downloadPercent(
  done: number | null | undefined,
  total: number | null | undefined
): number | null {
  if (done == null || !total) return null
  return Math.min(100, Math.round((done / total) * 100))
}

/** `1.2 GB of 3.4 GB`, or `1.2 GB` before the total is known. */
export function downloadLabel(
  done: number | null | undefined,
  total: number | null | undefined
): string | null {
  if (done == null) return null
  return total
    ? `${formatBytes(done)} of ${formatBytes(total)}`
    : formatBytes(done)
}

/** The step a failed create stopped at. */
export function failedStep(creation: SandboxCreation): CreationStep | null {
  return creation.steps.find((step) => step.status === 'failed') ?? null
}

/** Total time of the create so far. */
export function creationDurationMs(
  creation: SandboxCreation,
  now: number
): number {
  const start = Date.parse(creation.started_at)
  const end = creation.finished_at ? Date.parse(creation.finished_at) : now
  return Math.max(0, end - start)
}

/** Whether polling should go on. */
export function creationRunning(
  creation: SandboxCreation | undefined
): boolean {
  return creation === undefined || creation.status === 'running'
}

/**
 * Whether a failed progress read means the server does not track the create
 * (a 404: it was refused before it began, or the server restarted and lost
 * it), as opposed to a failure worth retrying (network, 5xx).
 */
export function creationUntracked(error: unknown): boolean {
  return httpStatusOf(error) === 404
}

/**
 * Whether to poll a create's steps again. `error` is the last read's
 * failure, or null; `failedAfterRequestEnded` says whether that failure
 * came after the create request (`POST /v1/sandboxes`) had ended.
 *
 * - No failure: while the create runs, or until the first answer.
 * - A 404 stops polling, even over steps read earlier: the server no longer
 *   tracks the create, so it will never finish there. The one exception is
 *   a 404 before anything was read that came while the create request was
 *   still in flight: that read can race the server registering the id, so
 *   one more read after the request ends decides.
 * - Any other failure is transient: keep polling while the create is
 *   running or not read yet, so its steps (or its failure) come back once
 *   the server answers.
 */
export function keepPolling(
  creation: SandboxCreation | undefined,
  error: unknown,
  failedAfterRequestEnded: boolean
): boolean {
  if (!error) return creationRunning(creation)
  if (creationUntracked(error)) {
    return creation === undefined && !failedAfterRequestEnded
  }
  return creationRunning(creation)
}

/**
 * Whether a read that failed at `errorAt` (ms since the epoch) failed after
 * the create request ended at `requestEndedAt` (null while it runs).
 */
export function failedAfterRequestEnded(
  errorAt: number,
  requestEndedAt: number | null
): boolean {
  return requestEndedAt !== null && errorAt >= requestEndedAt
}

/**
 * Whether a starting sandbox's step poll runs again. Only a 404 (this
 * server no longer tracks the create: restarted, or it finished long ago)
 * stops it; a network or server error keeps polling, so the steps come back
 * once the server answers.
 */
export function keepPollingStartingSandbox(
  starting: boolean,
  creation: SandboxCreation | undefined,
  error: unknown
): boolean {
  if (!starting) return false
  if (error) return !creationUntracked(error)
  return creationRunning(creation)
}

/**
 * Whether a starting sandbox shows its steps card. It is left out only when
 * the server confirmed it does not track the create and nothing was read;
 * any other failure shows the card with the error and a retry.
 */
export function showStartingSandboxSteps(
  starting: boolean,
  creation: SandboxCreation | undefined,
  error: unknown
): boolean {
  if (!starting) return false
  if (creation !== undefined) return true
  return !error || !creationUntracked(error)
}

/** One line for a host's image state. */
export function imageStateLabel(
  status: ImagePrepareStatus | null | undefined
): string {
  if (!status) return 'Unknown'
  switch (status.state) {
    case 'not_pulled':
      return 'Not pulled'
    case 'pulling':
      return status.detail === 'building locally' ? 'Building' : 'Pulling'
    case 'ready':
      return 'Ready'
    case 'failed':
      return 'Failed'
    case 'unsupported':
      return 'Not applicable'
  }
}

/** Hosts a "Prepare sandboxes" would pull on: eligible, not ready, not pulling. */
export function hostsToPrepare(
  hosts: HostImagePreparation[]
): HostImagePreparation[] {
  return hosts.filter(
    (host) =>
      host.eligible &&
      host.status?.state !== 'ready' &&
      host.status?.state !== 'pulling' &&
      host.status?.state !== 'unsupported'
  )
}

/** Whether any host is pulling, so the panel keeps polling. */
export function anyHostPulling(hosts: HostImagePreparation[]): boolean {
  return hosts.some((host) => host.status?.state === 'pulling')
}
