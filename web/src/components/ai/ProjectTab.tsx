// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getLastDeploymentOptions,
  getProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, ExternalLink, GitBranch } from 'lucide-react'
import { Link } from 'react-router'
import {
  isActiveDeploymentStatus,
  isReadyDeploymentStatus,
} from './deployment-card-state'
import { statusPresentation } from './GeneratedDeploymentCard'

/** A project opened from the chat: what it is and how its latest deployment is doing. */
export function ProjectTab({ projectId }: { projectId: number }) {
  const project = useQuery(getProjectOptions({ path: { id: projectId } }))
  const latest = useQuery({
    ...getLastDeploymentOptions({ path: { id: projectId } }),
    // A project that never deployed answers `deployment: null`.
    select: (data) => data.deployment ?? null,
    retry: false,
    refetchInterval: (query) => {
      const deployment = query.state.data?.deployment
      return deployment && isActiveDeploymentStatus(deployment.status)
        ? 3000
        : false
    },
  })

  if (project.isPending) {
    return (
      <div className="space-y-3 p-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-4 w-56" />
        <Skeleton className="h-20 w-full" />
      </div>
    )
  }
  if (project.isError || !project.data) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Temps could not load project {projectId}. It may have been removed, or
        you may not have access to it.
      </p>
    )
  }

  const data = project.data
  const deployment = latest.data
  const status = deployment ? statusPresentation(deployment.status) : null
  const live =
    deployment && isReadyDeploymentStatus(deployment.status) && deployment.url
  return (
    <div className="flex min-h-0 flex-col">
      <div className="flex items-center gap-3 px-4 py-3">
        <ServiceIcon service={data} size="lg" />
        <div className="min-w-0">
          <h3 className="truncate text-sm font-semibold">{data.name}</h3>
          <p className="flex items-center gap-1 text-xs text-muted-foreground">
            <span className="truncate font-mono">{data.slug}</span>
            <CopyButton value={data.slug} className="size-5 shrink-0" />
          </p>
        </div>
      </div>
      <p className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 pb-3 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1 font-mono">
          <GitBranch className="size-3" aria-hidden="true" />
          {data.main_branch}
        </span>
        {data.repo_owner && data.repo_name && (
          <span className="truncate">
            {data.repo_owner}/{data.repo_name}
          </span>
        )}
        {data.preset && <span>{data.preset}</span>}
      </p>

      <div className="space-y-2 border-t px-4 py-3">
        <div className="flex items-center justify-between text-xs">
          <span className="font-medium">Latest deployment</span>
          {status && (
            <span className={cn('font-medium', status.className)}>
              {status.label}
            </span>
          )}
        </div>
        {latest.isPending ? (
          <Skeleton className="h-10 w-full" />
        ) : deployment ? (
          <div className="space-y-1.5 text-xs text-muted-foreground">
            <p>
              #{deployment.id} · {deployment.environment.name} ·{' '}
              <TimeAgo date={deployment.started_at ?? deployment.created_at} />
            </p>
            {deployment.url &&
              (live ? (
                <a
                  href={deployment.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex min-w-0 items-start gap-1 text-sm font-medium text-primary underline-offset-2 hover:underline"
                >
                  <span className="break-all">{deployment.url}</span>
                  <ExternalLink className="mt-0.5 size-3.5 shrink-0" />
                </a>
              ) : (
                <span className="break-all text-sm">{deployment.url}</span>
              ))}
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            This project has not been deployed yet.
          </p>
        )}
      </div>

      <div className="flex flex-wrap gap-2 border-t px-4 py-3">
        <Button asChild size="sm" variant="outline" className="h-8">
          <Link to={`/projects/${data.slug}`}>
            Open project
            <ArrowRight aria-hidden="true" />
          </Link>
        </Button>
        {deployment && (
          <Button asChild size="sm" variant="ghost" className="h-8">
            <Link to={`/projects/${data.slug}/deployments/${deployment.id}`}>
              Logs and details
            </Link>
          </Button>
        )}
      </div>
    </div>
  )
}
