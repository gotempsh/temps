// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ReloadableImage } from '@/components/utils/ReloadableImage'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
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
import { ExternalLink, GitFork, Rocket, Users } from 'lucide-react'
import BitbucketIcon from '@/icons/Bitbucket'
import GiteaIcon from '@/icons/Gitea'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import { Link, useNavigate } from 'react-router'

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
  const screenshotLocation = lastDeployment?.screenshot_location
  const repositoryUrl = repositoryCloneUrl
    ? repositoryWebUrl(repositoryCloneUrl)
    : null
  const repositoryProvider = repositoryCloneUrl
    ? gitProviderKind(repositoryProviderType, repositoryCloneUrl)
    : null

  const handleVisitorsClick = () => {
    if ((activeVisitorsCount?.active_visitors ?? 0) > 0) {
      navigate(`/projects/${project.slug}/analytics/live-visitors`)
    }
  }

  return (
    <header className="flex h-12 sm:h-16 shrink-0 items-center gap-2 border-b px-3 sm:px-4">
      <div className="flex flex-1 items-center justify-between gap-4 min-w-0">
        <div className="flex items-center gap-4">
          {screenshotLocation ? (
            <div className="size-8 shrink-0 overflow-hidden rounded-md border bg-muted/30">
              <ReloadableImage
                src={`/api/files${
                  screenshotLocation.startsWith('/')
                    ? screenshotLocation
                    : '/' + screenshotLocation
                }`}
                alt={`${project.slug} preview`}
                className="h-full w-full object-cover object-top"
              />
            </div>
          ) : (
            <Avatar className="size-8">
              <AvatarImage src={`/api/projects/${project.id}/favicon`} />
              <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
            </Avatar>
          )}
          <div className="flex items-center gap-2 min-w-0">
            <h1 className="text-base sm:text-lg font-semibold truncate">
              {project.slug}
            </h1>
            <Badge
              variant={project.last_deployment ? 'default' : 'outline'}
              className="hidden sm:inline-flex shrink-0"
            >
              {project.last_deployment ? 'Deployed' : 'Not deployed'}
            </Badge>
            <Link
              to={`/projects/${project.slug}/monitors`}
              title={healthIndicator.detail}
            >
              <Badge
                variant="outline"
                className="hidden sm:inline-flex shrink-0 gap-1.5"
              >
                <span
                  className={`inline-block h-2 w-2 rounded-full ${healthToneStyles[healthIndicator.tone]}`}
                />
                {healthIndicator.label}
                <span className="sr-only">. {healthIndicator.detail}</span>
              </Badge>
            </Link>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {activeVisitorsCount !== undefined && (
            <button
              onClick={handleVisitorsClick}
              disabled={(activeVisitorsCount?.active_visitors ?? 0) === 0}
              className={`flex items-center gap-1.5 px-2.5 py-1.5 bg-muted/30 rounded-full transition-colors ${
                (activeVisitorsCount?.active_visitors ?? 0) > 0
                  ? 'cursor-pointer hover:bg-muted/50 active:bg-muted/70'
                  : 'cursor-default'
              }`}
              title={
                (activeVisitorsCount?.active_visitors ?? 0) > 0
                  ? 'Click to view live visitors'
                  : 'No active visitors'
              }
            >
              <div
                className={`h-2 w-2 rounded-full ${activeVisitorsCount?.active_visitors > 0 ? 'bg-green-500 animate-pulse' : 'bg-gray-400'}`}
              />
              <span className="text-sm font-semibold flex items-center gap-1">
                {(activeVisitorsCount?.active_visitors ?? 0) > 0 && (
                  <Users className="h-3.5 w-3.5" />
                )}
                {activeVisitorsCount?.active_visitors}
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
          {lastDeploymentUrl && !isLoadingLastDeployment && (
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
          <Button size="sm" onClick={onDeploy}>
            <Rocket className="size-4" />
            <span className="hidden sm:inline">Deploy</span>
          </Button>
        </div>
      </div>
    </header>
  )
}
