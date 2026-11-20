// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getDeploymentRegressionsOptions,
  getProjectDeploymentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { changePercent, comparableDeployments } from '@/lib/operation-trend'
import { useQuery } from '@tanstack/react-query'
import { Rocket } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import type { TrendTarget } from './OperationTrendSheet'

interface DeploymentRegressionsCardProps {
  projectId: number
  projectSlug: string
  environmentId?: number
  onOpenTrend: (target: TrendTarget) => void
}

const NO_DEPLOYMENTS_YET =
  'No deployment of this project has gone live yet. After one does, this compares each operation\'s p95 in the day after it with the week before, for example "GET /checkout is 40% slower since deployment #812".'

function formatMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  return `${ms.toFixed(ms >= 10 ? 0 : 1)}ms`
}

/**
 * Which operations got slower or faster after a deployment: each operation's
 * hourly p95 in the day after it went live against the week before it.
 */
export function DeploymentRegressionsCard({
  projectId,
  projectSlug,
  environmentId,
  onOpenTrend,
}: DeploymentRegressionsCardProps) {
  const deploymentsQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: projectId },
      query: {
        per_page: 20,
        ...(environmentId != null ? { environment_id: environmentId } : {}),
      },
    }),
  })
  const deployments = useMemo(
    () => comparableDeployments(deploymentsQuery.data?.deployments ?? []),
    [deploymentsQuery.data]
  )
  const [picked, setPicked] = useState<string | null>(null)
  const deploymentId =
    picked ?? (deployments[0] ? String(deployments[0].id) : null)

  const regressionsQuery = useQuery({
    ...getDeploymentRegressionsOptions({
      query: {
        project_id: projectId,
        deployment_id: Number(deploymentId ?? 0),
      },
    }),
    enabled: deploymentId !== null,
  })
  const result = regressionsQuery.data

  return (
    <Card>
      <CardHeader className="flex flex-col gap-2 space-y-0 sm:flex-row sm:items-center sm:justify-between">
        <CardTitle className="flex items-center gap-2 text-base">
          <Rocket className="h-4 w-4" />
          Did a deployment slow anything down?
        </CardTitle>
        {deployments.length > 0 && deploymentId && (
          <Select value={deploymentId} onValueChange={setPicked}>
            <SelectTrigger
              className="w-full sm:w-[280px]"
              aria-label="Deployment to compare"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {deployments.map((d) => (
                <SelectItem key={d.id} value={String(d.id)}>
                  #{d.id}
                  {d.commit_hash ? ` · ${d.commit_hash.slice(0, 7)}` : ''}
                  {d.commit_message
                    ? ` · ${d.commit_message.split('\n')[0].slice(0, 40)}`
                    : ''}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
      </CardHeader>
      <CardContent className="space-y-3">
        {deploymentsQuery.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : deployments.length === 0 ? (
          <div className="flex flex-col gap-2 text-sm text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
            <span>{NO_DEPLOYMENTS_YET}</span>
            <Button variant="outline" size="sm" asChild>
              <Link to={`/projects/${projectSlug}/deployments`}>
                Go to deployments
              </Link>
            </Button>
          </div>
        ) : regressionsQuery.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : regressionsQuery.isError ? (
          <p className="text-sm text-muted-foreground">
            {regressionsQuery.error instanceof Error
              ? regressionsQuery.error.message
              : 'This deployment could not be compared.'}
          </p>
        ) : result && !result.tracking.configured ? (
          <p className="text-sm text-muted-foreground">
            {result.tracking.reason}
          </p>
        ) : result ? (
          <>
            <p className="text-sm text-muted-foreground">
              {result.regressed} slower, {result.improved} faster,{' '}
              {result.unchanged} unchanged
              {result.insufficient_after > 0
                ? `; ${result.insufficient_after} still waiting for ${result.min_after_hours} complete hours after the deployment`
                : ''}
              {result.insufficient_baseline > 0
                ? `; ${result.insufficient_baseline} without enough history before it`
                : ''}
              .
            </p>
            {result.changes.length > 0 && (
              <ul className="divide-y rounded-md border">
                {result.changes.map((change) => (
                  <li
                    key={`${change.service_name}-${change.span_name}`}
                    className="flex flex-col gap-1 p-2 sm:flex-row sm:items-center sm:justify-between"
                  >
                    <button
                      type="button"
                      className="min-w-0 text-left"
                      onClick={() =>
                        onOpenTrend({
                          serviceName: change.service_name,
                          spanName: change.span_name,
                        })
                      }
                    >
                      <span className="block truncate font-medium underline-offset-4 hover:underline">
                        {change.span_name}
                      </span>
                      <span className="block text-xs text-muted-foreground">
                        {change.service_name}
                      </span>
                    </button>
                    <div className="flex shrink-0 items-center gap-2 text-sm tabular-nums">
                      <span className="text-muted-foreground">
                        p95 {formatMs(change.baseline_p95_ms)} →{' '}
                        {formatMs(change.after_p95_ms)}
                      </span>
                      <Badge
                        variant={
                          change.change === 'regressed'
                            ? 'destructive'
                            : 'secondary'
                        }
                      >
                        {changePercent(change.change_ratio)}
                      </Badge>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </>
        ) : null}
      </CardContent>
    </Card>
  )
}
