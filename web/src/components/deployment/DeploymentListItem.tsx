import { DeploymentResponse } from '@/api/client'
import { getDeploymentOptions } from '@/api/client/@tanstack/react-query.gen'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useQuery } from '@tanstack/react-query'
import { GitBranch, GitCommit, MoreHorizontal, X } from 'lucide-react'
import { useCallback, useEffect, useMemo } from 'react'
import { Link } from 'react-router-dom'
import { TimeAgo } from '../utils/TimeAgo'
import { DeploymentStatusBadge } from './DeploymentStatusBadge'

interface DeploymentListItemProps {
  deployment: DeploymentResponse
  onViewDetails?: () => void
  onRedeploy?: () => void
  onCancel?: () => void
  onCopyUrl?: () => void
  onDeploymentUpdate?: (updatedDeployment: DeploymentResponse) => void
}

export default function DeploymentListItem({
  deployment: initialDeployment,
  onViewDetails,
  onRedeploy,
  onCancel,
  onCopyUrl,
  onDeploymentUpdate,
}: DeploymentListItemProps) {
  const { refetch, data: refreshedDeployment } = useQuery({
    ...getDeploymentOptions({
      path: {
        deployment_id: initialDeployment.id,
        project_id: initialDeployment.project_id,
      },
    }),
    enabled:
      initialDeployment.status !== 'completed' &&
      initialDeployment.status !== 'failed' &&
      initialDeployment.status !== 'stopped' &&
      initialDeployment.status !== 'cancelled',
  })
  const deployment = useMemo(() => {
    if (refreshedDeployment) {
      return refreshedDeployment
    }
    return initialDeployment
  }, [refreshedDeployment, initialDeployment])

  const pollDeployment = useCallback(async () => {
    const { data } = await refetch()
    if (data && onDeploymentUpdate) {
      onDeploymentUpdate(data)
    }
  }, [refetch, onDeploymentUpdate])

  useEffect(() => {
    let intervalId: ReturnType<typeof setInterval> | undefined

    if (
      deployment.status !== 'completed' &&
      deployment.status !== 'failed' &&
      deployment.status !== 'stopped' &&
      deployment.status !== 'cancelled'
    ) {
      intervalId = setInterval(pollDeployment, 2000)
    }

    return () => {
      if (intervalId) {
        clearInterval(intervalId)
      }
    }
  }, [deployment.status, pollDeployment])

  return (
    <li className="flex flex-col sm:flex-row items-start sm:items-center justify-between p-4 gap-4 sm:gap-2">
      <div className="grid gap-1 w-full sm:w-auto">
        <div className="flex flex-wrap items-center gap-2">
          <Link to={`${deployment.id}`} className="font-medium hover:underline">
            #{deployment.id}
          </Link>
          <Badge variant="secondary">{deployment.environment.name}</Badge>
          <DeploymentStatusBadge deployment={deployment} />
        </div>
        <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          <div className="flex items-center gap-2">
            <GitBranch className="h-4 w-4" />
            <span>{deployment.branch}</span>
          </div>
          <div className="flex items-center gap-2">
            <GitCommit className="h-4 w-4" />
            <span className="font-mono">
              {deployment.commit_hash?.slice(0, 8)}
            </span>
          </div>
          <span className="text-muted-foreground break-all">
            {deployment.commit_message}
          </span>
        </div>
      </div>
      <div className="flex flex-col sm:flex-row items-start sm:items-center gap-4 w-full sm:w-auto">
        <div className="flex items-center gap-2 w-full sm:w-auto">
          {deployment.commit_author && (
            <>
              <Avatar className="h-6 w-6 shrink-0">
                <AvatarImage
                  src={deployment.commit_author || '/placeholder.svg'}
                  alt={deployment.commit_author!}
                />
                <AvatarFallback>
                  {deployment.commit_author?.slice(0, 1).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <span className="text-sm text-muted-foreground truncate">
                {deployment.commit_author}
              </span>
            </>
          )}
          <span className="text-sm text-muted-foreground">•</span>
          <span className="text-sm text-muted-foreground whitespace-nowrap">
            <TimeAgo date={deployment.created_at} />
          </span>
        </div>
        <div className="hidden sm:block h-8 w-px bg-border mx-2"></div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" className="ml-auto sm:ml-0">
              <MoreHorizontal className="h-4 w-4" />
              <span className="sr-only">Open menu</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onClick={onViewDetails}>
              View details
            </DropdownMenuItem>
            {(deployment.status === 'running' ||
              deployment.status === 'pending') && (
              <DropdownMenuItem
                onClick={(e) => {
                  e.preventDefault()
                  onCancel?.()
                }}
              >
                <X className="mr-2 h-4 w-4" />
                Cancel
              </DropdownMenuItem>
            )}
            <DropdownMenuItem
              onClick={(e) => {
                e.preventDefault()
                onRedeploy?.()
              }}
            >
              Redeploy
            </DropdownMenuItem>
            <DropdownMenuItem
              onClick={(e) => {
                e.preventDefault()
                onCopyUrl?.()
              }}
            >
              Copy URL
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </li>
  )
}
