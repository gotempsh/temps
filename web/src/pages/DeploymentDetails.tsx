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
import { useAssistantPageContext } from '@/components/ai/AiAssistantContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { formatMicrocores } from '@/lib/cpu-format'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowLeft,
  Camera,
  CheckCircle2,
  Clock,
  ExternalLink,
  GitBranch,
  GitCommitHorizontal,
  Globe,
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
// stable env URL). A deployment also has its own deployment-specific `url`.
// The current deployment is served at the environment's stable domain, so we
// surface that; older deployments fall back to their deployment-specific URL.
function resolvePrimaryUrl(deployment: DeploymentResponse): string | null {
  const normalize = (u: string) => (u.startsWith('http') ? u : `https://${u}`)
  const envUrl = deployment.environment.domains?.[0]
  if (deployment.is_current && envUrl) return normalize(envUrl)
  if (deployment.url) return normalize(deployment.url)
  return envUrl ? normalize(envUrl) : null
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
  return stats
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

function screenshotSrc(location: string): string {
  return `/api/files${location.startsWith('/') ? location : '/' + location}`
}

interface CommitUrls {
  commit: string | null
  branch: string | null
}

interface RepoWebBase {
  base: string
  isGitlab: boolean
}

// Best-effort derivation of the repository's web base URL so the commit hash and
// branch can deep-link back to the git provider. Falls back to plain text when we
// can't confidently build a URL.
function deriveRepoWebBase(project: ProjectResponse): RepoWebBase | null {
  const gitUrl = project.git_url
  if (gitUrl) {
    const https = gitUrl.match(/^https?:\/\/([^/]+)\/(.+?)(?:\.git)?\/?$/i)
    if (https)
      return {
        base: `https://${https[1]}/${https[2]}`,
        isGitlab: /gitlab/i.test(https[1]),
      }
    const scp = gitUrl.match(/^[^@]+@([^:]+):(.+?)(?:\.git)?\/?$/i)
    if (scp)
      return {
        base: `https://${scp[1]}/${scp[2]}`,
        isGitlab: /gitlab/i.test(scp[1]),
      }
    const ssh = gitUrl.match(/^ssh:\/\/[^@]+@([^/]+)\/(.+?)(?:\.git)?\/?$/i)
    if (ssh)
      return {
        base: `https://${ssh[1]}/${ssh[2]}`,
        isGitlab: /gitlab/i.test(ssh[1]),
      }
  }
  if (project.repo_owner && project.repo_name) {
    // No raw clone URL to sniff a host from. A non-null `gitlab_webhook_id` is
    // the one signal the frontend actually has that this project is wired up
    // through a GitLab provider connection (it's only ever set by
    // install_gitlab_webhook_for_connection); anything else defaults to
    // GitHub, the common case for connections without a raw git_url.
    const isGitlab = project.gitlab_webhook_id != null
    const host = isGitlab ? 'https://gitlab.com' : 'https://github.com'
    return {
      base: `${host}/${project.repo_owner}/${project.repo_name}`,
      isGitlab,
    }
  }
  return null
}

function commitWebUrls(
  project: ProjectResponse,
  deployment: DeploymentResponse
): CommitUrls {
  let resolved = deriveRepoWebBase(project)
  if (!resolved) {
    const gpe = deployment.metadata?.gitPushEvent
    if (gpe?.owner && gpe?.repo) {
      const isGitlab = project.gitlab_webhook_id != null
      const host = isGitlab ? 'https://gitlab.com' : 'https://github.com'
      resolved = { base: `${host}/${gpe.owner}/${gpe.repo}`, isGitlab }
    }
  }
  if (!resolved) return { commit: null, branch: null }
  const { base, isGitlab } = resolved
  const commitSeg = isGitlab ? '/-/commit/' : '/commit/'
  const treeSeg = isGitlab ? '/-/tree/' : '/tree/'
  return {
    commit: deployment.commit_hash
      ? `${base}${commitSeg}${deployment.commit_hash}`
      : null,
    branch: deployment.branch
      ? `${base}${treeSeg}${encodeURIComponent(deployment.branch)}`
      : null,
  }
}

interface OverviewActions {
  onRedeploy: () => void
  onCancel: () => void
  onPause: () => void
  onResume: () => void
  onRollback: () => void
  cancelPending: boolean
  pausePending: boolean
  resumePending: boolean
  rollbackPending: boolean
}

interface OverviewProps {
  project: ProjectResponse
  deployment: DeploymentResponse
  primaryUrl: string | null
  urlEntries: UrlEntry[]
  buildStats: StatItem[]
  isLive: boolean
  screenshotsEnabled: boolean
  commitUrls: CommitUrls
  actions: OverviewActions
}

// ---------------------------------------------------------------------------
// Overview building blocks
// ---------------------------------------------------------------------------

function StatusBadges({
  deployment,
  isLive,
}: {
  deployment: DeploymentResponse
  isLive: boolean
}) {
  const md = deployment.metadata
  return (
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
  )
}

function VisitButton({
  url,
  label = 'Visit',
  full = false,
  className,
}: {
  url: string
  label?: string
  full?: boolean
  className?: string
}) {
  return (
    <Button asChild className={cn(full && 'w-full', className)}>
      <a href={url} target="_blank" rel="noreferrer">
        {label}
        <ExternalLink className="h-4 w-4" />
      </a>
    </Button>
  )
}

function SecondaryActions({
  deployment,
  actions,
}: {
  deployment: DeploymentResponse
  actions: OverviewActions
}) {
  const showMenu = ['completed', 'paused', 'running', 'pending'].includes(
    deployment.status
  )
  return (
    <>
      <Button variant="outline" onClick={actions.onRedeploy} title="Redeploy">
        <RotateCw className="h-4 w-4" />
        <span className="hidden sm:inline">Redeploy</span>
      </Button>
      {showMenu && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="outline" size="icon" title="More actions">
              <MoreVertical className="h-4 w-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {(deployment.status === 'running' ||
              deployment.status === 'pending') && (
              <DropdownMenuItem
                onClick={actions.onCancel}
                disabled={actions.cancelPending}
              >
                <X className="mr-2 h-4 w-4" />
                Cancel Deployment
              </DropdownMenuItem>
            )}
            {deployment.status === 'completed' && (
              <DropdownMenuItem
                onClick={actions.onPause}
                disabled={actions.pausePending}
              >
                <Pause className="mr-2 h-4 w-4" />
                Pause Deployment
              </DropdownMenuItem>
            )}
            {deployment.status === 'completed' && (
              <DropdownMenuItem
                onClick={actions.onRollback}
                disabled={actions.rollbackPending}
              >
                <RotateCcw className="mr-2 h-4 w-4" />
                Rollback to this
              </DropdownMenuItem>
            )}
            {deployment.status === 'paused' && (
              <DropdownMenuItem
                onClick={actions.onResume}
                disabled={actions.resumePending}
              >
                <Play className="mr-2 h-4 w-4" />
                Resume Deployment
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </>
  )
}

// Top-level failure/cancellation banner shown directly under the header for
// deployments that didn't succeed.
function CancelledReason({ deployment }: { deployment: DeploymentResponse }) {
  if (!deployment.cancelled_reason) return null
  const isCancelled = deployment.status === 'cancelled'
  return (
    <div className="flex items-start gap-2.5 rounded-lg border border-destructive/30 bg-destructive/5 p-4">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
      <div className="min-w-0">
        <p className="text-sm font-medium text-destructive">
          {isCancelled ? 'Deployment cancelled' : 'Deployment failed'}
        </p>
        <p className="mt-0.5 break-words text-sm text-destructive/80">
          {deployment.cancelled_reason}
        </p>
      </div>
    </div>
  )
}

function DeploymentUrlsCard({
  entries,
  title = 'Deployment URLs',
}: {
  entries: UrlEntry[]
  title?: string
}) {
  if (entries.length === 0) return null
  return (
    <Card>
      <CardContent className="space-y-3 p-6">
        <h2 className="text-base font-semibold">{title}</h2>
        <div className="space-y-2">
          {entries.map((entry) => (
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
                variant={entry.kind === 'primary' ? 'secondary' : 'outline'}
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
  )
}

// A screenshot rendered inside browser chrome, with the live URL in the address
// bar. The address bar and the screenshot itself open the environment URL.
function BrowserFrameScreenshot({
  deployment,
  project,
  url,
  screenshotsEnabled,
}: {
  deployment: DeploymentResponse
  project: ProjectResponse
  url: string | null
  screenshotsEnabled: boolean
}) {
  const location = deployment.screenshot_location
  const display = url
    ? url.replace(/^https?:\/\//, '').replace(/\/$/, '')
    : deployment.environment.name
  const generating =
    !location &&
    screenshotsEnabled &&
    (deployment.status === 'completed' || deployment.status === 'running')

  // Only render a preview body when there's something to show — a screenshot,
  // or a short "generating" strip. With no screenshot, the frame collapses to
  // just the address bar rather than a large empty box.
  let body: ReactNode = null
  if (location) {
    body = (
      <ReloadableImage
        src={screenshotSrc(location)}
        alt={`${project.name} deployment ${deployment.id}`}
        className="block max-h-[420px] w-full bg-muted object-cover object-top"
      />
    )
  } else if (generating) {
    body = (
      <div className="flex h-36 w-full flex-col items-center justify-center gap-2 bg-muted/30 text-muted-foreground">
        <Camera className="h-5 w-5" />
        <span className="text-sm">
          {deployment.status === 'completed'
            ? 'Generating preview screenshot…'
            : 'Deployment in progress…'}
        </span>
      </div>
    )
  }

  return (
    <Card className="overflow-hidden">
      <div className="flex items-center gap-2 bg-muted/40 px-3 py-2">
        <div className="hidden shrink-0 items-center gap-1.5 sm:flex">
          <span className="h-2.5 w-2.5 rounded-full bg-gray-950/15" />
          <span className="h-2.5 w-2.5 rounded-full bg-gray-950/15" />
          <span className="h-2.5 w-2.5 rounded-full bg-gray-950/15" />
        </div>
        {url ? (
          <a
            href={url}
            target="_blank"
            rel="noreferrer"
            className="flex min-w-0 flex-1 items-center gap-2 rounded-md border border-gray-950/5 bg-background px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
          >
            <Globe className="h-3.5 w-3.5 shrink-0" />
            <span className="truncate">{display}</span>
            <ExternalLink className="ml-auto h-3 w-3 shrink-0" />
          </a>
        ) : (
          <div className="flex min-w-0 flex-1 items-center gap-2 rounded-md border border-gray-950/5 bg-background px-2.5 py-1 text-xs text-muted-foreground">
            <Globe className="h-3.5 w-3.5 shrink-0" />
            <span className="truncate">{display}</span>
          </div>
        )}
        {url && (
          <CopyButton
            value={url}
            minimal
            className="h-7 w-7 shrink-0 rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
          />
        )}
      </div>
      {body &&
        (url ? (
          <a
            href={url}
            target="_blank"
            rel="noreferrer"
            className="group relative block border-t border-gray-950/5"
          >
            {body}
            <span className="pointer-events-none absolute inset-0 bg-foreground/0 transition-colors group-hover:bg-foreground/5" />
          </a>
        ) : (
          <div className="border-t border-gray-950/5">{body}</div>
        ))}
    </Card>
  )
}

// Unified deployment header: back link, status/environment badges, and the
// commit summary (hash + message + branch) all on a single row, with the
// primary + secondary actions on the right — above a single divider so the
// page's substantive content (preview, timings, config, stages) starts
// immediately after it.
function DeploymentHeader({
  project,
  deployment,
  isLive,
  primaryUrl,
  wasDeployed,
  commitUrls,
  buildStats,
  actions,
}: {
  project: ProjectResponse
  deployment: DeploymentResponse
  isLive: boolean
  primaryUrl: string | null
  wasDeployed: boolean
  commitUrls: CommitUrls
  buildStats: StatItem[]
  actions: OverviewActions
}) {
  const shortHash = deployment.commit_hash?.slice(0, 7)
  const firstLine = deployment.commit_message?.split('\n')[0]
  const hasCommit = Boolean(shortHash || firstLine || deployment.branch)
  return (
    <div className="flex flex-col gap-3 border-b border-gray-950/10 pb-5 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-3 gap-y-2 sm:flex-nowrap">
        <Button
          variant="ghost"
          size="sm"
          asChild
          className="-ml-2 h-8 shrink-0 gap-1.5 px-2 text-muted-foreground"
        >
          <Link to={`/projects/${project.slug}/deployments`}>
            <ArrowLeft className="h-4 w-4" />
            Back
          </Link>
        </Button>
        <span
          className="hidden h-5 w-px shrink-0 bg-gray-950/10 sm:block"
          aria-hidden="true"
        />
        <div className="shrink-0">
          <StatusBadges deployment={deployment} isLive={isLive} />
        </div>
        {hasCommit && (
          <>
            <span
              className="hidden h-5 w-px shrink-0 bg-gray-950/10 sm:block"
              aria-hidden="true"
            />
            <GitCommitHorizontal className="hidden h-4 w-4 shrink-0 text-muted-foreground sm:block" />
            {shortHash &&
              (commitUrls.commit ? (
                <a
                  href={commitUrls.commit}
                  target="_blank"
                  rel="noreferrer"
                  className="hidden shrink-0 font-mono text-sm font-medium text-foreground hover:underline sm:inline"
                >
                  {shortHash}
                </a>
              ) : (
                <span className="hidden shrink-0 font-mono text-sm font-medium text-foreground sm:inline">
                  {shortHash}
                </span>
              ))}
            {firstLine && (
              <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
                {firstLine}
              </span>
            )}
            {deployment.branch && (
              <span className="inline-flex min-w-0 max-w-[160px] items-center gap-1 text-xs text-muted-foreground sm:max-w-none sm:shrink-0">
                <GitBranch className="h-3.5 w-3.5 shrink-0" />
                <span className="truncate">{deployment.branch}</span>
              </span>
            )}
          </>
        )}
        {buildStats.length > 0 && (
          <span className="ml-auto hidden shrink-0 items-center gap-2 pl-3 text-xs text-muted-foreground lg:flex">
            <Clock className="h-3.5 w-3.5" />
            {buildStats.map((stat, i) => (
              <span key={stat.label} className="inline-flex items-center gap-1">
                {i > 0 && <span className="text-muted-foreground/40">·</span>}
                <span>{stat.label}</span>
                <span className="font-medium tabular-nums text-foreground">
                  {stat.value}
                </span>
              </span>
            ))}
          </span>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-2">
        {wasDeployed && primaryUrl && <VisitButton url={primaryUrl} />}
        <SecondaryActions deployment={deployment} actions={actions} />
      </div>
    </div>
  )
}

// A screenshot-led hero where the live preview, the environment URL, and commit
// info are the focal point, stacked vertically.
function OverviewClassic(p: OverviewProps) {
  const { deployment, project, primaryUrl, urlEntries } = p
  // A failed/cancelled/in-progress deployment was never served, so its URL,
  // preview, and Visit affordances are meaningless — only show them once the
  // deployment has actually been deployed (completed, or completed-then-paused).
  const wasDeployed =
    deployment.status === 'completed' || deployment.status === 'paused'
  return (
    <div className="space-y-4">
      {wasDeployed && (
        <BrowserFrameScreenshot
          deployment={deployment}
          project={project}
          url={primaryUrl}
          screenshotsEnabled={p.screenshotsEnabled}
        />
      )}
      {/* Only list URLs when there's more than the primary already shown in the
          frame's address bar (e.g. extra preview/custom domains). */}
      {wasDeployed && urlEntries.length > 1 && (
        <DeploymentUrlsCard entries={urlEntries} />
      )}
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

  // Tell the assistant which deployment the user is looking at.
  const assistantContext = deployment
    ? [
        'The user is viewing a deployment in the Temps console.',
        `Project: "${project.name}" (slug: ${project.slug}, id: ${project.id}).`,
        `Deployment #${deployment.id} — status: ${deployment.status ?? 'unknown'}${deployment.environment ? `, environment: ${deployment.environment}` : ''}.`,
        deployment.branch ? `Branch: ${deployment.branch}.` : '',
        deployment.commit_hash
          ? `Commit: ${deployment.commit_hash.slice(0, 8)}${deployment.commit_message ? ` — ${deployment.commit_message.split('\n')[0]}` : ''}.`
          : '',
        deployment.cancelled_reason
          ? `Failure reason: ${deployment.cancelled_reason}.`
          : '',
        'Fetch details via the temps CLI: `deployments get_deployment`, `get_deployment_jobs`, `get_deployment_job_logs`.',
      ]
        .filter(Boolean)
        .join('\n')
    : null
  useAssistantPageContext(assistantContext, `deployment #${deploymentId}`)

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

  if (!deployment) {
    return null
  }

  const md = deployment.metadata
  const cfg = deployment.deployment_config
  const primaryUrl = resolvePrimaryUrl(deployment)
  const urlEntries = buildUrlEntries(deployment, primaryUrl)
  const buildStats = buildSummaryStats(deployment)
  const isLive =
    deployment.status === 'pending' || deployment.status === 'running'

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

  // The resource facts worth surfacing, as compact chips: CPU + memory
  // request/limit and replica count. Exposed port and feature toggles are omitted.
  const resourceBadges: string[] = []
  if (cfg?.cpuRequest != null) {
    const cpuLabel =
      cfg.cpuLimit != null && cfg.cpuLimit !== cfg.cpuRequest
        ? `${formatMicrocores(cfg.cpuRequest)}-${formatMicrocores(cfg.cpuLimit)} CPU`
        : `${formatMicrocores(cfg.cpuRequest)} CPU`
    resourceBadges.push(cpuLabel)
  }
  if (cfg?.memoryRequest != null) {
    const memoryLabel =
      cfg.memoryLimit != null && cfg.memoryLimit !== cfg.memoryRequest
        ? `${cfg.memoryRequest}-${cfg.memoryLimit} MB memory`
        : `${cfg.memoryRequest} MB memory`
    resourceBadges.push(memoryLabel)
  }
  if (cfg?.replicas != null)
    resourceBadges.push(
      `${cfg.replicas} replica${cfg.replicas === 1 ? '' : 's'}`
    )

  const overviewProps: OverviewProps = {
    project,
    deployment,
    primaryUrl,
    urlEntries,
    buildStats,
    isLive,
    screenshotsEnabled,
    commitUrls: commitWebUrls(project, deployment),
    actions: {
      onRedeploy: () => setIsRedeployModalOpen(true),
      onCancel: handleCancelDeployment,
      onPause: handlePauseDeployment,
      onResume: handleResumeDeployment,
      onRollback: handleRollbackDeployment,
      cancelPending: cancelDeployment.isPending,
      pausePending: pauseDeployment.isPending,
      resumePending: resumeDeployment.isPending,
      rollbackPending: rollbackDeployment.isPending,
    },
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-3 p-4 sm:p-6">
        {/* Unified header: back + status/env badges + commit + branch + timing,
            with actions on the right. */}
        <DeploymentHeader
          project={project}
          deployment={deployment}
          isLive={isLive}
          primaryUrl={primaryUrl}
          wasDeployed={
            deployment.status === 'completed' || deployment.status === 'paused'
          }
          commitUrls={overviewProps.commitUrls}
          buildStats={buildStats}
          actions={overviewProps.actions}
        />

        {/* Failure/cancellation reason — prominent, directly under the header. */}
        <CancelledReason deployment={deployment} />

        {resourceBadges.length > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            {resourceBadges.map((badge) => (
              <Badge key={badge} variant="secondary">
                {badge}
              </Badge>
            ))}
          </div>
        )}

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

        {/* Deployment overview — preview and (extra) URLs. */}
        <OverviewClassic {...overviewProps} />

        {/* Build configuration */}
        {hasBuildConfig && md && (
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

        {/* Deployment Pipeline — failed stages expose a "Debug with AI" sidebar
            (ADR-023), gated on the project's ai_debug_chat_enabled toggle */}
        <DeploymentStages project={project} deployment={deployment} />

        {/* Captured logs from previous containers (survive teardown) */}
        <DeploymentContainerLogs
          projectId={deployment.project_id}
          deploymentId={deployment.id}
        />

        <RedeploymentModal
          project={project}
          isOpen={isRedeployModalOpen}
          onClose={() => setIsRedeployModalOpen(false)}
          onConfirm={handleRedeploy}
          mode="redeploy"
          defaultBranch={deployment.branch || ''}
          defaultCommit={deployment.commit_hash || ''}
          defaultTag={deployment.tag || ''}
          defaultType={
            deployment.tag ? 'tag' : deployment.branch ? 'branch' : 'commit'
          }
          defaultEnvironment={deployment.environment_id || 0}
          isLoading={createDeployment.isPending || redeployImage.isPending}
          imageRef={deployment.metadata?.externalImageRef}
        />
      </div>
    </div>
  )
}
