// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'

export interface LiveMetrics {
  cpu_percent: number
  /** CPU limit in whole cores (e.g. 1.0). null = no limit. */
  cpu_limit_cores: number | null
  memory_bytes: number
  /** Bytes; 0/null when no limit. Docker reports the host's total RAM as the
   * "limit" when no explicit limit is set, so callers should treat very large
   * values (e.g. > 100 GiB) as "no limit set". */
  memory_limit_bytes: number | null
  memory_percent: number | null
  network_rx_bytes: number
  network_tx_bytes: number
  network_rx_rate: number
  network_tx_rate: number
  /** Restart count from Docker. > 0 means the container was restarted at
   * least once since being created. */
  restart_count: number | null
  /** When the current process most recently started (RFC3339). Use this for
   * uptime — the container could have been restarted in place after a crash. */
  started_at: string | null
  timestamp: string
}

interface RawMetric {
  cpu_percent: number
  cpu_limit_cores?: number | null
  memory_bytes: number
  memory_limit_bytes?: number | null
  memory_percent?: number | null
  network_rx_bytes: number
  network_tx_bytes: number
  restart_count?: number | null
  started_at?: string | null
  timestamp: string
}

export function useContainerMetricsStream(
  projectId: string,
  environmentId: string,
  containerId: string,
  enabled: boolean = true
): { metrics: LiveMetrics | null; connected: boolean; error: string | null } {
  const [metrics, setMetrics] = useState<LiveMetrics | null>(null)
  const [connected, setConnected] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!enabled || !projectId || !environmentId || !containerId) return

    const eventSource = new EventSource(
      `/api/projects/${projectId}/environments/${environmentId}/containers/${containerId}/metrics/stream`
    )

    let prev: RawMetric | null = null

    eventSource.onopen = () => {
      setConnected(true)
      setError(null)
    }

    eventSource.onmessage = (event) => {
      try {
        const next = JSON.parse(event.data) as RawMetric
        let rxRate = 0
        let txRate = 0
        if (prev) {
          const dt = Math.max(
            (new Date(next.timestamp).getTime() -
              new Date(prev.timestamp).getTime()) /
              1000,
            1
          )
          rxRate =
            Math.max(next.network_rx_bytes - prev.network_rx_bytes, 0) / dt
          txRate =
            Math.max(next.network_tx_bytes - prev.network_tx_bytes, 0) / dt
        }
        prev = next
        setMetrics({
          ...next,
          cpu_limit_cores: next.cpu_limit_cores ?? null,
          memory_limit_bytes: next.memory_limit_bytes ?? null,
          memory_percent: next.memory_percent ?? null,
          restart_count: next.restart_count ?? null,
          started_at: next.started_at ?? null,
          network_rx_rate: rxRate,
          network_tx_rate: txRate,
        })
      } catch (e) {
        console.error('Failed to parse metrics:', e)
      }
    }

    eventSource.onerror = () => {
      setConnected(false)
      setError('Failed to connect to metrics stream')
      eventSource.close()
    }

    return () => eventSource.close()
  }, [projectId, environmentId, containerId, enabled])

  return { metrics, connected, error }
}
