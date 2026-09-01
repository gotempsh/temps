// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listContainerHistoryOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  currentRetainedContainers,
  retainedContainerLogsPath,
} from '@/lib/retained-compose-containers'
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, ExternalLink, ScrollText } from 'lucide-react'
import { Link } from 'react-router'

interface RetainedComposeContainersProps {
  projectId: number
  projectSlug: string
  environmentId: number
  deploymentId: number
  deploymentStatus: string | null | undefined
}

/**
 * Failed Compose candidates stay alive until the next deploy/delete so their
 * runtime logs remain inspectable. They are deliberately never routed: only
 * successful finalization promotes deployment routes.
 */
export function RetainedComposeContainers({
  projectId,
  projectSlug,
  environmentId,
  deploymentId,
  deploymentStatus,
}: RetainedComposeContainersProps) {
  const isFailed = deploymentStatus === 'failed'
  const { data, isPending, isError } = useQuery({
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

  if (!isFailed || isError) return null
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
            Failed Compose containers retained for debugging
          </h3>
          <Badge variant="outline" className="ml-1">
            {retained.length}
          </Badge>
        </div>
        <p className="mb-4 text-xs text-muted-foreground">
          These containers are not receiving public traffic. Inspect their live
          logs before redeploying; the next deployment or project deletion
          removes them automatically.
        </p>

        <div className="flex flex-col gap-2">
          {retained.map((container) => (
            <div
              key={container.id}
              className="flex flex-col gap-3 rounded-md border border-border/60 bg-background/70 px-3 py-3 sm:flex-row sm:items-center"
            >
              <ScrollText className="h-4 w-4 shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <div className="truncate font-mono text-sm">
                  {container.container_name}
                </div>
                {container.service_name && (
                  <div className="text-xs text-muted-foreground">
                    Compose service: {container.service_name}
                  </div>
                )}
              </div>
              <Button variant="outline" size="sm" asChild>
                <Link
                  to={retainedContainerLogsPath(
                    projectSlug,
                    environmentId,
                    deploymentId,
                    container.container_id
                  )}
                >
                  View logs
                  <ExternalLink className="ml-2 h-3.5 w-3.5" />
                </Link>
              </Button>
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}
