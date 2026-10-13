// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  InitializationFailure,
  InitializationFailureKind,
  ReadinessNextAction,
  ServiceReadiness,
} from '@/api/client/types.gen'

/**
 * What the console shows about a service's startup readiness gate.
 *
 * The backend keeps a service `starting` until it serves a real,
 * authenticated request (for RustFS: ListBuckets with the service's own
 * credentials), and marks it `failed` with a typed reason if that never
 * happens. Everything here is derived from `status` + `readiness` on the
 * service record; nothing is inferred from health checks.
 */
export type ReadinessView =
  | { kind: 'none' }
  | {
      kind: 'starting'
      /** Latest reason the service is not usable yet; null before the first probe. */
      reason: string | null
      restartCount: number
      /** Seconds waited so far, when the start time is known. */
      elapsedSecs: number | null
      deadlineSecs: number | null
    }
  | {
      kind: 'failed'
      failure: InitializationFailure
      restartCount: number
    }

interface ServiceLike {
  status: string
  readiness?: ServiceReadiness | null
}

export function readinessView(
  service: ServiceLike,
  now: number = Date.now()
): ReadinessView {
  const readiness = service.readiness ?? null
  if (service.status === 'starting') {
    const startedAt = readiness ? Date.parse(readiness.started_at) : NaN
    return {
      kind: 'starting',
      reason: readiness?.reason ?? null,
      restartCount: readiness?.restart_count ?? 0,
      elapsedSecs: Number.isNaN(startedAt)
        ? null
        : Math.max(0, Math.round((now - startedAt) / 1000)),
      deadlineSecs: readiness?.deadline_secs ?? null,
    }
  }
  if (service.status === 'failed' && readiness?.failure) {
    return {
      kind: 'failed',
      failure: readiness.failure,
      restartCount: readiness.restart_count,
    }
  }
  return { kind: 'none' }
}

export const FAILURE_KIND_TITLE: Record<InitializationFailureKind, string> = {
  store_init_failed: 'Storage initialization failed',
  restart_loop: 'The container keeps restarting',
  timeout: 'The service did not become ready in time',
}

export const NEXT_ACTION_COPY: Record<
  ReadinessNextAction,
  { title: string; description: string; button: string }
> = {
  view_logs: {
    title: 'Read the logs',
    description: "The container's logs show why the service could not start.",
    button: 'View logs',
  },
  retry: {
    title: 'Start again',
    description:
      'Restart the service and wait for it to serve requests again. Its volumes are kept.',
    button: 'Start again',
  },
  try_another_image: {
    title: 'Try another image version',
    description:
      'Upgrade the service to a different image tag. Its volumes are kept.',
    button: 'Change image…',
  },
  recreate_with_fresh_volumes: {
    title: 'Recreate with fresh volumes',
    description:
      'Delete this service, then create it again. Deleting removes its data and log volumes, so only do this when they hold nothing you need.',
    button: 'Delete service…',
  },
}

/** "Waiting 42s of 180s" style progress line for a starting service. */
export function startingProgress(
  view: Extract<ReadinessView, { kind: 'starting' }>
): string {
  if (view.elapsedSecs === null)
    return 'Waiting for the first successful request.'
  const deadline =
    view.deadlineSecs !== null ? ` (gives up after ${view.deadlineSecs}s)` : ''
  return `Waiting ${view.elapsedSecs}s so far${deadline}.`
}

/** Whether the detail page should keep polling the service record. */
export function isServiceSettling(status: string | undefined): boolean {
  return status === 'creating' || status === 'starting'
}
