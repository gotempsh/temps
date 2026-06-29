import { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  cancelDeploymentMutation,
  deployFromImageMutation,
  getDeploymentOptions,
  getSettingsOptions,
  pauseDeploymentMutation,
  resumeDeploymentMutation,
  rollbackToDeploymentMutation,
  triggerProjectPipelineMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { DeploymentContainerLogs } from '@/components/deployments/DeploymentContainerLogs'
import { DeploymentStages } from '@/components/deployments/DeploymentStages'
import { RedeploymentModal } from '@/components/deployments/RedeploymentModal'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
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
import { formatLocalDate } from '@/lib/date'
import { cn, formatBytes } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  ExternalLink,
  MoreVertical,
  Pause,
  Play,
  RotateCcw,
  RotateCw,
  X,
} from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

type BadgeVariant =
  | 'default'
  | 'secondary'
  | 'destructive'
  | 'success'
  | 'warning'
  | 'outline'

function statusBadgeVariant(status: string): BadgeVariant {
  switch (status) {
    case 'completed':
      return 'success'
    case 'failed':
      return 'destructive'
    case 'cancelled':
      return 'outline'
    default:
      return 'secondary'
  }
}

// The environment's URLs come through `environment.domains` (domains[0] is the
// env URL). A current deployment also has its own deployment-specific `url`.
function resolvePrimaryUrl(deployment: DeploymentResponse): string | null {
  if (deployment.is_current && deployment.url) return deployment.url
  const first = deployment.environment.domains?.[0]
  if (!first) return null
  return first.startsWith('http') ? first : `https://${first}`
}

function formatDurationMs(ms?: number | null): string | null {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return null
  const totalSeconds = Math.round(ms / 1000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`
}

function formatRange(startMs: number, endMs: number): string {
  const totalSeconds = Math.max(0, Math.round((endMs - startMs) / 1000))
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return `${minutes}m ${seconds}s`
}

interface UrlEntry {
  url: string
  display: string
  kind: 'primary' | 'preview'
}

function buildUrlEntries(
  deployment: DeploymentResponse,
  primaryUrl: string | null
): UrlEntry[] {
  const entries: UrlEntry[] = []
  const seen = new Set<string>()
  if (primaryUrl) {
    entries.push({ url: primaryUrl, display: primaryUrl, kind: 'primary' })
    seen.add(primaryUrl)
  }
  deployment.environment.domains?.forEach((domain) => {
    const url = domain.startsWith('http') ? domain : `https://${domain}`
    if (seen.has(url)) return
    seen.add(url)
    entries.push({ url, display: domain, kind: 'preview' })
  })
  return entries
}

interface StatItem {
  label: string
  value: ReactNode
}

function buildSummaryStats(deployment: DeploymentResponse): StatItem[] {
  const md = deployment.metadata
  const stats: StatItem[] = []
  const buildTime = formatDurationMs(md?.buildDurationMs)
  if (buildTime) stats.push({ label: 'Build time', value: buildTime })
  const deployTime = formatDurationMs(md?.deploymentDurationMs)
  if (deployTime) stats.push({ label: 'Deploy time', value: deployTime })
  if (deployment.finished_at) {
    stats.push({
      label: 'Total',
      value: formatRange(deployment.created_at, deployment.finished_at),
    })
  }
  if (md?.imageSizeBytes != null && md.imageSizeBytes > 0) {
    stats.push({ label: 'Image size', value: formatBytes(md.imageSizeBytes) })
  }
  if (md?.fileCount != null) {
    stats.push({ label: 'Files', value: md.fileCount.toLocaleString() })
  }
  return stats
}

function Stat({ label, value }: StatItem) {
  return (
    <div className="space-y-1">
      <dt className="truncate text-sm font-medium text-foreground">{label}</dt>
      <dd className="text-2xl font-semibold tabular-nums tracking-tight text-foreground">
        {value}
      </dd>
    </div>
  )
}

function Field({
  label,
  value,
  mono = false,
}: {
  label: string
  value: ReactNode
  mono?: boolean
}) {
  return (
    <div className="space-y-0.5">
      <dt className="text-sm font-medium text-foreground">{label}</dt>
      <dd className={cn('text-sm text-muted-foreground', mono && 'font-mono')}>
        {value}
      </dd>
    </div>
  )
}

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

  // docker_image projects re-pull the prebuilt image instead of the git pipeline.
  const redeployImage = useMutation({
    ...deployFromImageMutation(),
    meta: {
      errorTitle: 'Failed to redeploy image',
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
    if (project.source_type === 'docker_image') {
      const ref = deployment?.metadata?.externalImageRef
      if (!ref) {
        toast.error('No image reference found for this deployment')
        return
      }
      await redeployImage.mutateAsync({
        path: { project_id: project.id, environment_id: environmentId },
        body: { image_ref: ref },
      })
      navigate(`/projects/${project.slug}/deployments?autoRefresh=true`)
      return
    }

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
        <div className="space-y-6 p-6">
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
        <div className="space-y-6 p-6">
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

  const md = deployment?.metadata
  const cfg = deployment?.deployment_config
  const primaryUrl = deployment ? resolvePrimaryUrl(deployment) : null
  const urlEntries = deployment ? buildUrlEntries(deployment, primaryUrl) : []
  const buildStats = deployment ? buildSummaryStats(deployment) : []
  const isLive =
    deployment?.status === 'pending' || deployment?.status === 'running'

  const hasBuildConfig = Boolean(
    md &&
      (md.builder ||
        md.deploymentSourceType ||
        md.externalImageRef ||
        md.healthCheckPath ||
        md.dockerfilePath ||
        md.staticBundlePath ||
        md.imageUploadedLocally)
  )

  const resourceCells: { label: string; value: ReactNode; mono?: boolean }[] =
    []
  if (cfg?.cpuRequest != null)
    resourceCells.push({ label: 'CPU request', value: `${cfg.cpuRequest}m` })
  if (cfg?.cpuLimit != null)
    resourceCells.push({ label: 'CPU limit', value: `${cfg.cpuLimit}m` })
  if (cfg?.memoryRequest != null)
    resourceCells.push({
      label: 'Memory request',
      value: `${cfg.memoryRequest} MB`,
    })
  if (cfg?.memoryLimit != null)
    resourceCells.push({
      label: 'Memory limit',
      value: `${cfg.memoryLimit} MB`,
    })
  if (cfg?.replicas != null)
    resourceCells.push({ label: 'Replicas', value: `${cfg.replicas}` })
  if (cfg?.exposedPort != null)
    resourceCells.push({
      label: 'Exposed port',
      value: `${cfg.exposedPort}`,
      mono: true,
    })

  const featureToggles: { label: string; on: boolean }[] = []
  if (cfg) {
    const toggleDefs: { label: string; on?: boolean }[] = [
      { label: 'Auto deploy', on: cfg.automaticDeploy },
      { label: 'Session recording', on: cfg.sessionRecordingEnabled },
      { label: 'Performance metrics', on: cfg.performanceMetricsEnabled },
      { label: 'Container exec', on: cfg.containerExecEnabled },
    ]
    toggleDefs.forEach((t) => {
      if (typeof t.on === 'boolean')
        featureToggles.push({ label: t.label, on: t.on })
    })
  }

  const envVarCount = cfg?.environmentVariables
    ? Object.keys(cfg.environmentVariables).length
    : 0

  const hasResourceConfig =
    resourceCells.length > 0 || featureToggles.length > 0 || envVarCount > 0

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-4 sm:p-6">
        {/* Header with Navigation and Title */}
        {deployment && (
          <div className="space-y-4">
            <Button variant="ghost" size="sm" asChild className="-ml-2 gap-2">
              <Link to={`/projects/${project.slug}/deployments`}>
                <ArrowLeft className="h-4 w-4" />
                Back to Deployments
              </Link>
            </Button>

            {/* Rollback lineage banner */}
            {md?.isRollback && (
              <div className="flex items-center gap-2 rounded-md border border-gray-950/5 bg-muted/40 px-3 py-2 text-sm">
                <RotateCcw className="h-4 w-4 shrink-0 text-muted-foreground" />
                <span className="text-muted-foreground">
                  This is a rollback deployment
                  {md.rolledBackFromId ? (
                    <>
                      {' '}
                      restoring{' '}
                      <Link
                        to={`/projects/${project.slug}/deployments/${md.rolledBackFromId}`}
                        className="font-medium text-foreground hover:underline"
                      >
                        deployment #{md.rolledBackFromId}
                      </Link>
                    </>
                  ) : null}
                  .
                </span>
              </div>
            )}

            {/* Status badges + primary actions */}
            <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
              <div className="flex flex-wrap items-center gap-2">
                <Badge
                  variant={statusBadgeVariant(deployment.status)}
                  className="capitalize"
                >
                  {deployment.status}
                </Badge>
                {deployment.is_current && (
                  <Badge variant="success" className="gap-1 py-1 pl-1.5 pr-2.5">
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    Current
                  </Badge>
                )}
                {deployment.environment && (
                  <Badge variant="outline" className="capitalize">
                    {deployment.environment.name}
                  </Badge>
                )}
                {md?.deploymentSourceType === 'manual' && (
                  <Badge variant="outline">Manual deploy</Badge>
                )}
                {md?.labels?.map((label) => (
                  <Badge key={label} variant="secondary">
                    {label}
                  </Badge>
                ))}
                {isLive && (
                  <span className="inline-flex items-center gap-1.5 text-sm text-muted-foreground">
                    <span className="h-2 w-2 animate-pulse rounded-full bg-orange-500" />
                    Live
                  </span>
                )}
              </div>

              <div className="flex items-center gap-2">
                {primaryUrl && (
                  <Button asChild>
                    <a href={primaryUrl} target="_blank" rel="noreferrer">
                      Visit
                      <ExternalLink className="h-4 w-4" />
                    </a>
                  </Button>
                )}
                <Button
                  variant="outline"
                  onClick={() => setIsRedeployModalOpen(true)}
                  title="Redeploy"
                >
                  <RotateCw className="h-4 w-4" />
                  <span className="hidden sm:inline">Redeploy</span>
                </Button>
                {(deployment.status === 'completed' ||
                  deployment.status === 'paused' ||
                  deployment.status === 'running' ||
                  deployment.status === 'pending') && (
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="outline"
                        size="icon"
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

            {/* Key facts */}
            <dl className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-4">
              <Field
                label="Started"
                value={<TimeAgo date={deployment.created_at} />}
              />
              {deployment.finished_at && (
                <Field
                  label="Duration"
                  value={formatRange(
                    deployment.created_at,
                    deployment.finished_at
                  )}
                />
              )}
              {deployment.branch && (
                <Field label="Branch" value={deployment.branch} />
              )}
              {deployment.commit_hash && (
                <Field
                  label="Commit"
                  value={deployment.commit_hash.slice(0, 7)}
                  mono
                />
              )}
            </dl>

            {/* Commit Message */}
            {deployment.commit_message && (
              <div className="flex items-start gap-2">
                <div className="flex-1 border-l-2 border-gray-950/10 pl-3 text-sm italic text-muted-foreground">
                  <div
                    className={
                      isCommitMessageExpanded
                        ? 'text-pretty'
                        : 'line-clamp-1 overflow-hidden text-ellipsis'
                    }
                  >
                    &ldquo;{deployment.commit_message}&rdquo;
                  </div>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 w-6 shrink-0 p-0"
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

            {/* Commit attribution */}
            {(deployment.commit_author || deployment.commit_date) && (
              <p className="text-sm text-muted-foreground">
                {deployment.commit_author && (
                  <>
                    By{' '}
                    <span className="font-medium text-foreground">
                      {deployment.commit_author}
                    </span>
                  </>
                )}
                {deployment.commit_date && (
                  <> on {formatLocalDate(deployment.commit_date)}</>
                )}
              </p>
            )}

            {/* Cancelled Reason */}
            {deployment.cancelled_reason && (
              <div className="flex items-start gap-2">
                <div className="flex-1 border-l-2 border-destructive/50 pl-3 text-sm text-destructive">
                  <div className="mb-1 font-medium">Cancellation Reason</div>
                  <div className="text-sm text-destructive/80">
                    {deployment.cancelled_reason}
                  </div>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Build & deploy summary */}
        {deployment && buildStats.length > 0 && (
          <Card>
            <CardContent className="p-6">
              <dl className="grid grid-cols-2 gap-x-4 gap-y-6 sm:grid-cols-3 lg:grid-cols-5">
                {buildStats.map((stat) => (
                  <Stat
                    key={stat.label}
                    label={stat.label}
                    value={stat.value}
                  />
                ))}
              </dl>
            </CardContent>
          </Card>
        )}

        {/* Deployment URLs */}
        {deployment && urlEntries.length > 0 && (
          <Card>
            <CardContent className="space-y-3 p-6">
              <h2 className="text-base font-semibold">Deployment URLs</h2>
              <div className="space-y-2">
                {urlEntries.map((entry) => (
                  <div
                    key={entry.url}
                    className="flex items-center gap-2 rounded-md border border-gray-950/5 px-3 py-2"
                  >
                    <a
                      href={entry.url}
                      target="_blank"
                      rel="noreferrer"
                      className="flex min-w-0 flex-1 items-center gap-2 text-sm font-medium text-foreground hover:underline"
                    >
                      <span className="truncate">{entry.display}</span>
                      <ExternalLink className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                    </a>
                    <Badge
                      variant={
                        entry.kind === 'primary' ? 'secondary' : 'outline'
                      }
                      className="shrink-0"
                    >
                      {entry.kind === 'primary' ? 'Primary' : 'Preview'}
                    </Badge>
                    <CopyButton
                      value={entry.url}
                      minimal
                      className="h-7 w-7 shrink-0 rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
                    />
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        )}

        {/* Screenshot (only when one actually exists) */}
        {deployment && deployment.screenshot_location ? (
          <Card>
            <CardContent className="p-2">
              <ReloadableImage
                src={`/api/files${deployment.screenshot_location.startsWith('/') ? deployment.screenshot_location : '/' + deployment.screenshot_location}`}
                alt={`${project.name} deployment ${deployment.id}`}
                className="w-full rounded-md"
              />
            </CardContent>
          </Card>
        ) : deployment &&
          screenshotsEnabled &&
          (deployment.status === 'completed' ||
            deployment.status === 'running') ? (
          <p className="text-sm text-muted-foreground">
            {deployment.status === 'completed'
              ? 'Generating preview screenshot…'
              : 'Deployment in progress…'}
          </p>
        ) : null}

        {/* Build configuration */}
        {deployment && hasBuildConfig && md && (
          <Card>
            <CardContent className="space-y-4 p-6">
              <h2 className="text-base font-semibold">Build configuration</h2>
              <dl className="grid grid-cols-1 gap-x-4 gap-y-4 sm:grid-cols-2">
                {md.builder && (
                  <Field
                    label="Builder"
                    value={<span className="capitalize">{md.builder}</span>}
                  />
                )}
                {md.deploymentSourceType && (
                  <div className="space-y-0.5">
                    <dt className="text-sm font-medium text-foreground">
                      Source type
                    </dt>
                    <dd>
                      <Badge variant="outline" className="capitalize">
                        {String(md.deploymentSourceType).replace('_', ' ')}
                      </Badge>
                    </dd>
                  </div>
                )}
                {md.dockerfilePath && (
                  <Field label="Dockerfile" value={md.dockerfilePath} mono />
                )}
                {md.healthCheckPath && (
                  <Field
                    label="Health check path"
                    value={md.healthCheckPath}
                    mono
                  />
                )}
                {md.staticBundlePath && (
                  <Field
                    label="Static bundle"
                    value={md.staticBundlePath}
                    mono
                  />
                )}
                {md.externalImageRef && (
                  <div className="space-y-0.5 sm:col-span-2">
                    <dt className="text-sm font-medium text-foreground">
                      Image
                    </dt>
                    <dd className="flex items-center gap-2">
                      <span className="truncate font-mono text-sm text-muted-foreground">
                        {md.externalImageRef}
                      </span>
                      <CopyButton
                        value={md.externalImageRef}
                        minimal
                        className="h-7 w-7 shrink-0 rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
                      />
                    </dd>
                  </div>
                )}
              </dl>
              {md.imageUploadedLocally && (
                <p className="text-sm text-muted-foreground">
                  Image was loaded locally and not pulled from a registry.
                </p>
              )}
            </CardContent>
          </Card>
        )}

        {/* Resource configuration */}
        {deployment && hasResourceConfig && (
          <Card>
            <CardContent className="space-y-4 p-6">
              <h2 className="text-base font-semibold">
                Resource configuration
              </h2>
              {resourceCells.length > 0 && (
                <dl className="grid grid-cols-2 gap-x-4 gap-y-4 sm:grid-cols-3">
                  {resourceCells.map((cell) => (
                    <Field
                      key={cell.label}
                      label={cell.label}
                      value={cell.value}
                      mono={cell.mono}
                    />
                  ))}
                </dl>
              )}
              {(featureToggles.length > 0 || envVarCount > 0) && (
                <div className="flex flex-wrap items-center gap-2">
                  {featureToggles.map((toggle) => (
                    <Badge
                      key={toggle.label}
                      variant={toggle.on ? 'success' : 'outline'}
                    >
                      {toggle.label}
                    </Badge>
                  ))}
                  {envVarCount > 0 && (
                    <Badge variant="secondary">
                      {envVarCount} environment variable
                      {envVarCount === 1 ? '' : 's'}
                    </Badge>
                  )}
                </div>
              )}
            </CardContent>
          </Card>
        )}

        {/* Deployment Pipeline — failed stages expose a "Debug with AI" sidebar
            (ADR-023), gated on the project's ai_debug_chat_enabled toggle */}
        {deployment && (
          <DeploymentStages project={project} deployment={deployment} />
        )}

        {/* Captured logs from previous containers (survive teardown) */}
        {deployment && (
          <DeploymentContainerLogs
            projectId={deployment.project_id}
            deploymentId={deployment.id}
          />
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
            deployment?.tag ? 'tag' : deployment?.branch ? 'branch' : 'commit'
          }
          defaultEnvironment={deployment?.environment_id || 0}
          isLoading={createDeployment.isPending || redeployImage.isPending}
          imageRef={deployment?.metadata?.externalImageRef}
        />
      </div>
    </div>
  )
}
