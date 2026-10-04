// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { Button, Callout } from '@temps-sdk/ds'
import { Button as LinkButton } from '@/components/ui/button'
import { useNodeCapability } from '@/hooks/useNodeCapability'
import { problemDetail } from '@/lib/api-problem'
import { SAMPLE_APP, firstDeployTrackingPath } from '@/lib/first-deploy'
import {
  WORKER_NODES_URL,
  WORKER_NODE_ASK_ADMIN_MESSAGE,
  canAddWorkerNode,
  shouldShowWorkerNodeBanner,
} from '@/lib/worker-nodes'
import { cn } from '@/lib/utils'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Network, Rocket } from 'lucide-react'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { createAndDeploySample } from './sample-deploy-api'

/** Router state handed to the tracking page when the deploy did not start. */
export interface SampleDeployNavigationState {
  startError?: string
}

/**
 * One-click sample deployment: creates a small project from a public image,
 * deploys it to production and opens the guide that waits for the live URL.
 *
 * The fastest way for a new install to prove Docker, image pulls and routing
 * work before the operator brings their own code.
 */
export function SampleDeployCard({ className }: { className?: string }) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const capability = useNodeCapability()
  const cannotSchedule = shouldShowWorkerNodeBanner(capability.data)

  const deploy = useMutation({
    mutationFn: createAndDeploySample,
    meta: { errorTitle: 'Could not create the sample app' },
    onSuccess: async (result) => {
      await queryClient.invalidateQueries({ queryKey: getProjectsQueryKey() })
      const path = firstDeployTrackingPath(result.projectSlug)
      if (result.status === 'started') {
        toast.success(`Deploying the sample app to ${result.environmentName}`)
        navigate(path)
        return
      }
      const startError =
        result.status === 'deploy_failed'
          ? problemDetail(result.error, 'The deployment did not start.')
          : 'The project has no environment to deploy to.'
      toast.error(
        `Sample project created, but it did not deploy: ${startError}`
      )
      navigate(path, {
        state: { startError } satisfies SampleDeployNavigationState,
      })
    },
  })

  return (
    <section
      aria-labelledby="sample-deploy-title"
      className={cn(
        'rounded-lg border bg-card p-5 text-card-foreground',
        className
      )}
    >
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 items-start gap-3">
          <div className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-muted/50 text-primary">
            <Rocket className="size-4" aria-hidden />
          </div>
          <div className="min-w-0">
            <h2 id="sample-deploy-title" className="text-base font-semibold">
              Deploy a sample app in one click
            </h2>
            <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
              No code needed. Temps creates a project named{' '}
              <span className="font-medium text-foreground">
                {SAMPLE_APP.baseName}
              </span>
              , runs the public{' '}
              <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                {SAMPLE_APP.image}
              </code>{' '}
              image in production and gives you its live URL — usually within a
              minute. Delete it whenever you like.
            </p>
          </div>
        </div>
        {cannotSchedule && !canAddWorkerNode(capability.data) ? (
          <p className="text-sm text-muted-foreground">
            {WORKER_NODE_ASK_ADMIN_MESSAGE}
          </p>
        ) : cannotSchedule ? (
          <LinkButton asChild variant="outline" className="shrink-0">
            <Link to={capability.data?.setup_path ?? WORKER_NODES_URL}>
              <Network className="size-4" aria-hidden />
              Add a worker node first
            </Link>
          </LinkButton>
        ) : (
          <Button
            className="shrink-0"
            busy={deploy.isPending}
            busyLabel="Starting deployment…"
            onClick={() => deploy.mutate()}
          >
            <Rocket className="size-4" aria-hidden />
            Deploy sample app
          </Button>
        )}
      </div>
      {deploy.isError && (
        <Callout
          tone="error"
          title="The sample project could not be created"
          className="mt-4"
        >
          {problemDetail(deploy.error, 'Unknown error. Try again.')}
        </Callout>
      )}
    </section>
  )
}
