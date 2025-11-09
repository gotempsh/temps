import { ProjectResponse } from '@/api/client'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button, buttonVariants } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { Menu, Github, ExternalLink } from 'lucide-react'
import { Link } from 'react-router-dom'
import { useMobileSidebar } from './ProjectDetailSidebar'

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
  const { setIsOpen } = useMobileSidebar()

  return (
    <header className="flex h-16 shrink-0 items-center gap-2 border-b px-4">
      <Button
        variant="ghost"
        size="icon"
        className="md:hidden"
        onClick={() => setIsOpen(true)}
        aria-label="Open navigation menu"
      >
        <Menu className="h-5 w-5" />
      </Button>
      <div className="flex flex-1 items-center justify-between gap-4">
        <div className="flex items-center gap-4">
          <Avatar className="size-8">
            <AvatarImage src={`/api/projects/${project.id}/favicon`} />
            <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
          </Avatar>
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-lg font-semibold">{project.slug}</h1>
            <Badge
              variant={project.last_deployment ? 'default' : 'outline'}
            >
              {project.last_deployment ? 'Deployed' : 'Not deployed'}
            </Badge>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {activeVisitorsCount !== undefined && (
            <div className="flex items-center gap-1.5 px-2.5 py-1.5 bg-muted/30 rounded-full">
              <div
                className={`h-2 w-2 rounded-full ${activeVisitorsCount?.active_visitors > 0 ? 'bg-green-500 animate-pulse' : 'bg-gray-400'}`}
              />
              <span className="text-sm font-semibold">
                {activeVisitorsCount?.active_visitors}
              </span>
            </div>
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
                <Github className="h-4 w-4" />
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
