// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getDeploymentJobLogs } from '@/api/client'
import { getDeploymentJobLogsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useState } from 'react'
import {
  deriveJobLogView,
  JobLogEntry,
  jobLogPhase,
  JobLogSnapshot,
  JobLogView,
  mergeLogEntries,
  parseJobLogContent,
  parseStreamMessage,
  reconnectDelayMs,
  shouldReadSnapshot,
  snapshotPollInterval,
  snapshotTailLines,
  SOCKET_CONNECT_TIMEOUT_MS,
  SocketState,
} from './job-log-state'

interface UseDeploymentJobLogsArgs {
  projectId: number
  deploymentId: number
  jobId: string
  jobStatus: string
}

export interface UseDeploymentJobLogsResult {
  entries: JobLogEntry[]
  view: JobLogView
  /** Detail for the `gone` and `error` states. */
  problemDetail: string | null
  retry: () => void
}

function problemDetailOf(error: unknown): string | null {
  if (typeof error === 'string') return error || null
  if (typeof error === 'object' && error !== null) {
    const { detail, title, message } = error as {
      detail?: unknown
      title?: unknown
      message?: unknown
    }
    if (typeof detail === 'string') return detail
    if (typeof title === 'string') return title
    if (typeof message === 'string') return message
  }
  return null
}

export function jobLogTailUrl(
  location: Pick<Location, 'protocol' | 'host'>,
  projectId: number,
  deploymentId: number,
  jobId: string
): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${protocol}//${location.host}/api/projects/${projectId}/deployments/${deploymentId}/jobs/${encodeURIComponent(jobId)}/logs/tail`
}

/**
 * Build logs for one deployment job: HTTP for finished or not-yet-started
 * jobs, the live tail socket for running ones, with HTTP polling whenever the
 * socket cannot connect or drops. See `job-log-state.ts` for the rules.
 */
export function useDeploymentJobLogs({
  projectId,
  deploymentId,
  jobId,
  jobStatus,
}: UseDeploymentJobLogsArgs): UseDeploymentJobLogsResult {
  const phase = jobLogPhase(jobStatus)
  const [socketState, setSocketState] = useState<SocketState>('idle')
  const [streamEntries, setStreamEntries] = useState<JobLogEntry[]>([])

  const path = {
    project_id: projectId,
    deployment_id: deploymentId,
    job_id: jobId,
  }
  const snapshotQuery = useQuery({
    // The phase is part of the key so the transition to `finished` always
    // performs a fresh read of the latest log tail instead of reusing a poll
    // taken while the job was still running.
    queryKey: [
      ...getDeploymentJobLogsQueryKey({
        path,
        query: { tail: snapshotTailLines(phase) },
      }),
      phase,
    ] as const,
    queryFn: async ({ signal }): Promise<JobLogSnapshot> => {
      const { data, error, response } = await getDeploymentJobLogs({
        path,
        query: { tail: snapshotTailLines(phase) },
        parseAs: 'text',
        signal,
      })
      if (response?.status === 404) {
        return {
          kind: 'gone',
          detail:
            problemDetailOf(error) ??
            `Logs for job '${jobId}' are no longer available`,
        }
      }
      if (error !== undefined || !response?.ok) {
        throw error ?? new Error(`Failed to read logs for job '${jobId}'`)
      }
      const received = parseJobLogContent(typeof data === 'string' ? data : '')
      // Keep earlier HTTP polls, just as we keep socket frames. Updating the
      // bounded buffer when the async read completes preserves overlap.
      if (!signal.aborted) {
        setStreamEntries((previous) => mergeLogEntries(previous, received))
      }
      return { kind: 'content', entries: received }
    },
    enabled: shouldReadSnapshot(phase, socketState),
    refetchInterval: (query) =>
      snapshotPollInterval(phase, socketState, query.state.data),
    // A poll that fails is retried by the next interval tick; a one-off read
    // of a finished job gets the usual retries.
    retry: phase === 'finished' ? 2 : false,
  })

  useEffect(() => {
    if (phase !== 'live') return

    let disposed = false
    let attempts = 0
    // Numbers plain-text frames that carry no line number of their own.
    let lastStreamLine = 0
    let socket: WebSocket | null = null
    let connectTimer: ReturnType<typeof setTimeout> | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null

    const clearConnectTimer = () => {
      if (connectTimer) clearTimeout(connectTimer)
      connectTimer = null
    }

    const detach = (target: WebSocket) => {
      target.onopen = null
      target.onmessage = null
      target.onerror = null
      target.onclose = null
    }

    // The socket is unusable: show the HTTP fallback and retry in the
    // background. Detaching first makes the timeout/close/error paths
    // idempotent for a given socket.
    const fail = (target: WebSocket) => {
      if (disposed || target !== socket) return
      clearConnectTimer()
      detach(target)
      if (target.readyState <= WebSocket.OPEN) target.close()
      socket = null
      setSocketState('failed')
      const delay = reconnectDelayMs(attempts)
      attempts += 1
      reconnectTimer = setTimeout(connect, delay)
    }

    const connect = () => {
      if (disposed) return
      // State is deliberately left alone here: the first attempt starts from
      // `idle` (rendered as connecting), and a background retry keeps showing
      // `failed` -- and keeps polling -- so the pane does not flicker back to
      // "connecting" every few seconds.
      let target: WebSocket
      try {
        target = new WebSocket(
          jobLogTailUrl(window.location, projectId, deploymentId, jobId)
        )
      } catch {
        // Invalid URL or a blocked scheme: treat it like a dropped socket.
        reconnectTimer = setTimeout(connect, reconnectDelayMs(attempts++))
        queueMicrotask(() => {
          if (!disposed) setSocketState('failed')
        })
        return
      }
      socket = target
      // A WebSocket upgrade that is never answered (e.g. a dev proxy that
      // does not forward it) stays in CONNECTING indefinitely without firing
      // `error` or `close`. Bound it.
      connectTimer = setTimeout(() => fail(target), SOCKET_CONNECT_TIMEOUT_MS)
      target.onopen = () => {
        clearConnectTimer()
        attempts = 0
        setSocketState('open')
      }
      target.onmessage = (event) => {
        const raw =
          typeof event.data === 'string' ? event.data : String(event.data)
        const message = parseStreamMessage(raw, lastStreamLine)
        if (message.kind === 'error') {
          fail(target)
          return
        }
        lastStreamLine = Math.max(lastStreamLine, message.entry.line)
        setStreamEntries((previous) =>
          mergeLogEntries(previous, [message.entry])
        )
      }
      target.onerror = () => fail(target)
      target.onclose = () => fail(target)
    }

    connect()

    return () => {
      disposed = true
      // The next live session (if any) starts from a fresh attempt.
      setSocketState('idle')
      clearConnectTimer()
      if (reconnectTimer) clearTimeout(reconnectTimer)
      if (socket) {
        detach(socket)
        socket.close(1000, 'Log viewer closed')
        socket = null
      }
    }
  }, [phase, projectId, deploymentId, jobId])

  const snapshot = snapshotQuery.data
  const entries = useMemo(
    () =>
      snapshot?.kind === 'content'
        ? mergeLogEntries(snapshot.entries, streamEntries)
        : streamEntries,
    [snapshot, streamEntries]
  )

  const view = deriveJobLogView({
    phase,
    socketState,
    entryCount: entries.length,
    snapshot: {
      data: snapshot,
      isPending: snapshotQuery.isPending,
      isError: snapshotQuery.isError,
    },
  })

  const problemDetail =
    snapshot?.kind === 'gone'
      ? snapshot.detail
      : snapshotQuery.isError
        ? problemDetailOf(snapshotQuery.error)
        : null

  return {
    entries,
    view,
    problemDetail,
    retry: () => {
      void snapshotQuery.refetch()
    },
  }
}
