// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps containers` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  containerMetricsGetHistory,
  getContainerEnvironmentVariable,
  streamContainerMetrics,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerContainersEndpoints(program: Command): void {
  const group = topGroup(program, 'containers', "containers")

  endpoint(group, getContainerEnvironmentVariable, {
    name: "get-environment-variable",
    description: "Get container environment variable",
    path: { project_id: 'project', environment_id: 'int', container_id: 'string', variable_name: 'string' },
  })

  endpoint(group, containerMetricsGetHistory, {
    name: "metrics-get-history",
    description: "Fetch a time-series range for a single container resource metric (recorded by the container health monitor..",
    path: { project_id: 'project', environment_id: 'int', container_id: 'string' },
    query: {
      metric: { kind: 'string!', help: "Dotted metric name, e.g. 'container.cpu_percent' or 'container.memory_used_bytes'" },
      range: { kind: 'string', help: "Time window: '1h', '6h', '24h', or '7d' (defaults to '1h')" },
    },
  })

  endpoint(group, streamContainerMetrics, {
    name: "stream-metrics",
    description: "Stream container metrics via Server-Sent Events (SSE)",
    path: { project_id: 'project', environment_id: 'int', container_id: 'string' },
    query: {
      interval: { kind: 'int', help: "Update interval in milliseconds (default: 1000)" },
    },
  })
}
