// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type LogConnectionStatus =
  'connecting' | 'connected' | 'error' | 'permanent_error' | 'ended'

/** Attempts per stream before the viewer stops and offers Retry. */
export const MAX_STREAM_ATTEMPTS = 3

/** One container's stream in "All containers" mode. */
export interface MergedStream {
  open: boolean
  /** Closes since it last proved stable. */
  failures: number
  /** The server closed it normally: nothing more will arrive on it. */
  ended: boolean
}

/** A stream stops reconnecting at the limit, whatever the others are doing. */
export function streamGaveUp(stream: Pick<MergedStream, 'failures'>) {
  return stream.failures >= MAX_STREAM_ATTEMPTS
}

/**
 * The one status shown for the streams merged in "All containers" mode: any
 * open stream means there is something to read, and only when none is open do
 * failures become the answer. Which sources gave up is reported apart, so a
 * failed one still shows while the others keep streaming.
 */
export function mergedStreamStatus(streams: readonly MergedStream[]): {
  status: LogConnectionStatus
  attempt: number
} {
  if (streams.some((stream) => stream.open)) {
    return { status: 'connected', attempt: 0 }
  }
  const attempt = Math.min(
    MAX_STREAM_ATTEMPTS,
    Math.max(0, ...streams.map((stream) => stream.failures))
  )
  // Still dialing or waiting to redial, so this is not the final answer yet.
  const pending = streams.filter(
    (stream) => !stream.ended && !streamGaveUp(stream)
  )
  if (pending.length > 0) {
    return { status: attempt === 0 ? 'connecting' : 'error', attempt }
  }
  return {
    status: streams.some(streamGaveUp) ? 'permanent_error' : 'ended',
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
