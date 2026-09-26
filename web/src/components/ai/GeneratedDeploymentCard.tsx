// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentJobResponse } from '@/api/client'
import {
  getDeploymentJobsOptions,
  getProjectDeploymentsOptions,
  getProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowRight,
  CheckCircle2,
  Circle,
  GitBranch,
  Rocket,
  XCircle,
} from 'lucide-react'
import { Link } from 'react-router'
import {
  deploymentPollingInterval,
  deploymentReference,
  isActiveDeploymentStatus,
  isFailedDeploymentStatus,
  matchingDeployment,
} from './deployment-card-state'

function statusPresentation(status: string) {
  if (status === 'completed') {
    return {
      label: 'Ready',
      className: 'text-green-600 dark:text-green-400',
    }
  }
  if (isFailedDeploymentStatus(status)) {
    return {
      label: status === 'failed' ? 'Failed' : 'Cancelled',
      className: 'text-destructive',
    }
  }
  if (status === 'paused') {
    return { label: 'Paused', className: 'text-amber-600 dark:text-amber-400' }
  }
  return {
    label: status === 'running' ? 'Deploying' : 'Queued',
    className: 'text-blue-600 dark:text-blue-400',
  }
}

function JobStatusIcon({ status }: { status: string }) {
  if (status === 'success') {
    return (
      <CheckCircle2 className="size-3.5 text-green-600 dark:text-green-400" />
    )
  }
  if (status === 'failure' || status === 'cancelled') {
    return <XCircle className="size-3.5 text-destructive" />
  }
  if (status === 'running') {
    return (
      <span
        aria-hidden
        className="grid shrink-0 grid-cols-[repeat(3,3px)] gap-px"
      >
        {[0, 90, 180].map((delay) => (
          <span
            key={delay}
            className="ai-activity-pixel size-[3px] rounded-[1px] bg-foreground"
            style={{ animationDelay: `${delay}ms` }}
          />
        ))}
      </span>
    )
  }
  return <Circle className="size-3.5 text-muted-foreground" />
}

function DeploymentJobs({ jobs }: { jobs: DeploymentJobResponse[] }) {
  if (jobs.length === 0) return null
  return (
    <ol className="grid gap-1.5 border-t px-3 py-3 sm:px-4">
      {jobs.map((job) => (
        <li key={job.id} className="flex min-w-0 items-center gap-2 text-xs">
          <JobStatusIcon status={job.status} />
          <span className="min-w-0 flex-1 truncate">{job.name}</span>
          <span className="capitalize text-muted-foreground">{job.status}</span>
        </li>
      ))}
    </ol>
  )
}

export function GeneratedDeploymentCard({
  paramsJson,
  resultJson,
  actionStatus,
  createdAt,
  summary,
  statusLabel,
  statusClassName,
}: {
  paramsJson: string | null
  resultJson: string | null
  actionStatus: string
  createdAt: string | null
  summary?: string
  statusLabel: string
  statusClassName?: string
}) {
  const reference = deploymentReference(paramsJson, resultJson, createdAt)
  const canResolve = actionStatus === 'executed' && reference !== null

  const deploymentsQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: reference?.projectId ?? 0 },
      query: {
        page: 1,
        per_page: 20,
        ...(reference?.environmentId
          ? { environment_id: reference.environmentId }
          : {}),
      },
    }),
    enabled: canResolve,
    refetchInterval: (query) => {
      const deployment = reference
        ? matchingDeployment(query.state.data?.deployments ?? [], reference)
        : null
      return deploymentPollingInterval(
        actionStatus,
        deployment?.status ?? null,
        createdAt
      )
    },
  })

  const deployment = reference
    ? matchingDeployment(deploymentsQuery.data?.deployments ?? [], reference)
    : null
  const projectQuery = useQuery({
    ...getProjectOptions({ path: { id: reference?.projectId ?? 0 } }),
    enabled: Boolean(reference),
  })
  const jobsQuery = useQuery({
    ...getDeploymentJobsOptions({
      path: {
        project_id: reference?.projectId ?? 0,
        deployment_id: deployment?.id ?? 0,
      },
    }),
    enabled: Boolean(deployment),
    refetchInterval: () =>
      deployment && isActiveDeploymentStatus(deployment.status) ? 1500 : false,
  })

  const liveStatus = deployment
    ? statusPresentation(deployment.status)
    : actionStatus === 'executed' && reference
      ? { label: 'Queued', className: 'text-blue-600 dark:text-blue-400' }
      : { label: statusLabel, className: statusClassName }
  const isLive =
    actionStatus === 'executing' ||
    (actionStatus === 'executed' &&
      reference !== null &&
      (!deployment || isActiveDeploymentStatus(deployment.status)))
  const projectSlug = projectQuery.data?.slug

  return (
    <div className="@container min-w-0" aria-live="polite">
      <div className="flex min-w-0 items-start gap-3 px-3 py-3 sm:px-4">
        <div className="flex size-11 shrink-0 items-center justify-center rounded-lg border bg-background text-blue-600 dark:text-blue-400">
          <Rocket className="size-6" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <h3 className="truncate text-base font-semibold sm:text-sm">
              {deployment ? `Deployment #${deployment.id}` : 'Deploy project'}
            </h3>
            {isLive && (
              <span className="ai-activity-shimmer bg-gradient-to-r from-muted-foreground via-foreground to-muted-foreground bg-[length:200%_100%] bg-clip-text text-[11px] font-medium text-transparent">
                Live
              </span>
            )}
          </div>
          {summary && (
            <p className="mt-1 text-sm text-muted-foreground sm:text-xs">
              {summary}
            </p>
          )}
          <div
            className={cn(
              'mt-1.5 flex items-center gap-1.5 text-xs font-medium',
              liveStatus.className
            )}
          >
            {isLive ? (
              <span className="grid shrink-0 grid-cols-[repeat(3,3px)] gap-px">
                {[0, 90, 180].map((delay) => (
                  <span
                    key={delay}
                    className="ai-activity-pixel size-[3px] rounded-[1px] bg-current"
                    style={{ animationDelay: `${delay}ms` }}
                  />
                ))}
              </span>
            ) : deployment?.status === 'completed' ? (
              <CheckCircle2 className="size-4" />
            ) : isFailedDeploymentStatus(deployment?.status ?? '') ? (
              <XCircle className="size-4" />
            ) : (
              <Rocket className="size-4" />
            )}
            {liveStatus.label}
          </div>
        </div>
      </div>

      {reference && (
        <dl className="grid grid-cols-1 border-t bg-background/40 @sm:grid-cols-2">
          <div className="min-w-0 border-b px-3 py-2.5 @sm:border-r sm:px-4">
            <dt className="text-xs font-medium text-muted-foreground">
              Environment
            </dt>
            <dd className="mt-1 truncate text-xs font-medium">
              {deployment?.environment.name ??
                (reference.environmentId
                  ? `Environment ${reference.environmentId}`
                  : 'Resolving')}
            </dd>
          </div>
          <div className="min-w-0 border-b px-3 py-2.5 sm:px-4">
            <dt className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
              <GitBranch className="size-3.5" /> Source
            </dt>
            <dd className="mt-1 truncate font-mono text-xs font-medium">
              {deployment?.branch ??
                reference.branch ??
                reference.tag ??
                'Default branch'}
            </dd>
          </div>
        </dl>
      )}

      <DeploymentJobs jobs={jobsQuery.data?.jobs ?? []} />

      {actionStatus === 'executed' &&
        reference &&
        !deployment &&
        !deploymentsQuery.isFetching && (
          <p className="border-t px-3 py-2.5 text-xs text-muted-foreground sm:px-4">
            {deploymentsQuery.isError
              ? 'Temps could not load deployment progress. Refresh to retry.'
              : 'The trigger was accepted. Waiting for the deployment worker to create its record.'}
          </p>
        )}

      {actionStatus === 'executed' && !reference && (
        <p className="border-t px-3 py-2.5 text-xs text-muted-foreground sm:px-4">
          The trigger completed, but its response did not include a project id,
          so live deployment progress is unavailable.
        </p>
      )}

      {deployment && projectSlug && (
        <div className="flex items-center gap-2 border-t px-3 py-2.5 sm:px-4">
          <Button asChild size="sm" variant="outline" className="h-8">
            <Link to={`/projects/${projectSlug}/deployments/${deployment.id}`}>
              View deployment
              <ArrowRight aria-hidden="true" />
            </Link>
          </Button>
          {deployment.status === 'completed' && deployment.url && (
            <Button asChild size="sm" variant="ghost" className="h-8">
              <a
                href={deployment.url}
                target="_blank"
                rel="noopener noreferrer"
              >
                Open application
              </a>
            </Button>
          )}
        </div>
      )}
    </div>
  )
}
