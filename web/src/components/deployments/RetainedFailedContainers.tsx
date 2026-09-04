// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listContainerHistoryOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ContainerLogs } from '@/components/containers/ContainerLogs'
import { Skeleton } from '@/components/ui/skeleton'
import {
  currentRetainedContainers,
  retainedContainerLogsPath,
  toggleRetainedContainerLogs,
} from '@/lib/retained-failed-containers'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ChevronDown,
  ChevronUp,
  ExternalLink,
  RefreshCw,
  ScrollText,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'

interface RetainedFailedContainersProps {
  projectId: number
  projectSlug: string
  environmentId: number
  deploymentId: number
  deploymentStatus: string | null | undefined
}

export function RetainedFailedContainersLoadError({
  onRetry,
}: {
  onRetry: () => void
}) {
  return (
    <Card className="border-destructive/40">
      <CardContent className="flex flex-col gap-3 p-6 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="text-sm font-semibold">
            Could not load retained deployment containers
          </h3>
          <p className="mt-1 text-xs text-muted-foreground">
            The deployment failed, but its diagnostic container status could not
            be retrieved.
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={onRetry}>
          <RefreshCw className="mr-2 h-4 w-4" />
          Retry
        </Button>
      </CardContent>
    </Card>
  )
}

/**
 * Failed deployment candidates stay alive until the next deploy/delete so their
 * runtime logs remain inspectable. They are deliberately never routed: only
 * successful finalization promotes deployment routes.
 */
export function RetainedFailedContainers({
  projectId,
  projectSlug,
  environmentId,
  deploymentId,
  deploymentStatus,
}: RetainedFailedContainersProps) {
  const [expandedContainerId, setExpandedContainerId] = useState<string | null>(
    null
  )
  const isFailed = deploymentStatus === 'failed'
  const { data, isPending, isError, refetch } = useQuery({
    ...listContainerHistoryOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
      },
      query: {
        deployment_id: deploymentId,
        limit: 20,
      },
    }),
    enabled: isFailed && environmentId > 0,
  })

  if (!isFailed) return null
  if (isError) {
    return <RetainedFailedContainersLoadError onRetry={() => void refetch()} />
  }
  if (isPending) {
    return (
      <Card>
        <CardContent className="space-y-3 p-6">
          <Skeleton className="h-5 w-56" />
          <Skeleton className="h-16 w-full" />
        </CardContent>
      </Card>
    )
  }

  const retained = currentRetainedContainers(data?.containers)
  if (retained.length === 0) return null

  return (
    <Card className="border-amber-500/40 bg-amber-500/5">
      <CardContent className="p-6">
        <div className="mb-2 flex items-center gap-2">
          <AlertTriangle className="h-4 w-4 text-amber-600 dark:text-amber-400" />
          <h3 className="text-sm font-semibold">
            Failed deployment containers retained for debugging
          </h3>
          <Badge variant="outline" className="ml-1">
            {retained.length}
          </Badge>
        </div>
        <p className="mb-4 text-xs text-muted-foreground">
          These containers are not receiving public traffic. Inspect their live
          logs before redeploying; the next successful deployment or project
          deletion removes them automatically.
        </p>

        <div className="flex flex-col gap-2">
          {retained.map((container) => {
            const isExpanded = expandedContainerId === container.container_id
            const panelId = `retained-container-logs-${container.id}`

            return (
              <div
                key={container.id}
                className="overflow-hidden rounded-md border border-border/60 bg-background/70"
              >
                <div className="flex flex-col gap-3 px-3 py-3 sm:flex-row sm:items-center">
                  <ScrollText className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-mono text-sm">
                      {container.container_name}
                    </div>
                    {container.service_name && (
                      <div className="text-xs text-muted-foreground">
                        Service: {container.service_name}
                      </div>
                    )}
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      aria-expanded={isExpanded}
                      aria-controls={panelId}
                      onClick={() =>
                        setExpandedContainerId((current) =>
                          toggleRetainedContainerLogs(
                            current,
                            container.container_id
                          )
                        )
                      }
                    >
                      {isExpanded ? 'Hide logs' : 'Show logs'}
                      {isExpanded ? (
                        <ChevronUp className="ml-2 h-3.5 w-3.5" />
                      ) : (
                        <ChevronDown className="ml-2 h-3.5 w-3.5" />
                      )}
                    </Button>
                    <Button variant="ghost" size="icon" asChild>
                      <Link
                        to={retainedContainerLogsPath(
                          projectSlug,
                          environmentId,
                          deploymentId,
                          container.container_id
                        )}
                        aria-label={`Open full logs for ${container.container_name}`}
                        title="Open full log view"
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
                      </Link>
                    </Button>
                  </div>
                </div>

                {isExpanded && (
                  <div
                    id={panelId}
                    role="region"
                    aria-label={`Live logs for ${container.container_name}`}
                    className="h-[24rem] border-t bg-background"
                  >
                    <ContainerLogs
                      projectId={projectId.toString()}
                      environmentId={environmentId.toString()}
                      containerId={container.container_id}
                      serviceName={container.service_name}
                    />
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </CardContent>
    </Card>
  )
}
