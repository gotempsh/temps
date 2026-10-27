// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentWithoutResolver } from '@/api/client'
import {
  clusterDnsStatusOptions,
  clusterDnsStatusQueryKey,
  getProjectDeploymentsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  hostsLabel,
  redeployWithoutResolver,
  staleBannerMessage,
  withoutResolverForProject,
} from '@/lib/cluster-dns-coverage'
import { CLUSTER_DNS_URL } from '@/lib/worker-nodes'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, CheckCircle2, Loader2, RefreshCw } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'

function useRedeployWithoutResolver() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (entry: DeploymentWithoutResolver) =>
      redeployWithoutResolver(entry),
    meta: { errorTitle: 'Failed to start the redeploy' },
    onSuccess: (skipped, entry) => {
      const name = `${entry.project_name} (${entry.environment_name})`
      if (skipped) toast.warning(`${name} was not redeployed: ${skipped}`)
      else toast.success(`Redeploying ${name}`)
      queryClient.invalidateQueries({ queryKey: clusterDnsStatusQueryKey() })
      queryClient.invalidateQueries({
        queryKey: getProjectDeploymentsQueryKey({
          path: { id: entry.project_id },
        }),
      })
    },
  })
}

/**
 * Applications listed by the server as running without the cluster
 * resolver, each with a redeploy action. Presentational so it can be tested
 * without a query client.
 */
export function ClusterDnsCoverageRows({
  entries,
  pendingDeploymentId,
  onRedeploy,
}: {
  entries: DeploymentWithoutResolver[]
  pendingDeploymentId?: number
  onRedeploy: (entry: DeploymentWithoutResolver) => void
}) {
  return (
    <ul className="divide-y rounded-lg border">
      {entries.map((entry) => (
        <li
          key={entry.deployment_id}
          className="flex flex-col gap-2 p-3 sm:flex-row sm:items-center sm:justify-between"
        >
          <div className="min-w-0">
            <Link
              to={`/projects/${entry.project_slug}`}
              className="text-sm font-medium hover:underline"
            >
              {entry.project_name}
            </Link>
            <span className="text-sm text-muted-foreground">
              {' '}
              · {entry.environment_name}
            </span>
            <p className="text-xs text-muted-foreground">
              Running on {hostsLabel(entry.hosts)}
            </p>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={pendingDeploymentId != null}
            onClick={() => onRedeploy(entry)}
          >
            {pendingDeploymentId === entry.deployment_id ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <RefreshCw className="size-3.5" />
            )}
            Redeploy
          </Button>
        </li>
      ))}
    </ul>
  )
}

/**
 * The Cluster DNS card's list of applications that still need a redeploy to
 * pick up the resolver. Shown only while Cluster DNS is on.
 */
export function ClusterDnsCoverageSection() {
  const status = useQuery({
    ...clusterDnsStatusOptions(),
    refetchInterval: 30_000,
  })
  const redeploy = useRedeployWithoutResolver()

  if (status.isLoading) return <Skeleton className="h-14 w-full" />
  if (status.isError) {
    return (
      <p className="text-xs text-muted-foreground">
        Could not check which applications use the resolver.{' '}
        <button
          type="button"
          className="underline"
          onClick={() => status.refetch()}
        >
          Retry
        </button>
      </p>
    )
  }
  const data = status.data
  if (!data?.cluster_dns_enabled) return null

  const entries = data.deployments_without_resolver ?? []
  return (
    <div id="cluster-dns-coverage" className="space-y-2">
      <h3 className="text-sm font-medium">Applications without the resolver</h3>
      {data.deployments_without_resolver_error && (
        <p className="text-xs text-destructive">
          {data.deployments_without_resolver_error}
        </p>
      )}
      {entries.length === 0 && !data.deployments_without_resolver_error ? (
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <CheckCircle2 className="size-3.5 text-emerald-600" />
          <span>
            Every running application can resolve <code>*.temps.local</code>{' '}
            names.
          </span>
        </p>
      ) : (
        <>
          <p className="text-xs text-muted-foreground max-w-prose">
            These were started before Cluster DNS was on, and a container keeps
            the nameservers it was created with. They cannot resolve{' '}
            <code>*.temps.local</code> names until redeployed.
          </p>
          <ClusterDnsCoverageRows
            entries={entries}
            pendingDeploymentId={
              redeploy.isPending ? redeploy.variables?.deployment_id : undefined
            }
            onRedeploy={(entry) => redeploy.mutate(entry)}
          />
        </>
      )}
    </div>
  )
}

/**
 * Shown on an application's page while one of its environments runs without
 * the cluster resolver. Renders nothing otherwise, including for roles that
 * cannot read the cluster DNS status (they cannot change it either).
 */
export function ClusterDnsStaleBanner({ projectId }: { projectId: number }) {
  const status = useQuery({ ...clusterDnsStatusOptions(), retry: false })
  const redeploy = useRedeployWithoutResolver()
  const entries = withoutResolverForProject(status.data, projectId)
  if (entries.length === 0) return null
  return (
    <ClusterDnsStaleAlert
      entries={entries}
      pendingDeploymentId={
        redeploy.isPending ? redeploy.variables?.deployment_id : undefined
      }
      onRedeploy={(entry) => redeploy.mutate(entry)}
    />
  )
}

export function ClusterDnsStaleAlert({
  entries,
  pendingDeploymentId,
  onRedeploy,
}: {
  entries: DeploymentWithoutResolver[]
  pendingDeploymentId?: number
  onRedeploy: (entry: DeploymentWithoutResolver) => void
}) {
  const environments = entries.map((entry) => entry.environment_name)
  return (
    <Alert className="mb-4 border-amber-500/30 bg-amber-500/5">
      <AlertTriangle className="h-4 w-4 text-amber-500" />
      <AlertTitle className="text-amber-700 dark:text-amber-400">
        Redeploy to reach services by name
      </AlertTitle>
      <AlertDescription className="space-y-2 text-amber-700 dark:text-amber-300">
        <p>{staleBannerMessage(environments)}</p>
        <div className="flex flex-wrap gap-2">
          {entries.map((entry) => (
            <Button
              key={entry.deployment_id}
              size="sm"
              disabled={pendingDeploymentId != null}
              onClick={() => onRedeploy(entry)}
            >
              {pendingDeploymentId === entry.deployment_id ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <RefreshCw className="size-3.5" />
              )}
              Redeploy {entry.environment_name}
            </Button>
          ))}
          <Button asChild size="sm" variant="outline">
            <Link to={CLUSTER_DNS_URL}>Cluster DNS settings</Link>
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  )
}
