// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ServiceGroupControl,
  useServiceGroup,
} from '@/components/project-groups/ServiceGroupControl'
import { ProjectServiceSwitcher } from '@/components/project-groups/ProjectServiceSwitcher'
import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import { listContainersOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { portlessWorkerHealth } from '@/components/containers/container-workload'
import { Status } from '@temps-sdk/ds'
import { useEffect, useRef } from 'react'
import {
  describeDockerSocket,
  HOST_DOCKER_ACCESS_SHORT_LABEL,
} from '@/lib/docker-socket'
import type { ProjectDeploymentStatus } from '@/lib/project-deployment-status'
import { isActiveDeploymentStatus } from '@/lib/recent-deployments'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { ExternalBadge } from '@/components/dashboard/ProjectCard'
import { Badge, badgeVariants } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
import { useProjectFailureState } from '@/hooks/useProjectFailureState'
import { useProjectsMonitorHealth } from '@/hooks/useProjectsMonitorHealth'
import {
  projectHealthIndicator,
  type ProjectHealthTone,
} from '@/components/dashboard/project-card-health'
import {
  gitProviderKind,
  repositoryWebUrl,
  type GitProviderKind,
} from '@/lib/project-header-actions'
import {
  AlertTriangle,
  ExternalLink,
  GitFork,
  Loader2,
  Plug,
  Rocket,
  Users,
} from 'lucide-react'
import BitbucketIcon from '@/icons/Bitbucket'
import GiteaIcon from '@/icons/Gitea'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import { Link, useNavigate } from 'react-router'
import { liveVisitorsPillLabel } from '@/components/analytics/analytics-onboarding'
import { ReachabilityStatus } from '@/components/reachability/ReachabilityStatus'

/** Badge variant for the problem states, which link to the overview. */
const deploymentStatusBadgeVariant: Record<
  Extract<ProjectDeploymentStatus, 'Failed' | 'Degraded'>,
  'destructive' | 'warning'
> = {
  Failed: 'destructive',
  Degraded: 'warning',
}

function isProblemStatus(
  status: ProjectDeploymentStatus | undefined
): status is 'Failed' | 'Degraded' {
  return status === 'Failed' || status === 'Degraded'
}

/**
 * Tones for the header health badge. Mirrors the projects-list card so the same
 * project reads the same in both places.
 */
const healthToneStyles: Record<ProjectHealthTone, string> = {
  healthy: 'bg-emerald-500',
  degraded: 'bg-amber-500',
  down: 'bg-red-500',
  idle: 'bg-zinc-300',
  unavailable: 'bg-zinc-400',
  pending: 'bg-zinc-300 animate-pulse',
}

interface ProjectDetailHeaderProps {
  project: ProjectResponse
  activeVisitorsCount?: { active_visitors: number }
  repositoryCloneUrl?: string | null
  repositoryProviderType?: string | null
  lastDeployment?: DeploymentResponse
  lastDeploymentUrl?: string | null
  /** Why there is no URL to visit: nobody can reach the environment yet. */
  lastDeploymentUnavailableReason?: string | null
  /** Environment served by lastDeploymentUrl, independently of healthEnvironment. */
  lastDeploymentEnvironmentId?: number
  isLoadingLastDeployment?: boolean
  onDeploy: () => void
}

function RepositoryProviderIcon({
  provider,
  className,
}: {
  provider: GitProviderKind | null
  className?: string
}) {
  if (provider === 'github') return <GithubIcon className={className} />
  if (provider === 'gitlab') return <GitlabIcon className={className} />
  if (provider === 'bitbucket') return <BitbucketIcon className={className} />
  if (provider === 'gitea') return <GiteaIcon className={className} />
  return <GitFork className={className} />
}

export function ProjectDetailHeader({
  project,
  activeVisitorsCount,
  repositoryCloneUrl,
  repositoryProviderType,
  lastDeployment,
  lastDeploymentUrl,
  lastDeploymentUnavailableReason,
  lastDeploymentEnvironmentId,
  isLoadingLastDeployment = false,
  onDeploy,
}: ProjectDetailHeaderProps) {
  const navigate = useNavigate()
  const healthQuery = useDashboardHealth([project.id])
  const monitorQuery = useProjectsMonitorHealth([project.id])
  // This badge links to Monitors, so it had better report what the monitors
  // say. Traffic health alone reports "unknown" for a project nobody visited
  // in the last hour — including one whose monitor is green — because the
  // proxy query excludes Temps' own checks (is_system_request = FALSE).
  const healthIndicator = projectHealthIndicator({
    health: healthQuery.data?.projects?.[String(project.id)],
    monitor: monitorQuery.data?.projects?.[String(project.id)],
    loading: healthQuery.isLoading,
    error: healthQuery.isError,
    windowHours: 1,
  })
  // Only the project *detail* responses carry this, and this header only ever
  // renders one of those — but `describeDockerSocket` still treats a missing
  // field as "unknown", so the badge stays hidden rather than claiming the
  // grant is absent.
  const dockerSocket = describeDockerSocket(project.docker_socket)
  const group = useServiceGroup(project.id).data?.group
  // An app hosted elsewhere is never deployed by Temps: no deploy button and
  // no "Not deployed", which would read as something to fix. `hosting` is the
  // explicit flag; `source_type: 'external'` marks a monitoring-only project.
  const isExternal =
    project.hosting === 'external' || project.source_type === 'external'
  // Latest build and currently deployed version can be different, including
  // during builds, after failures, and following a rollback.
  const {
    status: deploymentStatus,
    environmentsQuery,
    containersQuery,
    healthEnvironment,
  } = useProjectFailureState(project.id, lastDeployment, {
    enabled: !isExternal,
  })
  const supportsWorker =
    !isExternal &&
    project.project_type === 'server' &&
    project.preset !== 'docker-compose'
  const currentDeploymentId = healthEnvironment?.current_deployment_id
  const refetchContainers = containersQuery.refetch
  useEffect(() => {
    if (
      supportsWorker &&
      currentDeploymentId != null &&
      !healthEnvironment?.sleeping
    )
      void refetchContainers()
  }, [
    currentDeploymentId,
    refetchContainers,
    supportsWorker,
    healthEnvironment?.sleeping,
  ])
  const workerHealth = supportsWorker
    ? portlessWorkerHealth(containersQuery.data?.containers)
    : null
  const isWorker = workerHealth !== null
  // The site URL may belong to staging while project health follows production.
  // Reuse the health query only when both surfaces refer to the same environment.
  const siteEnvironmentId =
    lastDeploymentEnvironmentId ?? lastDeployment?.environment_id
  const siteEnvironment = environmentsQuery.data?.find(
    (environment) => environment.id === siteEnvironmentId
  )
  const siteUsesHealthEnvironment =
    siteEnvironmentId != null && siteEnvironmentId === healthEnvironment?.id
  const siteContainersQuery = useQuery({
    ...listContainersOptions({
      path: { project_id: project.id, environment_id: siteEnvironmentId ?? 0 },
    }),
    enabled:
      supportsWorker &&
      !!lastDeploymentUrl &&
      !siteUsesHealthEnvironment &&
      siteEnvironment?.current_deployment_id != null &&
      !siteEnvironment.sleeping,
    refetchInterval: 30_000,
    staleTime: 15_000,
    retry: false,
  })
  const siteCurrentDeploymentId = siteEnvironment?.current_deployment_id
  const refetchSiteContainers = siteContainersQuery.refetch
  useEffect(() => {
    if (
      supportsWorker &&
      lastDeploymentUrl &&
      !siteUsesHealthEnvironment &&
      siteCurrentDeploymentId != null &&
      !siteEnvironment?.sleeping
    )
      void refetchSiteContainers()
  }, [
    supportsWorker,
    lastDeploymentUrl,
    siteUsesHealthEnvironment,
    siteCurrentDeploymentId,
    siteEnvironment?.sleeping,
    refetchSiteContainers,
  ])
  const siteIsWorker =
    supportsWorker &&
    (siteUsesHealthEnvironment
      ? isWorker
      : portlessWorkerHealth(siteContainersQuery.data?.containers) !== null)
  // When a build finishes, its environment pointer is already set; refetch at
  // once so "Deploying" turns into "Deployed" instead of briefly reading
  // "Not deployed" until the next poll.
  const lastDeploymentStatus = lastDeployment?.status
  const previousDeploymentStatus = useRef(lastDeploymentStatus)
  const refetchEnvironments = environmentsQuery.refetch
  useEffect(() => {
    const previous = previousDeploymentStatus.current
    previousDeploymentStatus.current = lastDeploymentStatus
    if (
      previous !== undefined &&
      isActiveDeploymentStatus(previous) &&
      !(lastDeploymentStatus && isActiveDeploymentStatus(lastDeploymentStatus))
    ) {
      void refetchEnvironments()
    }
  }, [lastDeploymentStatus, refetchEnvironments])
  const repositoryUrl = repositoryCloneUrl
    ? repositoryWebUrl(repositoryCloneUrl)
    : null
  const repositoryProvider = repositoryCloneUrl
    ? gitProviderKind(repositoryProviderType, repositoryCloneUrl)
    : null

  // Always navigable: at zero, Live visitors explains why it is empty and
  // links to analytics setup when nothing has been installed yet.
  const handleVisitorsClick = () => {
    navigate(`/projects/${project.slug}/analytics/live-visitors`)
  }
  const visitorsLabel = liveVisitorsPillLabel(
    activeVisitorsCount?.active_visitors ?? 0
  )

  return (
    <header className="flex h-12 sm:h-16 shrink-0 items-center gap-2 border-b px-3 sm:px-4">
      <div className="flex flex-1 items-center justify-between gap-4 min-w-0">
        <div className="flex min-w-0 items-center gap-2 sm:gap-4">
          <ServiceIcon service={project} size="md" className="shrink-0" />
          <div className="flex items-center gap-2 min-w-0">
            <h1 className="text-base sm:text-lg font-semibold truncate">
              {group ? (
                <ProjectServiceSwitcher project={project} group={group}>
                  {project.slug}
                </ProjectServiceSwitcher>
              ) : (
                project.slug
              )}
            </h1>
            {isExternal ? (
              <ExternalBadge />
            ) : isProblemStatus(deploymentStatus) ? (
              // Failed / Degraded link to the overview banner that explains
              // the problem and offers the fix. Unlike the routine states it
              // stays visible on phones, where it shrinks to its icon so the
              // header's actions still fit.
              <Link
                to={`/projects/${project.slug}/project`}
                className={`${badgeVariants({ variant: deploymentStatusBadgeVariant[deploymentStatus] })} inline-flex shrink-0 gap-1`}
                title={
                  deploymentStatus === 'Failed'
                    ? 'The latest deployment failed. Open the overview for the reason and recovery actions.'
                    : 'Live containers are down or restarting. Open the overview for details and recovery actions.'
                }
              >
                <AlertTriangle aria-hidden="true" className="size-3" />
                <span className="sr-only sm:not-sr-only">
                  {deploymentStatus}
                </span>
              </Link>
            ) : (
              <Badge
                variant={
                  deploymentStatus === 'Deployed' ? 'default' : 'outline'
                }
                className="hidden sm:inline-flex shrink-0 gap-1"
              >
                {deploymentStatus === 'Deploying' && (
                  <Loader2 aria-hidden="true" className="size-3 animate-spin" />
                )}
                {deploymentStatus ??
                  (environmentsQuery.isError
                    ? 'Deployment status unavailable'
                    : 'Checking deployment…')}
              </Badge>
            )}
            {dockerSocket.state === 'granted' && (
              // Deliberately NOT hidden below `sm` like the badges around it:
              // this is the only place the console states that the project is
              // root-equivalent on its host, and a phone-width console that
              // showed nothing would be a silent omission of exactly the fact
              // an operator needs. It degrades to an icon plus a short label
              // instead of disappearing.
              <Badge
                role="img"
                variant="outline"
                className="inline-flex shrink-0 gap-1"
                title={dockerSocket.detail}
                aria-label={`${dockerSocket.label}: ${dockerSocket.detail}`}
              >
                <Plug aria-hidden="true" className="size-3" />
                <span className="sm:hidden">
                  {HOST_DOCKER_ACCESS_SHORT_LABEL}
                </span>
                <span className="hidden sm:inline">{dockerSocket.label}</span>
              </Badge>
            )}
            {isWorker ? (
              <span
                className="inline-flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground"
                title="Deploy-time health; runtime probing is not enabled"
              >
                <span className="max-sm:sr-only">Background worker</span>
                <Status
                  tone={
                    workerHealth === 'healthy'
                      ? 'ok'
                      : workerHealth === 'unhealthy'
                        ? 'error'
                        : 'idle'
                  }
                  label={`${workerHealth} at deploy`}
                />
              </span>
            ) : (
              <Link
                to={`/projects/${project.slug}/monitors`}
                title={`${healthIndicator.label}: ${healthIndicator.detail}`}
                aria-label={`${healthIndicator.label}: ${healthIndicator.detail}`}
                className="inline-flex size-6 shrink-0 items-center justify-center rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <span
                  aria-hidden="true"
                  className={`inline-block size-2 rounded-full ${healthToneStyles[healthIndicator.tone]}`}
                />
              </Link>
            )}
            <ServiceGroupControl project={project} />
          </div>
          {!isExternal && !isWorker && healthEnvironment && (
            // Per environment, per address (ADR 064 D3): what a visitor
            // can open, or why not and the button that fixes it.
            <ReachabilityStatus
              projectId={project.id}
              environmentId={healthEnvironment.id}
              environmentName={healthEnvironment.name}
            />
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {activeVisitorsCount !== undefined && !isWorker && (
            <button
              type="button"
              onClick={handleVisitorsClick}
              className="flex cursor-pointer items-center gap-1.5 rounded-full bg-muted/30 px-2.5 py-1.5 transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring active:bg-muted/70"
              title={visitorsLabel}
              aria-label={visitorsLabel}
            >
              <div
                className={`h-2 w-2 rounded-full ${activeVisitorsCount?.active_visitors > 0 ? 'bg-green-500 animate-pulse' : 'bg-gray-400'}`}
              />
              <span className="text-sm font-semibold flex items-center gap-1">
                {(activeVisitorsCount?.active_visitors ?? 0) > 0 && (
                  <Users className="h-3.5 w-3.5" />
                )}
                {activeVisitorsCount?.active_visitors ?? 0}
              </span>
            </button>
          )}
          {repositoryUrl && (
            <Button variant="outline" size="icon" className="size-9" asChild>
              <a
                href={repositoryUrl}
                target="_blank"
                rel="noopener noreferrer"
                aria-label="Open repository in a new window"
                title="Open repository"
              >
                <RepositoryProviderIcon
                  provider={repositoryProvider}
                  className="size-4"
                />
              </a>
            </Button>
          )}
          {lastDeploymentUrl && !isLoadingLastDeployment && !siteIsWorker && (
            <Button variant="outline" size="icon" className="size-9" asChild>
              <a
                href={lastDeploymentUrl}
                target="_blank"
                rel="noopener noreferrer"
                aria-label="Visit deployed site in a new window"
                title="Visit deployed site"
              >
                <ExternalLink className="size-4" />
              </a>
            </Button>
          )}
          {!lastDeploymentUrl &&
            lastDeploymentUnavailableReason &&
            !isLoadingLastDeployment &&
            !siteIsWorker && (
              // ADR 068 D4: nothing to open, so the button stays and says
              // why. The span carries the tooltip, which a disabled button
              // would not show.
              <span
                title={`Nothing to visit yet: ${lastDeploymentUnavailableReason}`}
              >
                <Button
                  variant="outline"
                  size="icon"
                  className="size-9"
                  disabled
                  aria-label={`Visit deployed site: unavailable, ${lastDeploymentUnavailableReason}`}
                >
                  <ExternalLink className="size-4" />
                </Button>
              </span>
            )}
          {!isExternal && (
            <Button size="sm" onClick={onDeploy}>
              <Rocket className="size-4" />
              <span className="max-sm:sr-only">Deploy</span>
            </Button>
          )}
        </div>
      </div>
    </header>
  )
}
