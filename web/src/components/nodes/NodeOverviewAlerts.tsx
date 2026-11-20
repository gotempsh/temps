// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NodeInfoResponse } from '@/api/client'
import {
  clusterDnsStatusOptions,
  listSystemAlarmsOptions,
  nodeMetricsGetAlertRulesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  alarmsOfRules,
  nodeOverviewAlerts,
  type NodeAlert,
} from '@/lib/node-detail'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, AlertTriangle } from 'lucide-react'
import { Link } from 'react-router'

/** Alarm type of a node alert rule (`temps_monitoring::evaluator`). */
const NODE_ALARM_TYPE = 'node_metric_threshold'

/**
 * What needs attention on a node, above its metrics. Each source is
 * best-effort: a role that cannot read the cluster DNS status or the alarms
 * still sees the node's own problems.
 */
export function NodeOverviewAlerts({ node }: { node: NodeInfoResponse }) {
  const dnsStatus = useQuery({
    ...clusterDnsStatusOptions(),
    retry: false,
    refetchInterval: 30_000,
  })
  const rules = useQuery({
    ...nodeMetricsGetAlertRulesOptions({ path: { id: node.id } }),
    retry: false,
    staleTime: 30_000,
  })
  const alarms = useQuery({
    ...listSystemAlarmsOptions({
      query: { alarm_type: NODE_ALARM_TYPE, status: 'firing', page_size: 100 },
    }),
    retry: false,
    refetchInterval: 30_000,
  })

  const resolver = dnsStatus.data?.nodes.find(
    (entry) => entry.node_id === node.id
  )
  const ruleIds = new Set((rules.data ?? []).map((rule) => rule.id))
  const firing = alarmsOfRules(alarms.data?.items ?? [], ruleIds)
  const alerts = nodeOverviewAlerts({
    node,
    dns: dnsStatus.data
      ? {
          enabled: dnsStatus.data.cluster_dns_enabled,
          running: resolver?.dns_resolver_running,
          lastError: resolver?.dns_resolver_last_error,
          applicationsWithoutResolver: (
            dnsStatus.data.deployments_without_resolver ?? []
          ).filter((entry) =>
            entry.hosts.some((host) => host.node_id === node.id)
          ).length,
        }
      : undefined,
    firingAlarms: firing,
  })

  return <NodeAlertList alerts={alerts} />
}

export function NodeAlertList({ alerts }: { alerts: NodeAlert[] }) {
  if (alerts.length === 0) return null
  return (
    <div className="space-y-3">
      {alerts.map((alert) => (
        <Alert
          key={alert.key}
          variant={alert.tone === 'error' ? 'destructive' : undefined}
          className={cn(
            alert.tone === 'warn' && 'border-amber-500/30 bg-amber-500/5'
          )}
        >
          {alert.tone === 'error' ? (
            <AlertCircle className="h-4 w-4" />
          ) : (
            <AlertTriangle className="h-4 w-4 text-amber-500" />
          )}
          <AlertTitle>{alert.title}</AlertTitle>
          <AlertDescription className="space-y-2">
            <p>{alert.description}</p>
            {alert.action && (
              <Button asChild size="sm" variant="outline">
                <Link to={alert.action.to}>{alert.action.label}</Link>
              </Button>
            )}
          </AlertDescription>
        </Alert>
      ))}
    </div>
  )
}
