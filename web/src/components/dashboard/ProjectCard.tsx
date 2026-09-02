// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProjectDashboardAnalytics,
  ProjectHealthSummary,
  ProjectMonitorHealth,
  ProjectResponse,
} from '@/api/client'
import { PresetIcon } from '@/components/presets/PresetIcon'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { ProviderLogo } from '@/components/git/ProviderLogo'
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
  projectHealthIndicator,
  type ProjectHealthIndicator,
  type ProjectHealthTone,
} from './project-card-health'
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
  /** Latest production uptime-monitor status; outranks traffic health. */
  monitorHealth?: ProjectMonitorHealth
  latestDeploymentMedia?: {
    latest_attempt_status: string
    url?: string | null
    screenshot_location?: string | null
  }
  latestDeploymentMediaLoading?: boolean
  latestDeploymentMediaError?: boolean
}

const HEALTH_TONE_STYLES: Record<
  ProjectHealthTone,
  { dot: string; text: string }
> = {
  healthy: {
    dot: 'bg-emerald-500',
    text: 'text-emerald-600 dark:text-emerald-400',
  },
  degraded: { dot: 'bg-amber-500', text: 'text-amber-600 dark:text-amber-400' },
  down: { dot: 'bg-red-500', text: 'text-red-700 dark:text-red-400' },
  idle: { dot: 'bg-zinc-300', text: 'text-muted-foreground' },
  unavailable: { dot: 'bg-zinc-400', text: 'text-muted-foreground' },
  pending: { dot: 'bg-zinc-300 animate-pulse', text: 'text-muted-foreground' },
}

/**
 * Always rendered. There is no health state — including "we could not measure
 * it" — that is better communicated by drawing nothing.
 */
function ProjectHealth({ indicator }: { indicator: ProjectHealthIndicator }) {
  const tone = HEALTH_TONE_STYLES[indicator.tone]

  return (
    <span
      className={`inline-flex shrink-0 items-center gap-1.5 text-xs ${tone.text}`}
      title={indicator.detail}
    >
      <span className={`inline-block size-2 rounded-full ${tone.dot}`} />
      <span className="whitespace-nowrap">{indicator.label}</span>
      <span className="sr-only">. {indicator.detail}</span>
    </span>
  )
}

function BuildSourceIcon({ kind }: { kind: ProjectBuildSource['kind'] }) {
  if (
    kind === 'github' ||
    kind === 'gitlab' ||
    kind === 'gitea' ||
    kind === 'bitbucket'
  ) {
    return <ProviderLogo providerType={kind} className="size-4 shrink-0" />
  }
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
  monitorHealth,
  latestDeploymentMedia,
  latestDeploymentMediaLoading = false,
  latestDeploymentMediaError = false,
}: ProjectCardProps) {
  const repository = projectRepository(project)
  const buildSource = projectBuildSource(project)
  const totalVisitors = analytics?.unique_visitors ?? 0
  const apiRequests = health?.total_requests ?? 0
  const healthIndicator = projectHealthIndicator({
    health,
    monitor: monitorHealth,
    loading: healthLoading,
    error: healthError,
  })
  const traffic = projectCardTraffic(
    analytics?.hourly_visits,
    health?.hourly_requests,
    apiRequests
  )
  const trafficSparklineLabel =
    traffic.kind === 'visitors'
      ? 'Visitor traffic over the last 24 hours'
      : 'API request traffic over the last 24 hours'

  const deploymentBadge = latestDeploymentMediaLoading ? (
    <Skeleton className="h-5 w-20" />
  ) : latestDeploymentMediaError ? (
    <Badge variant="outline" className="h-5 shrink-0 px-1.5">
      Unavailable
    </Badge>
  ) : (
    <Badge variant="secondary" className="h-5 shrink-0 px-1.5">
      {deploymentLabel(latestDeploymentMedia?.latest_attempt_status)}
    </Badge>
  )

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
                <ProjectHealth indicator={healthIndicator} />
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
          {project.last_deployment && deploymentBadge}
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
              <ProjectHealth indicator={healthIndicator} />
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
              {deploymentBadge}
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
            <ProjectHealth indicator={healthIndicator} />
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
            {deploymentBadge}
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
