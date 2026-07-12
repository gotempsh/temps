import { ProjectResponse } from '@/api/client'
import { getLastDeploymentOptions } from '@/api/client/@tanstack/react-query.gen'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { buttonVariants } from '@/components/ui/button'
import { ReloadableImage } from '@/components/utils/ReloadableImage'
import { cn } from '@/lib/utils'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
import { useQuery } from '@tanstack/react-query'
import { ExternalLink, Users } from 'lucide-react'
import GithubIcon from '@/icons/Github'
import { Link, useNavigate } from 'react-router-dom'

const healthDotColors: Record<string, string> = {
  operational: 'bg-emerald-500',
  degraded: 'bg-amber-500',
  down: 'bg-red-500',
}

const healthLabels: Record<string, string> = {
  operational: 'Operational',
  degraded: 'Degraded',
  down: 'Down',
}

interface ProjectDetailHeaderProps {
  project: ProjectResponse
  activeVisitorsCount?: { active_visitors: number }
  repositoryCloneUrl?: string | null
  lastDeploymentUrl?: string | null
  isLoadingLastDeployment?: boolean
}

export function ProjectDetailHeader({
  project,
  activeVisitorsCount,
  repositoryCloneUrl,
  lastDeploymentUrl,
  isLoadingLastDeployment = false,
}: ProjectDetailHeaderProps) {
  const navigate = useNavigate()
  const healthQuery = useDashboardHealth([project.id])
  const health = healthQuery.data?.projects?.[String(project.id)]
  const { data: lastDeployment } = useQuery({
    ...getLastDeploymentOptions({ path: { id: project.id } }),
    enabled: !!project.id,
    refetchOnWindowFocus: true,
  })
  const screenshotLocation = lastDeployment?.screenshot_location

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
            <h1 className="text-base sm:text-lg font-semibold truncate">{project.slug}</h1>
            <Badge variant={project.last_deployment ? 'default' : 'outline'} className="hidden sm:inline-flex shrink-0">
              {project.last_deployment ? 'Deployed' : 'Not deployed'}
            </Badge>
            {health && health.status !== 'no_monitors' && (
              <Link to={`/projects/${project.slug}/monitors`}>
                <Badge variant="outline" className="hidden sm:inline-flex shrink-0 gap-1.5">
                  <span className={`inline-block h-2 w-2 rounded-full ${healthDotColors[health.status] || 'bg-zinc-400'}`} />
                  {healthLabels[health.status] || health.status}
                </Badge>
              </Link>
            )}
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
          {/* Mobile: Icon-only buttons */}
          <div className="md:hidden flex items-center gap-1">
            {repositoryCloneUrl && (
              <Link
                to={repositoryCloneUrl.replace('.git', '')}
                target="_blank"
                rel="noopener noreferrer"
                className="p-2 hover:bg-accent rounded-md transition-colors"
                title="View repository"
              >
                <GithubIcon className="h-4 w-4" />
              </Link>
            )}
            {lastDeploymentUrl && !isLoadingLastDeployment && (
              <Link
                to={lastDeploymentUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="p-2 hover:bg-accent rounded-md transition-colors"
                title="Visit deployed site"
              >
                <ExternalLink className="h-4 w-4" />
              </Link>
            )}
          </div>
          {/* Desktop: Full buttons */}
          <div className="hidden md:flex items-center gap-2">
            {repositoryCloneUrl && (
              <Link
                to={repositoryCloneUrl.replace('.git', '')}
                target="_blank"
                rel="noopener noreferrer"
                className={cn(
                  buttonVariants({
                    variant: 'outline',
                    size: 'sm',
                  })
                )}
              >
                Repository
              </Link>
            )}
            {lastDeploymentUrl && !isLoadingLastDeployment && (
              <Link
                to={lastDeploymentUrl}
                target="_blank"
                rel="noopener noreferrer"
                className={cn(
                  buttonVariants({
                    size: 'sm',
                  })
                )}
              >
                Visit
              </Link>
            )}
          </div>
        </div>
      </div>
    </header>
  )
}
