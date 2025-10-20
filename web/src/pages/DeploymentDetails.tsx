import { ProjectResponse } from '@/api/client'
import {
  cancelDeploymentMutation,
  getDeploymentOptions,
  pauseDeploymentMutation,
  resumeDeploymentMutation,
  triggerProjectPipelineMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { DeploymentStages } from '@/components/deployments/DeploymentStages'
import { RedeploymentModal } from '@/components/deployments/RedeploymentModal'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Clock,
  GitBranch,
  GitCommit,
  MoreVertical,
  Pause,
  Play,
  RotateCw,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

interface DeploymentDetailsProps {
  project: ProjectResponse
}
export function DeploymentDetails({ project }: DeploymentDetailsProps) {
  const { deploymentId } = useParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [isRedeployModalOpen, setIsRedeployModalOpen] = useState(false)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const {
    data: deployment,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getDeploymentOptions({
      path: {
        project_id: project.id,
        deployment_id: Number(deploymentId) || 0,
      },
    }),
    enabled: !!project.slug && !!deploymentId,
    refetchInterval: (query) => {
      // Only auto-refresh if deployment is in a non-final state
      const status = query.state.data?.status
      if (status === 'pending' || status === 'running') {
        return 5000 // Refresh every 5 seconds
      }
      return false // Don't refresh for completed, failed, cancelled, or paused deployments
    },
  })

  const createDeployment = useMutation({
    ...triggerProjectPipelineMutation(),
    meta: {
      errorTitle: 'Failed to create deployment',
    },
    onSuccess: () => {
      toast.success('Deployment created successfully')
      setIsRedeployModalOpen(false)
    },
  })

  const pauseDeployment = useMutation({
    ...pauseDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to pause deployment',
    },
    onSuccess: () => {
      toast.success('Deployment paused successfully')
      refetch()
    },
  })

  const resumeDeployment = useMutation({
    ...resumeDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to resume deployment',
    },
    onSuccess: () => {
      toast.success('Deployment resumed successfully')
      refetch()
    },
  })

  const cancelDeployment = useMutation({
    ...cancelDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to cancel deployment',
    },
    onSuccess: () => {
      toast.success('Deployment cancelled successfully')
      refetch()
    },
  })

  const handleRedeploy = async ({
    branch,
    commit,
    tag,
    environmentId,
  }: {
    branch?: string
    commit?: string
    tag?: string
    environmentId: number
  }) => {
    await createDeployment.mutateAsync({
      path: {
        id: project.id,
      },
      body: {
        branch,
        commit,
        tag,
        environment_id: environmentId,
      },
    })

    navigate(`/projects/${project.slug}/deployments?autoRefresh=true`)
  }

  const handlePauseDeployment = async () => {
    await pauseDeployment.mutateAsync({
      path: {
        project_id: project.id,
        deployment_id: Number(deploymentId),
      },
    })
  }

  const handleResumeDeployment = async () => {
    await resumeDeployment.mutateAsync({
      path: {
        project_id: project.id,
        deployment_id: Number(deploymentId),
      },
    })
  }

  const handleCancelDeployment = async () => {
    await cancelDeployment.mutateAsync({
      path: {
        project_id: project.id,
        deployment_id: Number(deploymentId),
      },
    })
  }

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}` },
      { label: 'Deployments', href: `/projects/${project.slug}/deployments` },
      { label: `Deployment ${deploymentId}` },
    ])
  }, [setBreadcrumbs, project.slug, deploymentId])

  // Invalidate jobs query when deployment status changes to ensure fresh job data
  useEffect(() => {
    if (deployment) {
      queryClient.invalidateQueries({
        queryKey: [
          'get',
          '/projects/:project_id/deployments/:deployment_id/jobs',
          {
            path: {
              project_id: project.id,
              deployment_id: deployment.id,
            },
          },
        ],
      })
    }
  }, [deployment?.status, deployment?.id, deployment, project.id, queryClient])

  usePageTitle(`${project.name} - Deployment ${deploymentId}`)

  if (error) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6 space-y-6">
          <div className="flex items-center gap-4">
            <Button variant="outline" size="sm" asChild>
              <Link to={`/projects/${project.slug}/deployments`}>
                <ArrowLeft className="mr-2 h-4 w-4" />
                Back to Deployments
              </Link>
            </Button>
          </div>
          <ErrorAlert
            title="Failed to load deployment details"
            description={
              error instanceof Error
                ? error.message
                : 'An unexpected error occurred'
            }
            retry={() => refetch()}
          />
        </div>
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6 space-y-6">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-4">
              <Button variant="outline" size="sm" asChild>
                <Link to={`/projects/${project.slug}/deployments`}>
                  <ArrowLeft className="mr-2 h-4 w-4" />
                  Back to Deployments
                </Link>
              </Button>
              <Skeleton className="h-6 w-24" />
            </div>
            <div className="flex items-center gap-2">
              <Skeleton className="h-9 w-24" />
              <Skeleton className="h-9 w-24" />
              <Skeleton className="h-9 w-24" />
            </div>
          </div>

          <Card className="p-6">
            <div className="grid gap-6 md:grid-cols-2 lg:grid-cols-4">
              {Array.from({ length: 4 }).map((_, i) => (
                <div key={i} className="space-y-2">
                  <div className="flex items-center gap-2">
                    <Skeleton className="h-4 w-4" />
                    <Skeleton className="h-4 w-24" />
                  </div>
                  <Skeleton className="h-6 w-32" />
                </div>
              ))}
            </div>
          </Card>

          <Card>
            <div className="p-4">
              <div className="space-y-3">
                {Array.from({ length: 5 }).map((_, i) => (
                  <div key={i} className="flex items-start gap-2">
                    <Skeleton className="h-4 w-4 mt-1" />
                    <Skeleton className="h-4 w-full" />
                  </div>
                ))}
              </div>
            </div>
          </Card>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-6 space-y-6">
        {/* Header with Navigation and Deployment Info */}
        {deployment && (
          <Card>
            <div className="p-4">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-4">
                  <Button variant="outline" size="sm" asChild>
                    <Link to={`/projects/${project.slug}/deployments`}>
                      <ArrowLeft className="mr-2 h-4 w-4" />
                      Back to Deployments
                    </Link>
                  </Button>
                  <Badge
                    variant={
                      deployment.status === 'completed'
                        ? 'success'
                        : deployment.status === 'failed'
                          ? 'destructive'
                          : deployment.status === 'cancelled'
                            ? 'outline'
                            : 'secondary'
                    }
                  >
                    {deployment.status}
                  </Badge>

                  <div className="flex items-center gap-2">
                    <Clock className="h-4 w-4 text-muted-foreground" />
                    <TimeAgo date={deployment.created_at} className="text-sm" />
                  </div>
                </div>

                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setIsRedeployModalOpen(true)}
                    title="Redeploy"
                  >
                    <RotateCw className="h-4 w-4" />
                  </Button>
                  {(deployment.status === 'completed' ||
                    deployment.status === 'paused' ||
                    deployment.status === 'running' ||
                    deployment.status === 'pending') && (
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button
                          variant="outline"
                          size="sm"
                          title="More actions"
                        >
                          <MoreVertical className="h-4 w-4" />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        {(deployment?.status === 'running' ||
                          deployment?.status === 'pending') && (
                          <DropdownMenuItem
                            onClick={handleCancelDeployment}
                            disabled={cancelDeployment.isPending}
                          >
                            <X className="mr-2 h-4 w-4" />
                            Cancel Deployment
                          </DropdownMenuItem>
                        )}
                        {deployment?.status === 'completed' && (
                          <DropdownMenuItem
                            onClick={handlePauseDeployment}
                            disabled={pauseDeployment.isPending}
                          >
                            <Pause className="mr-2 h-4 w-4" />
                            Pause Deployment
                          </DropdownMenuItem>
                        )}
                        {deployment?.status === 'paused' && (
                          <DropdownMenuItem
                            onClick={handleResumeDeployment}
                            disabled={resumeDeployment.isPending}
                          >
                            <Play className="mr-2 h-4 w-4" />
                            Resume Deployment
                          </DropdownMenuItem>
                        )}
                      </DropdownMenuContent>
                    </DropdownMenu>
                  )}
                </div>
              </div>
            </div>
          </Card>
        )}

        {deployment && (
          <div className="grid gap-6 lg:grid-cols-3">
            {/* Left Column - Deployment Information */}
            <div className="lg:col-span-1 space-y-6">
              {/* Deployment Information Card */}
              <Card>
                <div className="p-6">
                  <h3 className="text-lg font-semibold mb-4">
                    Deployment Information
                  </h3>

                  {/* Status */}
                  <div className="mb-4">
                    <div className="text-sm text-muted-foreground mb-1">
                      Status
                    </div>
                    <Badge
                      variant={
                        deployment.status === 'completed'
                          ? 'success'
                          : deployment.status === 'failed'
                            ? 'destructive'
                            : deployment.status === 'cancelled'
                              ? 'outline'
                              : 'secondary'
                      }
                    >
                      {deployment.status}
                    </Badge>
                    {deployment.status === 'completed' && (
                      <div className="text-sm text-muted-foreground mt-1">
                        Pipeline execution started
                      </div>
                    )}
                    {deployment.status === 'failed' &&
                      deployment.error_message && (
                        <div className="mt-2 p-3 rounded-lg bg-destructive/10 border border-destructive/20">
                          <div className="text-sm font-medium text-destructive mb-1">
                            Error
                          </div>
                          <div className="text-sm text-destructive/90">
                            {deployment.error_message}
                          </div>
                        </div>
                      )}
                    {deployment.status === 'cancelled' &&
                      deployment.cancelled_reason && (
                        <div className="mt-2 p-3 rounded-lg bg-muted border">
                          <div className="text-sm font-medium mb-1">
                            Cancellation Reason
                          </div>
                          <div className="text-sm text-muted-foreground">
                            {deployment.cancelled_reason}
                          </div>
                        </div>
                      )}
                  </div>

                  {/* URLs */}
                  <div className="space-y-3">
                    <div className="text-sm font-medium">Source</div>
                    <div className="space-y-2">
                      <div className="flex items-center gap-2">
                        <GitBranch className="h-4 w-4 text-muted-foreground" />
                        <span className="text-sm">{deployment.branch}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <GitCommit className="h-4 w-4 text-muted-foreground" />
                        <span className="font-mono text-sm">
                          {deployment.commit_hash?.slice(0, 7)}
                        </span>
                        <span className="text-sm text-muted-foreground">
                          {deployment.commit_author || ''}
                        </span>
                      </div>
                      <span className="font-mono text-sm text-muted-foreground">
                        {deployment.commit_message}
                      </span>
                    </div>
                  </div>
                </div>
              </Card>
            </div>

            {/* Right Column - Logs and Stages */}
            <div className="lg:col-span-2">
              <DeploymentStages project={project} deployment={deployment} />
            </div>
          </div>
        )}

        <RedeploymentModal
          project={project}
          isOpen={isRedeployModalOpen}
          onClose={() => setIsRedeployModalOpen(false)}
          onConfirm={handleRedeploy}
          defaultBranch={deployment?.branch || ''}
          defaultCommit={deployment?.commit_hash || ''}
          defaultTag={deployment?.tag || ''}
          defaultEnvironment={deployment?.environment_id || 0}
          isLoading={createDeployment.isPending}
        />
      </div>
    </div>
  )
}
