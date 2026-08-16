import { DeploymentResponse, type SourceType } from '@/api/client'
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
import {
  ArrowUpRight,
  CheckCircle2,
  Container,
  FileArchive,
  GitBranch,
  GitCommit,
  MoreHorizontal,
  Package,
  RefreshCw,
  RotateCcw,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useMemo } from 'react'
import { deploymentSourceSummary } from '@/lib/deployment-source-summary'
import { TimeAgo } from '../utils/TimeAgo'
import { DeploymentStatusBadge } from './DeploymentStatusBadge'

interface DeploymentCompactRowProps {
  deployment: DeploymentResponse
  onRedeploy?: () => void
  onCancel?: () => void
  onRollback?: () => void
  onPromote?: () => void
  onDeploymentUpdate?: (updatedDeployment: DeploymentResponse) => void
  projectSourceType?: SourceType
}

export default function DeploymentCompactRow({
  deployment: initialDeployment,
  onRedeploy,
  onCancel,
  onRollback,
  onPromote,
  onDeploymentUpdate,
  projectSourceType,
}: DeploymentCompactRowProps) {
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

  const deployment = useMemo(
    () => refreshedDeployment ?? initialDeployment,
    [refreshedDeployment, initialDeployment]
  )
  const source = deploymentSourceSummary(deployment, projectSourceType)

  const pollDeployment = useCallback(async () => {
    const { data } = await refetch()
    if (data && onDeploymentUpdate) onDeploymentUpdate(data)
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
      if (intervalId) clearInterval(intervalId)
    }
  }, [deployment.status, pollDeployment])

  return (
    <li className="flex flex-col gap-2 px-3 py-2.5 sm:flex-row sm:items-center sm:gap-3">
      {/* Primary line: id + status + env + current */}
      <div className="flex min-w-0 items-center gap-2 sm:shrink-0">
        <span className="font-medium text-sm">#{deployment.id}</span>
        <DeploymentStatusBadge
          deployment={deployment}
          className="text-[10px] px-1.5 py-0 h-5"
        />
        <Badge variant="secondary" className="text-[10px] px-1.5 py-0 h-5">
          {deployment.environment.name}
        </Badge>
        {deployment.is_current && (
          <Badge className="bg-green-600 hover:bg-green-700 flex items-center gap-0.5 text-[10px] px-1.5 py-0 h-5">
            <CheckCircle2 className="h-2.5 w-2.5" />
            Current
          </Badge>
        )}
      </div>

      {/* Meta line: source info — takes remaining space, truncates */}
      <div className="flex min-w-0 flex-1 items-center gap-3 text-xs text-muted-foreground">
        {source.kind === 'git' ? (
          <>
            {source.branch && (
              <div className="flex shrink-0 items-center gap-1">
                <GitBranch className="h-3 w-3" />
                <span className="max-w-[100px] truncate">{source.branch}</span>
              </div>
            )}
            {source.commit && (
              <div className="flex shrink-0 items-center gap-1">
                <GitCommit className="h-3 w-3" />
                <span className="font-mono">{source.commit.slice(0, 7)}</span>
              </div>
            )}
            {source.message && (
              <span className="min-w-0 truncate">{source.message}</span>
            )}
          </>
        ) : (
          <div className="flex min-w-0 items-center gap-1.5">
            {source.kind === 'docker_image' ? (
              <Container className="h-3.5 w-3.5 shrink-0" />
            ) : source.kind === 'manual' ? (
              <Package className="h-3.5 w-3.5 shrink-0" />
            ) : (
              <FileArchive className="h-3.5 w-3.5 shrink-0" />
            )}
            <span className="shrink-0 font-medium text-foreground/80">
              {source.label}
            </span>
            {source.detail && (
              <span
                className="min-w-0 truncate font-mono"
                title={source.detail}
              >
                {source.detail}
              </span>
            )}
          </div>
        )}
      </div>

      {/* Right cluster: author + time + menu */}
      <div className="flex items-center gap-2 sm:shrink-0">
        {deployment.commit_author && (
          <Avatar className="h-5 w-5 shrink-0">
            <AvatarImage
              src={deployment.commit_author || '/placeholder.svg'}
              alt={deployment.commit_author!}
            />
            <AvatarFallback className="text-[9px]">
              {deployment.commit_author?.slice(0, 1).toUpperCase()}
            </AvatarFallback>
          </Avatar>
        )}
        <span className="text-xs text-muted-foreground whitespace-nowrap">
          <TimeAgo date={deployment.created_at} />
        </span>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7 ml-auto sm:ml-0"
              onClick={(e) => {
                e.preventDefault()
                e.stopPropagation()
              }}
            >
              <MoreHorizontal className="h-3.5 w-3.5" />
              <span className="sr-only">Open menu</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
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
              <RefreshCw className="mr-2 h-4 w-4" />
              Redeploy
            </DropdownMenuItem>
            {(deployment.status === 'superseded' ||
              deployment.status === 'completed') && (
              <>
                <DropdownMenuItem
                  onClick={(e) => {
                    e.preventDefault()
                    onRollback?.()
                  }}
                >
                  <RotateCcw className="mr-2 h-4 w-4" />
                  Rollback to this
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={(e) => {
                    e.preventDefault()
                    onPromote?.()
                  }}
                >
                  <ArrowUpRight className="mr-2 h-4 w-4" />
                  Promote to...
                </DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </li>
  )
}
