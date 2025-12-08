import { ProjectResponse } from '@/api/client'
import {
  cancelDeploymentMutation,
  getDeploymentOptions,
  getSettingsOptions,
  pauseDeploymentMutation,
  resumeDeploymentMutation,
  rollbackToDeploymentMutation,
  triggerProjectPipelineMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { DeploymentStages } from '@/components/deployments/DeploymentStages'
import { RedeploymentModal } from '@/components/deployments/RedeploymentModal'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { ReloadableImage } from '@/components/utils/ReloadableImage'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Camera,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  Clock,
  ExternalLink,
  GitBranch,
  GitCommit,
  MoreVertical,
  Pause,
  Play,
  RotateCcw,
  RotateCw,
  Settings,
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
  const [isCommitMessageExpanded, setIsCommitMessageExpanded] = useState(false)
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
      const data = query.state.data
      const status = data?.status
      // Auto-refresh if deployment is in a non-final state
      if (status === 'pending' || status === 'running') {
        return 5000 // Refresh every 5 seconds
      }
      // Also refresh if deployment is completed but screenshot is not yet available
      // (screenshot job runs after deployment is marked complete)
      if (status === 'completed' && !data?.screenshot_location) {
        return 3000 // Refresh every 3 seconds while waiting for screenshot
      }
      return false // Don't refresh for completed (with screenshot), failed, cancelled, or paused deployments
    },
  })

  // Fetch platform settings to check if screenshots are enabled
  const { data: settings } = useQuery({
    ...getSettingsOptions(),
    retry: false,
  })

  const screenshotsEnabled = settings?.screenshots?.enabled ?? false

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

  const rollbackDeployment = useMutation({
    ...rollbackToDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to rollback deployment',
    },
    onSuccess: () => {
      toast.success('Deployment rollback initiated successfully')
      navigate(`/projects/${project.slug}/deployments?autoRefresh=true`)
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

  const handleRollbackDeployment = async () => {
    await rollbackDeployment.mutateAsync({
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

  usePageTitle(`${project.slug} - Deployment ${deploymentId}`)

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
        {/* Header with Navigation and Title */}
        {deployment && (
          <div className="space-y-4">
            <Button variant="ghost" size="sm" asChild className="gap-2">
              <Link to={`/projects/${project.slug}/deployments`}>
                <ArrowLeft className="h-4 w-4" />
                Back to Deployments
              </Link>
            </Button>

            {/* Metadata Row - Single Line with Status and Actions */}
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-4 text-sm text-muted-foreground">
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
                  className="capitalize flex items-center gap-1.5"
                >
                  <span
                    className={`h-2 w-2 rounded-full ${
                      deployment.status === 'completed'
                        ? 'bg-green-500 dark:bg-green-400'
                        : deployment.status === 'failed'
                          ? 'bg-red-500 dark:bg-red-400'
                          : deployment.status === 'cancelled'
                            ? 'bg-gray-500 dark:bg-gray-400'
                            : deployment.status === 'running'
                              ? 'bg-orange-500 dark:bg-orange-400 animate-pulse'
                              : 'bg-blue-500 dark:bg-blue-400'
                    }`}
                  />
                  {deployment.status}
                </Badge>
                {deployment.is_current && (
                  <Badge
                    variant="default"
                    className="bg-green-600 hover:bg-green-700 flex items-center gap-1"
                  >
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    Current
                  </Badge>
                )}
                <span className="text-muted-foreground/30">•</span>
                <div className="flex items-center gap-1.5">
                  <Clock className="h-4 w-4" />
                  <span>Started:</span>
                  <TimeAgo date={deployment.created_at} />
                </div>
                {deployment.finished_at && (
                  <>
                    <span className="text-muted-foreground/30">•</span>
                    <div className="flex items-center gap-1.5">
                      <Clock className="h-4 w-4" />
                      <span>Duration:</span>
                      <span>
                        {Math.round(
                          (new Date(deployment.finished_at).getTime() -
                            new Date(deployment.created_at).getTime()) /
                            1000 /
                            60
                        )}
                        m{' '}
                        {Math.round(
                          ((new Date(deployment.finished_at).getTime() -
                            new Date(deployment.created_at).getTime()) /
                            1000) %
                            60
                        )}
                        s
                      </span>
                    </div>
                  </>
                )}
                <span className="text-muted-foreground/30">•</span>
                <div className="flex items-center gap-1.5">
                  <GitBranch className="h-4 w-4" />
                  <span>Branch:</span>
                  <span className="font-medium text-foreground">
                    {deployment.branch}
                  </span>
                </div>
                <span className="text-muted-foreground/30">•</span>
                <div className="flex items-center gap-1.5">
                  <GitCommit className="h-4 w-4" />
                  <span>Commit:</span>
                  <span className="font-mono font-medium text-foreground">
                    {deployment.commit_hash?.slice(0, 7)}
                  </span>
                </div>
                {deployment.environment && (
                  <>
                    <span className="text-muted-foreground/30">•</span>
                    <div className="flex items-center gap-1.5">
                      <span>Environment:</span>
                      <Badge variant="secondary" className="capitalize">
                        {deployment.environment.name}
                      </Badge>
                    </div>
                  </>
                )}
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
                      <Button variant="outline" size="sm" title="More actions">
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
                      {deployment?.status === 'completed' && (
                        <DropdownMenuItem
                          onClick={handleRollbackDeployment}
                          disabled={rollbackDeployment.isPending}
                        >
                          <RotateCcw className="mr-2 h-4 w-4" />
                          Rollback to this
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

            {/* Commit Message - Separate line if exists */}
            {deployment.commit_message && (
              <div className="flex items-start gap-2 mt-2">
                <div className="flex-1 text-sm text-muted-foreground italic border-l-2 border-muted pl-3">
                  <div
                    className={
                      isCommitMessageExpanded
                        ? ''
                        : 'line-clamp-1 overflow-hidden text-ellipsis'
                    }
                  >
                    &ldquo;{deployment.commit_message}&rdquo;
                  </div>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 w-6 p-0 shrink-0"
                  onClick={() =>
                    setIsCommitMessageExpanded(!isCommitMessageExpanded)
                  }
                >
                  {isCommitMessageExpanded ? (
                    <ChevronUp className="h-3.5 w-3.5" />
                  ) : (
                    <ChevronDown className="h-3.5 w-3.5" />
                  )}
                </Button>
              </div>
            )}

            {/* Cancelled Reason - Show if deployment was cancelled */}
            {deployment.cancelled_reason && (
              <div className="flex items-start gap-2 mt-2">
                <div className="flex-1 text-sm text-destructive border-l-2 border-destructive/50 pl-3">
                  <div className="font-medium mb-1">Cancellation Reason</div>
                  <div className="text-sm text-destructive/80">
                    {deployment.cancelled_reason}
                  </div>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Screenshot and Domains Card */}
        {deployment && (
          <Card>
            <CardContent className="p-6">
              <div className="flex flex-col md:flex-row gap-6 md:gap-4">
                {/* Screenshot Section */}
                <div className="w-full md:w-1/3">
                  {!screenshotsEnabled ? (
                    <div className="flex items-center justify-center">
                      <Card className="w-full bg-muted/50 border-dashed">
                        <CardContent className="flex flex-col items-center justify-center h-48 text-center p-4">
                          <Camera className="h-8 w-8 text-muted-foreground mb-2" />
                          <p className="text-sm text-muted-foreground mb-3">
                            Screenshot generation is disabled
                          </p>
                          <Link to="/settings">
                            <Button variant="outline" size="sm">
                              <Settings className="h-3 w-3 mr-1" />
                              Enable in Settings
                            </Button>
                          </Link>
                        </CardContent>
                      </Card>
                    </div>
                  ) : deployment.screenshot_location ? (
                    <ReloadableImage
                      src={`/api/files${deployment.screenshot_location?.startsWith('/') ? deployment.screenshot_location : '/' + deployment.screenshot_location}`}
                      alt={`${project.name} deployment ${deployment.id}`}
                      className="w-full rounded-md"
                    />
                  ) : deployment.status === 'failed' ? (
                    <div className="flex items-center justify-center">
                      <Card className="w-full bg-muted/50 border-dashed">
                        <CardContent className="flex items-center justify-center h-48">
                          <p className="text-muted-foreground">
                            Failed to deploy
                          </p>
                        </CardContent>
                      </Card>
                    </div>
                  ) : (
                    <div className="flex items-center justify-center">
                      <Card className="w-full bg-muted/50 border-dashed">
                        <CardContent className="flex items-center justify-center h-48">
                          <p className="text-muted-foreground">
                            {deployment.status === 'completed'
                              ? 'Generating screenshot...'
                              : deployment.status === 'running'
                                ? 'Deployment in progress...'
                                : 'Waiting for deployment...'}
                          </p>
                        </CardContent>
                      </Card>
                    </div>
                  )}
                </div>

                {/* Domains and URL Section */}
                <div className="w-full md:w-2/3">
                  <h3 className="text-lg font-semibold mb-4">
                    Deployment URLs
                  </h3>
                  <div className="flex flex-col items-start gap-2">
                    {deployment.environment.domains?.map((domain) => {
                      const url = domain.startsWith('http')
                        ? domain
                        : `https://${domain}`
                      return (
                        <div
                          key={domain}
                          className="flex items-center gap-1 cursor-pointer hover:opacity-80 transition-opacity"
                          onClick={() => window.open(url, '_blank')}
                        >
                          <span className="text-sm text-muted-foreground truncate">
                            {domain}
                          </span>
                          <ExternalLink className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                        </div>
                      )
                    })}
                    {/* Show deployment URL only if current, otherwise show environment URL */}
                    {deployment.is_current && deployment.url ? (
                      <div
                        className="flex items-center gap-1 cursor-pointer hover:opacity-80 transition-opacity"
                        onClick={() => window.open(deployment.url, '_blank')}
                      >
                        <span className="text-sm text-muted-foreground truncate">
                          {deployment.url}
                        </span>
                        <ExternalLink className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                      </div>
                    ) : (
                      deployment.environment.main_url && (
                        <div
                          className="flex items-center gap-1 cursor-pointer hover:opacity-80 transition-opacity"
                          onClick={() =>
                            window.open(deployment.environment.main_url, '_blank')
                          }
                        >
                          <span className="text-sm text-muted-foreground truncate">
                            {deployment.environment.main_url}
                          </span>
                          <ExternalLink className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                        </div>
                      )
                    )}
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>
        )}

        {/* Deployment Pipeline */}
        {deployment && (
          <DeploymentStages project={project} deployment={deployment} />
        )}

        <RedeploymentModal
          project={project}
          isOpen={isRedeployModalOpen}
          onClose={() => setIsRedeployModalOpen(false)}
          onConfirm={handleRedeploy}
          mode="redeploy"
          defaultBranch={deployment?.branch || ''}
          defaultCommit={deployment?.commit_hash || ''}
          defaultTag={deployment?.tag || ''}
          defaultType={
            deployment?.tag
              ? 'tag'
              : deployment?.branch
                ? 'branch'
                : 'commit'
          }
          defaultEnvironment={deployment?.environment_id || 0}
          isLoading={createDeployment.isPending}
        />
      </div>
    </div>
  )
}
