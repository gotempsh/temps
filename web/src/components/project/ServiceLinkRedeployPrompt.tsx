// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getProjectDeploymentsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  environmentsToRedeploy,
  redeployCurrentDeployments,
  redeploySummary,
  serviceLinkRedeployMessage,
  type ServiceLinkChange,
} from '@/lib/service-link-redeploy'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Info, Loader2, RefreshCw, X } from 'lucide-react'
import { Link } from 'react-router'
import { useEffect, useRef } from 'react'
import { toast } from 'sonner'

/**
 * Shown after a managed service is linked to or unlinked from a project:
 * the running app keeps its old environment variables until it is
 * redeployed. Offers a one-click redeploy of each environment's current
 * deployment, and stays out of the way when nothing is deployed yet (the
 * first deployment picks the variables up on its own).
 */
export function ServiceLinkRedeployPrompt({
  project,
  change,
  onDismiss,
}: {
  project: Pick<ProjectResponse, 'id' | 'slug' | 'source_type'>
  change: ServiceLinkChange | null
  onDismiss: () => void
}) {
  const queryClient = useQueryClient()
  const completed = useRef(new Set<number>())
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: change != null,
  })
  const redeploy = useMutation({
    mutationFn: () =>
      redeployCurrentDeployments(
        project.id,
        project.source_type,
        environments.data ?? [],
        undefined,
        completed.current
      ),
    meta: { errorTitle: 'Failed to start the redeploy' },
    onSuccess: (outcomes) => {
      const summary = redeploySummary(outcomes)
      if (summary.started > 0) toast.success(summary.message)
      else toast.warning(summary.message)
      queryClient.invalidateQueries({
        queryKey: getProjectDeploymentsQueryKey({ path: { id: project.id } }),
      })
      if (!outcomes.some((o) => o.failed)) onDismiss()
    },
  })

  const resetRedeploy = redeploy.reset
  useEffect(() => {
    completed.current.clear()
    resetRedeploy()
  }, [change, project.id, resetRedeploy])

  const targets = environmentsToRedeploy(environments.data)
  if (!change || (environments.isSuccess && targets.length === 0)) return null

  const { title, description } = serviceLinkRedeployMessage(change, targets)

  return (
    <Alert role="status">
      <Info className="size-4" />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>
        <p>{description}</p>
        {environments.isError && (
          <p>
            Could not load environments. Running apps keep their old connection
            variables until redeployed.
          </p>
        )}
        {environments.isError && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => environments.refetch()}
          >
            Retry loading environments
          </Button>
        )}
        {redeploy.data
          ?.filter((o) => o.failed)
          .map((o) => (
            <p key={o.environment}>
              {o.environment}: {o.failed}
            </p>
          ))}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button
            size="sm"
            onClick={() => redeploy.mutate()}
            disabled={redeploy.isPending || !environments.isSuccess}
          >
            {redeploy.isPending ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <RefreshCw className="size-3.5" />
            )}
            Redeploy now
          </Button>
          <Button size="sm" variant="outline" asChild>
            <Link to={`/projects/${project.slug}/deployments`}>
              View deployments
            </Link>
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={onDismiss}
            disabled={redeploy.isPending}
          >
            <X className="size-3.5" />
            Later
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  )
}
