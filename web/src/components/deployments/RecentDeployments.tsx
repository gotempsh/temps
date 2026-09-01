// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  DeploymentResponse,
  ProjectResponse,
  SourceType,
} from '@/api/client'
import { getProjectDeploymentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { DeploymentStatusBadge } from '@/components/deployment/DeploymentStatusBadge'
import { Card } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { deploymentSourceSummary } from '@/lib/deployment-source-summary'
import { recentDeploymentsRefetchInterval } from '@/lib/recent-deployments'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowRight,
  CheckCircle2,
  ChevronRight,
  Container,
  FileArchive,
  GitBranch,
  GitCommit,
  Package,
} from 'lucide-react'
import { Link } from 'react-router'

const RECENT_DEPLOYMENT_COUNT = 5

export function RecentDeployments({ project }: { project: ProjectResponse }) {
  const deploymentsQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: { page: 1, per_page: RECENT_DEPLOYMENT_COUNT },
    }),
    refetchOnWindowFocus: true,
    refetchInterval: (query) =>
      recentDeploymentsRefetchInterval(query.state.data?.deployments),
  })

  if (deploymentsQuery.isLoading) {
    return <RecentDeploymentsSkeleton />
  }

  if (deploymentsQuery.isError || !deploymentsQuery.data?.deployments.length) {
    return null
  }

  const deployments = deploymentsQuery.data.deployments

  return (
    <section aria-labelledby="recent-deployments-title">
      <div className="mb-3 flex items-end justify-between gap-4">
        <div>
          <h2 id="recent-deployments-title" className="text-sm font-semibold">
            Recent deployments
          </h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            The latest releases across every environment.
          </p>
        </div>
        <Link
          to={`/projects/${project.slug}/deployments`}
          className="inline-flex shrink-0 items-center gap-1 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          View all
          <ArrowRight className="size-3.5" aria-hidden="true" />
        </Link>
      </div>
      <Card className="overflow-hidden">
        <ul className="divide-y divide-border">
          {deployments.map((deployment) => (
            <RecentDeploymentRow
              key={deployment.id}
              deployment={deployment}
              projectSlug={project.slug}
              projectSourceType={project.source_type}
            />
          ))}
        </ul>
      </Card>
    </section>
  )
}

function RecentDeploymentRow({
  deployment,
  projectSlug,
  projectSourceType,
}: {
  deployment: DeploymentResponse
  projectSlug: string
  projectSourceType?: SourceType
}) {
  return (
    <li>
      <Link
        to={`/projects/${projectSlug}/deployments/${deployment.id}`}
        className="group flex min-w-0 flex-col gap-2.5 px-4 py-3 transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring sm:flex-row sm:items-center sm:gap-4"
      >
        <div className="flex min-w-0 items-center gap-2 sm:shrink-0">
          <span className="shrink-0 text-sm font-medium tabular-nums">
            #{deployment.id}
          </span>
          <DeploymentStatusBadge
            deployment={deployment}
            className="h-5 px-1.5 py-0 text-[10px]"
          />
          <span
            className="max-w-32 shrink-0 truncate rounded-full bg-muted px-2 py-0.5 text-[10px] font-medium text-muted-foreground"
            title={deployment.environment.name}
          >
            {deployment.environment.name}
          </span>
          {deployment.is_current && (
            <span className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap text-[10px] font-medium text-emerald-600 dark:text-emerald-400">
              <CheckCircle2 className="size-3" aria-hidden="true" />
              Current
            </span>
          )}
        </div>

        <DeploymentSource
          deployment={deployment}
          projectSourceType={projectSourceType}
        />

        <div className="flex shrink-0 items-center gap-2 self-end sm:self-auto">
          <span className="whitespace-nowrap text-xs text-muted-foreground">
            <TimeAgo date={deployment.created_at} />
          </span>
          <ChevronRight
            className="size-4 text-muted-foreground/60 transition-transform group-hover:translate-x-0.5 group-hover:text-foreground"
            aria-hidden="true"
          />
        </div>
      </Link>
    </li>
  )
}

function DeploymentSource({
  deployment,
  projectSourceType,
}: {
  deployment: DeploymentResponse
  projectSourceType?: SourceType
}) {
  const source = deploymentSourceSummary(deployment, projectSourceType)

  if (source.kind === 'git') {
    return (
      <div className="flex min-w-0 flex-1 items-center gap-3 text-xs text-muted-foreground">
        {source.branch && (
          <span className="flex shrink-0 items-center gap-1">
            <GitBranch className="size-3" aria-hidden="true" />
            <span className="max-w-24 truncate">{source.branch}</span>
          </span>
        )}
        {source.commit && (
          <span className="flex shrink-0 items-center gap-1 font-mono">
            <GitCommit className="size-3" aria-hidden="true" />
            {source.commit.slice(0, 7)}
          </span>
        )}
        {source.message && (
          <span className="min-w-0 truncate">{source.message}</span>
        )}
      </div>
    )
  }

  const SourceIcon =
    source.kind === 'docker_image'
      ? Container
      : source.kind === 'manual'
        ? Package
        : FileArchive

  return (
    <div className="flex min-w-0 flex-1 items-center gap-1.5 text-xs text-muted-foreground">
      <SourceIcon className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="shrink-0 font-medium text-foreground/80">
        {source.label}
      </span>
      {source.detail && (
        <span className="min-w-0 truncate font-mono" title={source.detail}>
          {source.detail}
        </span>
      )}
    </div>
  )
}

function RecentDeploymentsSkeleton() {
  return (
    <section aria-label="Loading recent deployments">
      <div className="mb-3 space-y-1">
        <Skeleton className="h-4 w-32" />
        <Skeleton className="h-3 w-56" />
      </div>
      <Card className="divide-y overflow-hidden">
        {Array.from({ length: 3 }).map((_, index) => (
          <div key={index} className="flex items-center gap-4 px-4 py-3">
            <Skeleton className="h-5 w-48" />
            <Skeleton className="h-4 flex-1" />
            <Skeleton className="h-4 w-16" />
          </div>
        ))}
      </Card>
    </section>
  )
}
