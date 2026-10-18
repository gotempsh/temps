// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getRuntimeHealthOptions,
  updateRuntimeHealthMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { Callout, Detail, Status, TimeChart } from '@temps-sdk/ds'
import {
  Button,
  Skeleton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@temps-sdk/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router'
import { useState } from 'react'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { toast } from 'sonner'
import { formatDistanceToNow } from 'date-fns'
import {
  formatRuntimePercent,
  runtimeHealthExplanation,
  runtimeStateTone,
} from './runtime-health-display'

interface Props {
  project: ProjectResponse
  environment: EnvironmentResponse
}

export function RuntimeHealthPanel({ project, environment }: Props) {
  const [page, setPage] = useState(1)
  const queryClient = useQueryClient()
  const options = getRuntimeHealthOptions({
    path: { project_id: project.id, environment_id: environment.id },
  })
  const healthQuery = useQuery({ ...options, refetchInterval: 10_000 })
  const update = useMutation({
    ...updateRuntimeHealthMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: options.queryKey })
      toast.success('Runtime monitoring policy saved')
    },
    meta: { errorTitle: 'Failed to update runtime monitoring' },
  })
  const health = healthQuery.data
  const explanation = health ? runtimeHealthExplanation(health) : undefined
  const settingsHref = `/projects/${project.slug}/settings/delivery?section=deployment`
  const data =
    health?.history.map((bucket) => ({
      at: bucket.bucket_at,
      availability: bucket.coverage.availability_percent,
      coverage: bucket.coverage.coverage_percent,
    })) ?? []

  const totalPages = Math.max(1, Math.ceil((health?.replicas.length ?? 0) / 10))
  const currentPage = Math.min(page, totalPages)
  const replicas =
    health?.replicas.slice((currentPage - 1) * 10, currentPage * 10) ?? []

  if (healthQuery.isPending)
    return (
      <div
        role="status"
        className="space-y-3 rounded-lg border p-4"
        aria-label={`${environment.name} runtime health loading`}
      >
        <Skeleton className="h-6 w-48" />
        <Skeleton className="h-20 w-full" />
        <Skeleton className="h-44 w-full" />
        <Skeleton className="h-20 w-full" />
      </div>
    )
  if (healthQuery.isError)
    return (
      <Callout
        tone="error"
        title={`${environment.name} runtime health could not be loaded`}
      >
        <Button
          variant="outline"
          size="sm"
          onClick={() => healthQuery.refetch()}
        >
          Retry
        </Button>
      </Callout>
    )
  if (!health) return null

  return (
    <section
      aria-label={`${environment.name} runtime health`}
      className="space-y-4 border-b pb-6 last:border-b-0"
    >
      <Detail
        embedded
        title={`${environment.name} runtime health`}
        description={
          health.portless === true
            ? 'Background worker · continuous checks on the assigned workload node'
            : 'Continuous checks on the assigned workload node'
        }
        verdict={
          <Status
            tone={runtimeStateTone(health.state)}
            label={
              health.policy.paused || !health.policy.enabled
                ? 'Paused'
                : health.state
            }
          />
        }
        actions={
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" asChild>
              <Link to={settingsHref}>Probe settings</Link>
            </Button>
            <Button
              variant="outline"
              size="sm"
              aria-disabled={update.isPending || !health.configured}
              onClick={() => {
                if (update.isPending || !health.configured) return
                update.mutate({
                  path: {
                    project_id: project.id,
                    environment_id: environment.id,
                  },
                  body: {
                    ...health.policy,
                    enabled: true,
                    paused: health.policy.enabled
                      ? !health.policy.paused
                      : false,
                  },
                })
              }}
            >
              {update.isPending
                ? 'Saving…'
                : health.policy.paused || !health.policy.enabled
                  ? 'Resume monitoring'
                  : 'Pause monitoring'}
            </Button>
          </div>
        }
        facts={[
          {
            label: 'Availability · observed time',
            value: formatRuntimePercent(health.coverage.availability_percent),
          },
          {
            label: 'Coverage · eligible time',
            value: formatRuntimePercent(health.coverage.coverage_percent),
          },
          {
            label: 'Unknown time',
            value: `${Math.round(health.coverage.unknown_seconds)}s`,
          },
          { label: 'Probe', value: health.probe?.kind ?? 'Not configured' },
          {
            label: 'Required healthy replicas',
            value: health.probe
              ? `${health.probe.required_healthy} / ${health.probe.desired_replicas}`
              : '—',
          },
        ]}
        main={
          <div className="space-y-4">
            {explanation && (
              <Callout tone="info">
                {explanation}{' '}
                <Link className="underline" to={settingsHref}>
                  Open probe settings
                </Link>
              </Callout>
            )}
            {health.probe && (
              <div className="space-y-1 text-sm">
                <p className="break-words font-medium">
                  {health.probe.description}
                </p>
                <p className="text-muted-foreground">
                  Source: {health.probe.source.replace(/_/g, ' ')} · every{' '}
                  {health.probe.interval_seconds}s · timeout{' '}
                  {health.probe.timeout_seconds}s · alarm after{' '}
                  {health.probe.failure_threshold} failures · recovery after{' '}
                  {health.probe.recovery_threshold} successes
                </p>
              </div>
            )}
            {data.length > 0 && (
              <div
                role="region"
                aria-label="Current deployment runtime availability and coverage for the last 24 hours"
              >
                <TimeChart
                  data={data}
                  xKey="at"
                  height={180}
                  connectNulls={false}
                  yDomain={[0, 100]}
                  series={[
                    {
                      dataKey: 'availability',
                      label: 'Availability during observed time',
                      tone: 'good',
                    },
                    {
                      dataKey: 'coverage',
                      label: 'Observation coverage',
                      tone: 'neutral',
                    },
                  ]}
                  yTickFormatter={(value) => `${value}%`}
                  tooltipValueFormatter={(value) => `${value.toFixed(1)}%`}
                  xTickFormatter={(value) =>
                    new Date(value).toLocaleTimeString([], {
                      hour: '2-digit',
                      minute: '2-digit',
                    })
                  }
                />
              </div>
            )}
            {health.replicas.length > 0 && (
              <div className="space-y-3">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Replica</TableHead>
                      <TableHead>Runtime state</TableHead>
                      <TableHead>Reason</TableHead>
                      <TableHead>Last accepted check</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {replicas.map((replica) => (
                      <TableRow key={replica.replica_id}>
                        <TableCell>{replica.replica_id}</TableCell>
                        <TableCell>
                          <Status
                            tone={runtimeStateTone(replica.state)}
                            label={replica.state}
                            variant="dot"
                          />
                        </TableCell>
                        <TableCell>
                          {replica.reason.replace(/_/g, ' ')}
                        </TableCell>
                        <TableCell>
                          {replica.checked_at
                            ? formatDistanceToNow(
                                new Date(replica.checked_at),
                                {
                                  addSuffix: true,
                                }
                              )
                            : 'No accepted observations'}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                <ResponsivePagination
                  page={currentPage}
                  pageSize={10}
                  total={health.replicas.length}
                  totalPages={totalPages}
                  onPageChange={setPage}
                  ariaLabel="Runtime replica pagination"
                />
              </div>
            )}
            <p className="text-xs text-muted-foreground">
              Current deployment, last 24 hours. Availability = healthy /
              (healthy + unhealthy). Coverage = observed / eligible time.
              Unknown time stays unknown; paused and intentionally sleeping time
              is excluded. The deployment gate result is recorded separately.
            </p>
          </div>
        }
      />
    </section>
  )
}
