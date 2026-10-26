// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  adminListNodesOptions,
  nodeMetricsGetReportingOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { metricNodeOptions, resolveNodeScope } from '@/lib/node-scope'
import { refetchUnlessRefused } from '@/lib/query-retry'
import { useQuery } from '@tanstack/react-query'
import { useCallback, useMemo } from 'react'
import { useSearchParams } from 'react-router'

/**
 * The page's node scope from the URL. `ready` is false while the node list
 * loads, so metric queries are only sent for the scope that will be shown.
 * A failed node list leaves the page on the control plane.
 */
export function useMetricNodeScope(enabled: boolean) {
  const [searchParams, setSearchParams] = useSearchParams()
  const nodesQ = useQuery({
    ...adminListNodesOptions(),
    enabled,
    staleTime: 60_000,
    retry: false,
  })
  const options = useMemo(
    () => metricNodeOptions(nodesQ.data?.nodes),
    [nodesQ.data]
  )
  const param = searchParams.get('node')
  const scope = useMemo(
    () => resolveNodeScope(param, options),
    [param, options]
  )
  const multiNode = options.length > 1
  const reportingQ = useQuery({
    ...nodeMetricsGetReportingOptions(),
    enabled: enabled && multiNode,
    staleTime: 15_000,
    refetchInterval: refetchUnlessRefused(30_000),
    retry: false,
  })
  const setScope = useCallback(
    (value: string) =>
      setSearchParams(
        (prev) => {
          const p = new URLSearchParams(prev)
          p.set('node', value)
          return p
        },
        { replace: true }
      ),
    [setSearchParams]
  )
  return {
    scope,
    options,
    setScope,
    ready: !enabled || !nodesQ.isPending,
    statuses: reportingQ.data ?? [],
  }
}
