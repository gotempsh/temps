import type {
  ProjectDashboardAnalytics,
  ProjectHealthSummary,
  ProjectResponse,
} from '@/api/client'
import { PresetIcon } from '@/components/presets/PresetIcon'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import {
  AlertCircle,
  Container,
  FileUp,
  GitFork,
  Network,
  PackageOpen,
  Users,
} from 'lucide-react'
import { Link } from 'react-router'
import { VisitorSparkline } from './VisitorSparkline'
import { ProjectCardMedia } from './ProjectCardMedia'
import { projectCardTraffic } from './project-card-traffic'
import {
  deploymentLabel,
  projectBuildSource,
  projectPresetLabel,
  projectRepository,
  type ProjectBuildSource,
} from './project-card-data'

interface ProjectCardProps {
  project: ProjectResponse
  layout?: 'wide' | 'compact' | 'dense'
  analytics?: ProjectDashboardAnalytics
  analyticsLoading?: boolean
  analyticsError?: boolean
  healthLoading?: boolean
  healthError?: boolean
  health?: ProjectHealthSummary
  latestDeploymentMedia?: {
    url?: string | null
    screenshot_location?: string | null
  }
}

function HealthStatusDot({ status }: { status: string }) {
  const colors: Record<string, string> = {
    operational: 'bg-emerald-500',
    healthy: 'bg-emerald-500',
    degraded: 'bg-amber-500',
    down: 'bg-red-500',
    no_monitors: 'bg-zinc-400',
    unknown: 'bg-zinc-400',
  }
  const label =
    status === 'no_monitors'
      ? 'No monitors'
      : status.charAt(0).toUpperCase() + status.slice(1)

  return (
    <span
      className={`inline-block size-2 rounded-full ${colors[status] || colors.unknown}`}
      title={label}
      aria-label={label}
    />
  )
}

function BuildSourceIcon({ kind }: { kind: ProjectBuildSource['kind'] }) {
  if (kind === 'github') return <GithubIcon className="size-4 shrink-0" />
  if (kind === 'gitlab') return <GitlabIcon className="size-4 shrink-0" />
  if (kind === 'git') {
    return <GitFork className="size-4 shrink-0 text-muted-foreground" />
  }
  if (kind === 'docker') {
    return <Container className="size-4 shrink-0 text-muted-foreground" />
  }
  return <FileUp className="size-4 shrink-0 text-muted-foreground" />
}

function MetadataCell({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="min-w-0">
      <div className="mb-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
        {label}
      </div>
      {children}
    </div>
  )
}

export function ProjectCard({
  project,
  layout = 'wide',
  analytics,
  analyticsLoading = false,
  analyticsError = false,
  healthLoading = false,
  healthError = false,
  health,
  latestDeploymentMedia,
}: ProjectCardProps) {
  const repository = projectRepository(project)
  const buildSource = projectBuildSource(project)
  const totalVisitors = analytics?.unique_visitors ?? 0
  const apiRequests = health?.total_requests ?? 0
  const traffic = projectCardTraffic(
    analytics?.hourly_visits,
    health?.hourly_requests,
    apiRequests
  )
  const trafficSparklineLabel =
    traffic.kind === 'visitors'
      ? 'Visitor traffic over the last 24 hours'
      : 'API request traffic over the last 24 hours'

  const activityContent =
    analyticsLoading && healthLoading ? (
      <Skeleton className="h-5 w-28" />
    ) : analyticsError && healthError ? (
      <span className="inline-flex items-center gap-1.5 text-sm text-muted-foreground">
        <AlertCircle className="size-3.5" /> Unavailable
      </span>
    ) : (
      <div className="flex min-w-0 items-center gap-3">
        <div className="flex min-w-fit flex-col gap-1 text-xs">
          {!healthError && (
            <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
              <Network className="size-3.5 text-muted-foreground" />
              <strong className="font-semibold tabular-nums">
                {healthLoading ? '…' : apiRequests.toLocaleString()}
              </strong>
              <span className="text-muted-foreground">API requests</span>
            </span>
          )}
          {!analyticsError && (
            <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
              <Users className="size-3.5 text-muted-foreground" />
              <strong className="font-semibold tabular-nums">
                {analyticsLoading ? '…' : totalVisitors.toLocaleString()}
              </strong>
              <span className="text-muted-foreground">visitors</span>
            </span>
          )}
        </div>
        <div
          className="min-w-16 flex-1"
          title={trafficSparklineLabel}
          aria-label={trafficSparklineLabel}
        >
          <VisitorSparkline
            data={traffic.data}
            className="w-full"
            height={30}
          />
        </div>
      </div>
    )

  if (layout === 'compact') {
    return (
      <Link
        to={`/projects/${project.slug}`}
        className="group flex min-h-44 flex-col rounded-xl border bg-card p-4 transition-colors hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="flex min-w-0 items-start justify-between gap-3">
          <div className="flex min-w-0 items-center gap-3">
            <ProjectCardMedia
              name={project.name}
              deploymentUrl={latestDeploymentMedia?.url}
              screenshotLocation={latestDeploymentMedia?.screenshot_location}
            />
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span className="truncate font-semibold group-hover:underline">
                  {project.name}
                </span>
                {health && health.status !== 'unknown' && (
                  <HealthStatusDot status={health.status} />
                )}
              </div>
              <p className="truncate text-xs text-muted-foreground">
                {project.slug}
              </p>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <span
              className="flex size-6 items-center justify-center rounded-md border bg-background"
              title={`Source: ${repository?.label ?? buildSource.label}`}
            >
              <BuildSourceIcon kind={buildSource.kind} />
            </span>
            <span title={`Preset: ${projectPresetLabel(project.preset)}`}>
              {project.preset ? (
                <PresetIcon
                  preset={project.preset}
                  label={projectPresetLabel(project.preset)}
                  className="size-6 rounded-md"
                  imageClassName="p-0.5"
                />
              ) : (
                <span className="flex size-6 items-center justify-center rounded-md border bg-background text-muted-foreground">
                  <PackageOpen className="size-3.5" />
                </span>
              )}
            </span>
          </div>
        </div>

        <div className="mt-4 rounded-lg bg-muted/35 px-3 py-2.5">
          {activityContent}
        </div>

        <div className="mt-auto flex items-end justify-between gap-3 border-t pt-3">
          <div className="min-w-0">
            <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              Latest deployment
            </p>
            {project.last_deployment ? (
              <p className="mt-1 truncate text-xs text-muted-foreground">
                <TimeAgo date={project.last_deployment} />
              </p>
            ) : (
              <p className="mt-1 text-xs text-muted-foreground">Not deployed</p>
            )}
          </div>
          {project.last_deployment && (
            <Badge variant="secondary" className="h-5 shrink-0 px-1.5">
              {deploymentLabel()}
            </Badge>
          )}
        </div>
      </Link>
    )
  }

  if (layout === 'dense') {
    return (
      <Link
        to={`/projects/${project.slug}`}
        className="group grid gap-3 px-3 py-2.5 transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring md:grid-cols-[minmax(12rem,1fr)_minmax(12rem,1fr)_minmax(13rem,1fr)_minmax(10rem,0.8fr)] md:items-center"
      >
        <div className="flex min-w-0 items-center gap-2.5">
          <ProjectCardMedia
            name={project.name}
            deploymentUrl={latestDeploymentMedia?.url}
            screenshotLocation={latestDeploymentMedia?.screenshot_location}
            className="size-8"
          />
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="truncate text-sm font-medium group-hover:underline">
                {project.name}
              </span>
              {health && health.status !== 'unknown' && (
                <HealthStatusDot status={health.status} />
              )}
            </div>
            <p className="truncate text-xs text-muted-foreground">
              {project.slug}
            </p>
          </div>
        </div>
        <div className="flex min-w-0 items-center gap-2 text-sm">
          <BuildSourceIcon kind={buildSource.kind} />
          <span className="truncate">
            {repository?.label ?? buildSource.label}
          </span>
          <span className="text-muted-foreground">·</span>
          <span className="truncate text-muted-foreground">
            {projectPresetLabel(project.preset)}
          </span>
        </div>
        <div className="min-w-0">{activityContent}</div>
        <div className="min-w-0 text-sm">
          {project.last_deployment ? (
            <div className="flex min-w-0 items-center gap-2">
              <Badge variant="secondary" className="h-5 shrink-0 px-1.5">
                Deployed
              </Badge>
              <span className="truncate text-xs text-muted-foreground">
                <TimeAgo date={project.last_deployment} />
              </span>
            </div>
          ) : (
            <span className="text-muted-foreground">Not deployed</span>
          )}
        </div>
      </Link>
    )
  }

  return (
    <Link
      to={`/projects/${project.slug}`}
      className="group grid gap-4 px-4 py-3.5 transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring lg:grid-cols-[minmax(14rem,0.8fr)_minmax(30rem,1.7fr)_minmax(11rem,0.65fr)] lg:items-center"
    >
      <div className="flex min-w-0 items-center gap-3">
        <ProjectCardMedia
          name={project.name}
          deploymentUrl={latestDeploymentMedia?.url}
          screenshotLocation={latestDeploymentMedia?.screenshot_location}
          className="size-9"
        />
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium group-hover:underline">
              {project.name}
            </span>
            {health && health.status !== 'unknown' && (
              <HealthStatusDot status={health.status} />
            )}
          </div>
          <p className="truncate text-xs text-muted-foreground">
            {project.slug}
          </p>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-x-6 gap-y-4 xl:grid-cols-3">
        <MetadataCell label="Source">
          <div
            className="flex min-w-0 items-center gap-2 text-sm"
            title={
              repository
                ? `${buildSource.label} · ${repository.label}`
                : buildSource.label
            }
          >
            <BuildSourceIcon kind={buildSource.kind} />
            <span className="truncate">
              {repository ? (
                <>
                  <span className="text-muted-foreground">
                    {buildSource.label} ·{' '}
                  </span>
                  <span className="font-mono text-xs">{repository.label}</span>
                </>
              ) : (
                buildSource.label
              )}
            </span>
          </div>
        </MetadataCell>

        <MetadataCell label="Preset">
          <div className="flex min-w-0 items-center gap-2 text-sm">
            {project.preset ? (
              <PresetIcon
                preset={project.preset}
                label={projectPresetLabel(project.preset)}
                className="size-6 shrink-0 rounded-md"
                imageClassName="p-1"
              />
            ) : (
              <span className="flex size-6 shrink-0 items-center justify-center rounded-md border bg-background text-muted-foreground">
                <PackageOpen className="size-3.5" />
              </span>
            )}
            <span className="truncate">
              {projectPresetLabel(project.preset)}
            </span>
          </div>
        </MetadataCell>

        <MetadataCell label="Activity · 24h">{activityContent}</MetadataCell>
      </div>

      <MetadataCell label="Latest deployment">
        {project.last_deployment ? (
          <div className="min-w-0">
            <Badge variant="secondary" className="h-5 px-1.5">
              Deployed
            </Badge>
            <p className="mt-1 truncate text-xs text-muted-foreground">
              <TimeAgo date={project.last_deployment} />
            </p>
          </div>
        ) : (
          <span className="text-sm text-muted-foreground">Not deployed</span>
        )}
      </MetadataCell>
    </Link>
  )
}
