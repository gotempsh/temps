// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type LogConnectionStatus =
  'connecting' | 'connected' | 'error' | 'permanent_error'

/** Attempts per stream before the viewer stops and offers Retry. */
export const MAX_STREAM_ATTEMPTS = 3

/**
 * The one status shown for the streams merged in "All containers" mode: any
 * open stream means there is something to read, and only when none is open do
 * failures become the answer. `failedAttempts` counts closes across all
 * streams since one last proved stable.
 */
export function mergedStreamStatus(
  openStreams: number,
  failedAttempts: number,
  streamCount: number
): { status: LogConnectionStatus; attempt: number } {
  if (openStreams > 0) return { status: 'connected', attempt: 0 }
  if (failedAttempts === 0) return { status: 'connecting', attempt: 0 }
  const attempt = Math.min(
    MAX_STREAM_ATTEMPTS,
    Math.ceil(failedAttempts / Math.max(streamCount, 1))
  )
  return {
    status:
      failedAttempts >= MAX_STREAM_ATTEMPTS * streamCount
        ? 'permanent_error'
        : 'error',
    attempt,
  }
}

/**
 * What an empty log pane says, so blank never means "something may be broken".
 * `null` while a connection alert above the pane already explains it.
 */
export function logPaneEmptyMessage({
  mode,
  status,
  totalLines,
  noContainers,
  allContainers,
}: {
  mode: 'live' | 'pause' | 'interval'
  status: LogConnectionStatus
  totalLines: number
  noContainers: boolean
  allContainers: boolean
}): string | null {
  if (noContainers) return 'No containers are running in this environment.'
  if (totalLines > 0) return 'No lines match the selected levels.'
  if (mode === 'pause') return 'Paused. Switch to Live to stream logs.'
  if (mode === 'interval') return 'No log lines in this window yet.'
  if (status !== 'connected') return null
  return allContainers
    ? 'Connected. Waiting for these containers to write a log line.'
    : 'Connected. Waiting for this container to write a log line.'
}
