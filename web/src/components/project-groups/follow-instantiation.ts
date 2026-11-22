// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { InstantiationResponse } from '@/api/client'

/** How often a running instantiation is polled. */
export const FOLLOW_INTERVAL_MS = 1500
/**
 * How long a dialog follows one before handing it to the background. The
 * server keeps running it; UnfinishedImportsNotice picks it up from there.
 */
export const FOLLOW_TIMEOUT_MS = 10 * 60 * 1000
/** Consecutive failed polls tolerated before giving up. */
export const FOLLOW_MAX_POLL_FAILURES = 3

export function isInstantiationFinished(
  instantiation: Pick<InstantiationResponse, 'status'>
): boolean {
  return (
    instantiation.status !== 'pending' && instantiation.status !== 'running'
  )
}

/**
 * Polling could not read the instantiation. It may well still be running on
 * the server, so the message names it instead of calling it failed.
 */
export class InstantiationPollError extends Error {
  readonly instantiationId: number
  readonly pollError: unknown

  constructor(instantiationId: number, cause: unknown) {
    super(
      `Lost track of #${instantiationId}: its progress could not be loaded. It may still be running on the server.`
    )
    this.name = 'InstantiationPollError'
    this.instantiationId = instantiationId
    this.pollError = cause
  }
}

export type FollowOutcome =
  | { kind: 'finished'; instantiation: InstantiationResponse }
  /** Still running after the deadline; the server carries on. */
  | { kind: 'timed_out'; instantiation: InstantiationResponse | null }
  /** The caller stopped following (dialog closed, component unmounted). */
  | { kind: 'aborted'; instantiation: InstantiationResponse | null }

export interface FollowOptions {
  signal: AbortSignal
  fetchInstantiation: (
    id: number,
    signal: AbortSignal
  ) => Promise<InstantiationResponse>
  onUpdate?: (instantiation: InstantiationResponse) => void
  intervalMs?: number
  timeoutMs?: number
  maxPollFailures?: number
  now?: () => number
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>
}

function abortableSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve()
    const timer = setTimeout(done, ms)
    function done() {
      clearTimeout(timer)
      signal.removeEventListener('abort', done)
      resolve()
    }
    signal.addEventListener('abort', done, { once: true })
  })
}

/**
 * Poll an instantiation until it finishes, the deadline passes or the caller
 * aborts. A few failed polls in a row are tolerated (a restarting server);
 * past that it throws {@link InstantiationPollError}.
 */
export async function followInstantiation(
  id: number,
  {
    signal,
    fetchInstantiation,
    onUpdate,
    intervalMs = FOLLOW_INTERVAL_MS,
    timeoutMs = FOLLOW_TIMEOUT_MS,
    maxPollFailures = FOLLOW_MAX_POLL_FAILURES,
    now = Date.now,
    sleep = abortableSleep,
  }: FollowOptions
): Promise<FollowOutcome> {
  const deadline = now() + timeoutMs
  let last: InstantiationResponse | null = null
  let failures = 0
  for (;;) {
    if (signal.aborted) return { kind: 'aborted', instantiation: last }
    try {
      last = await fetchInstantiation(id, signal)
      failures = 0
    } catch (error) {
      if (signal.aborted) return { kind: 'aborted', instantiation: last }
      failures += 1
      if (failures >= maxPollFailures) {
        throw new InstantiationPollError(id, error)
      }
    }
    if (last) {
      onUpdate?.(last)
      if (isInstantiationFinished(last)) {
        return { kind: 'finished', instantiation: last }
      }
    }
    if (now() >= deadline) return { kind: 'timed_out', instantiation: last }
    await sleep(intervalMs, signal)
  }
}
