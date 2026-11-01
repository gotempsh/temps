// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getDeploymentJobsOptions,
  getDeploymentOptions,
  getLastDeploymentOptions,
  getProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ElapsedTime } from '@/components/global/ElapsedTime'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowRight,
  ExternalLink,
  FolderGit2,
  FolderTree,
  GitBranch,
  GitCompare,
  Layers,
  Monitor,
  Network,
  Rocket,
  SquareTerminal,
  X,
  type LucideIcon,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router'
import { ArchitectureTab } from './ArchitectureTab'
import { ChatChangesTab } from './ChatChangesTab'
import { ChatFilesTab } from './ChatFilesTab'
import { ChatPreviewTab } from './ChatPreviewTab'
import { ChatTerminalTab } from './ChatTerminalTab'
import { ChatResourcesTab } from './ChatResourcesTab'
import { ProjectTab } from './ProjectTab'
import type { ProjectRef } from './project-refs'
import {
  deploymentUrlNote,
  isActiveDeploymentStatus,
  isFailedDeploymentStatus,
  isReadyDeploymentStatus,
} from './deployment-card-state'
import { JobStatusIcon, statusPresentation } from './GeneratedDeploymentCard'
import type { WatchedDeployment } from './goal-presentation'
import { DEPLOYMENTS_TAB, sidebarSelection } from './sidebar-selection'

/** Projects opened from replies beyond this many are not given a tab. */
const MAX_PROJECT_TABS = 6

/** The fixed tab that shows the application's architecture. */
export const ARCHITECTURE_TAB = 'architecture'

/** The fixed tab that lists what the chat made. */
const MADE_TAB = 'made'

/** The fixed tab that shows the page the chat's sandbox is serving. */
export const PREVIEW_TAB = 'preview'
const TERMINAL_TAB = 'terminal'
const FILES_TAB = 'files'
const CHANGES_TAB = 'changes'

/** The tabs every chat has, in order. Deployments and projects follow. */
const CHAT_TABS: ReadonlyArray<{
  value: string
  label: string
  Icon: LucideIcon
}> = [
  { value: ARCHITECTURE_TAB, label: 'Architecture', Icon: Network },
  { value: MADE_TAB, label: 'Made here', Icon: Layers },
  { value: PREVIEW_TAB, label: 'Preview', Icon: Monitor },
  { value: FILES_TAB, label: 'Files', Icon: FolderTree },
  { value: CHANGES_TAB, label: 'Changes', Icon: GitCompare },
  { value: TERMINAL_TAB, label: 'Terminal', Icon: SquareTerminal },
]

/**
 * A tab's label, hidden to an icon when the panel is narrow so the strip
 * fits without scrolling. The active tab and a wide panel keep it visible;
 * screen readers always get it.
 */
const COLLAPSIBLE_LABEL =
  'max-w-32 truncate sr-only group-data-[state=active]:not-sr-only @xl:not-sr-only'

function projectKey(project: ProjectRef): string {
  return `project:${project.id}`
}

/** How far along the build is, as whole stages finished out of all of them. */
function stageProgress(jobs: ReadonlyArray<{ status: string }>): {
  done: number
  total: number
} {
  return {
    done: jobs.filter((job) => job.status === 'success').length,
    total: jobs.length,
  }
}

/**
 * A watched deployment, polled while it is still building. A goal that watches
 * "the latest deployment" follows whichever one that is.
 */
function useWatchedDeployment(projectId: number, deploymentId: number | null) {
  const poll = (query: { state: { data?: { status: string } } }) =>
    query.state.data && !isActiveDeploymentStatus(query.state.data.status)
      ? false
      : 3000
  const pinned = useQuery({
    ...getDeploymentOptions({
      path: { project_id: projectId, deployment_id: deploymentId ?? 0 },
    }),
    enabled: deploymentId !== null,
    refetchInterval: poll,
  })
  const latest = useQuery({
    ...getLastDeploymentOptions({ path: { id: projectId } }),
    // `deployment: null` (nothing deployed yet) reads like a deployment
    // that has not appeared; keep polling until one does.
    select: (data) => data.deployment ?? undefined,
    enabled: deploymentId === null,
    refetchInterval: (query) => {
      const deployment = query.state.data?.deployment
      return deployment && !isActiveDeploymentStatus(deployment.status)
        ? false
        : 3000
    },
  })
  return deploymentId === null ? latest : pinned
}

function DeploymentTab({
  projectId,
  deploymentId,
}: {
  projectId: number
  deploymentId: number | null
}) {
  const deployment = useWatchedDeployment(projectId, deploymentId)
  const active =
    deployment.data !== undefined &&
    isActiveDeploymentStatus(deployment.data.status)
  const jobs = useQuery({
    ...getDeploymentJobsOptions({
      path: {
        project_id: projectId,
        deployment_id: deployment.data?.id ?? 0,
      },
    }),
    enabled: deployment.data !== undefined,
    // One last fetch happens when the deployment stops being active, so the
    // final stage statuses are never left half-updated.
    refetchInterval: active ? 1500 : false,
  })
  const project = useQuery(getProjectOptions({ path: { id: projectId } }))

  if (deployment.isPending) {
    return (
      <div className="space-y-3 p-4">
        <Skeleton className="h-5 w-40" />
        <Skeleton className="h-4 w-56" />
        <Skeleton className="h-24 w-full" />
      </div>
    )
  }
  // Following "the latest deployment" of a project that has none yet is a
  // successful read with nothing in it: say it is waiting, not that it failed.
  if (deploymentId === null && deployment.isSuccess && !deployment.data) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        This project has no deployment yet. Its first one shows here as soon as
        it starts.
      </p>
    )
  }
  if (deployment.isError || !deployment.data) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Temps could not load this deployment. It may have been removed, or you
        may not have access to it.
      </p>
    )
  }

  const data = deployment.data
  const status = statusPresentation(data.status)
  const stages = jobs.data?.jobs ?? []
  const progress = stageProgress(stages)
  const slug = project.data?.slug
  const live = isReadyDeploymentStatus(data.status) && data.url
  const urlNote = deploymentUrlNote(data.status)
  return (
    <div className="flex min-h-0 flex-col" aria-live="polite">
      <div className="space-y-2 px-4 py-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="text-sm font-semibold">
            {project.data?.name ?? 'Deployment'} · #{data.id}
          </h3>
          <span className={cn('text-xs font-medium', status.className)}>
            {status.label}
          </span>
        </div>
        <p className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          <span>{data.environment.name}</span>
          {data.branch && (
            <span className="inline-flex min-w-0 items-center gap-1 font-mono">
              <GitBranch className="size-3 shrink-0" />
              <span className="truncate">{data.branch}</span>
              {data.commit_hash && (
                <span>@ {data.commit_hash.slice(0, 7)}</span>
              )}
            </span>
          )}
          <span>
            started <TimeAgo date={data.started_at ?? data.created_at} />
          </span>
        </p>
        {data.url && (
          <div className="flex min-w-0 items-center gap-1.5">
            {live ? (
              <a
                href={data.url}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex min-w-0 items-start gap-1 text-sm font-medium text-primary underline-offset-2 hover:underline"
              >
                <span className="break-all">{data.url}</span>
                <ExternalLink className="mt-0.5 size-3.5 shrink-0" />
              </a>
            ) : (
              <span
                className="break-all text-sm text-muted-foreground"
                title={
                  urlNote === 'once live'
                    ? 'The link opens once the deployment is live'
                    : 'This deployment is not serving traffic'
                }
              >
                {data.url}
                {urlNote && <span className="ml-1 text-xs">({urlNote})</span>}
              </span>
            )}
            <CopyButton value={data.url} className="size-6 shrink-0" />
          </div>
        )}
        {data.failure && (
          <p className="rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-2 text-xs text-destructive">
            {data.failure.title}
          </p>
        )}
      </div>

      <div className="border-t px-4 py-3">
        <div className="mb-2 flex items-center justify-between text-xs">
          <span className="font-medium">Stages</span>
          {progress.total > 0 && (
            <span className="text-muted-foreground">
              {progress.done} of {progress.total}
            </span>
          )}
        </div>
        {progress.total > 0 && (
          <div
            role="progressbar"
            aria-label="Deployment stages finished"
            aria-valuemin={0}
            aria-valuemax={progress.total}
            aria-valuenow={progress.done}
            className="mb-3 h-1 overflow-hidden rounded-full bg-muted"
          >
            <div
              className={cn(
                'h-full rounded-full transition-[width]',
                isFailedDeploymentStatus(data.status)
                  ? 'bg-destructive'
                  : 'bg-primary'
              )}
              style={{ width: `${(progress.done / progress.total) * 100}%` }}
            />
          </div>
        )}
        {jobs.isPending ? (
          <Skeleton className="h-20 w-full" />
        ) : stages.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            The stages appear here as soon as the build starts.
          </p>
        ) : (
          <ol className="grid gap-2">
            {stages.map((job) => (
              <li
                key={job.id}
                className="flex min-w-0 items-center gap-2 text-sm"
              >
                <JobStatusIcon status={job.status} />
                <span className="min-w-0 flex-1 truncate">{job.name}</span>
                {job.started_at ? (
                  <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                    <ElapsedTime
                      startedAt={job.started_at}
                      endedAt={job.finished_at}
                    />
                  </span>
                ) : (
                  <span className="shrink-0 text-xs capitalize text-muted-foreground">
                    {job.status}
                  </span>
                )}
              </li>
            ))}
          </ol>
        )}
      </div>

      {slug && (
        <div className="border-t px-4 py-3">
          <Button asChild size="sm" variant="outline" className="h-8">
            <Link to={`/projects/${slug}/deployments/${data.id}`}>
              View logs and details
              <ArrowRight aria-hidden="true" />
            </Link>
          </Button>
        </div>
      )}
    </div>
  )
}

/**
 * What this chat is working on: the chat's own tabs, one Deployments tab with
 * every deployment it started or watched and their build stages, and the
 * projects the user opened from a reply. Sits beside the conversation so
 * progress stays visible while the assistant keeps working.
 */
export function DeploymentSidebar({
  deployments,
  projects = [],
  focus,
  conversationPublicId,
  initialSelected,
  onSelectedChange,
  onOpenProject,
  onClose,
  inDrawer = false,
}: {
  /** Newest first. */
  deployments: WatchedDeployment[]
  /** Projects opened from the chat, newest first. */
  projects?: ProjectRef[]
  /** Selects a tab each time `nonce` changes, even if it is already open. */
  focus?: { key: string; nonce: number }
  /** The chat, for the tab listing what it made. Without it that tab is left out. */
  conversationPublicId?: string
  /**
   * The tab to open on, e.g. from the address. A deployment starting later
   * still takes the focus, but the first one found while the page loads does
   * not override it.
   */
  initialSelected?: string
  /** Told whenever the selection changes, so it can be kept in the address. */
  onSelectedChange?: (key: string) => void
  /** Opens a project from the "made" tab as a tab of its own. */
  onOpenProject?: (project: ProjectRef) => void
  /** Omit when the panel is a fixed part of the layout. */
  onClose?: () => void
  /** Shown in a drawer, which brings its own close button. */
  inDrawer?: boolean
}) {
  const projectTabs = projects.slice(0, MAX_PROJECT_TABS)
  const deploymentKeys = deployments.map(key)
  const newestKey = deploymentKeys[0] ?? ''
  const [selected, setSelected] = useState(
    initialSelected || newestKey || (conversationPublicId ? MADE_TAB : '')
  )
  // The deployment last shown, so returning to the Deployments tab shows it
  // again rather than jumping back to the newest.
  const [pickedDeployment, setPickedDeployment] = useState('')
  const select = (next: string) => {
    setSelected(next)
    if (deploymentKeys.includes(next)) setPickedDeployment(next)
  }
  const choose = (next: string) => {
    const resolved =
      next === DEPLOYMENTS_TAB
        ? deploymentKeys.includes(pickedDeployment)
          ? pickedDeployment
          : newestKey
        : next
    select(resolved)
    onSelectedChange?.(resolved)
  }
  // A deployment started later takes the focus; one the user picked stays put
  // until then. The first deployment seen while a requested tab is waiting is
  // just the page loading, so it must not push that tab aside.
  const lastNewest = useRef('')
  useEffect(() => {
    if (!newestKey) return
    const first = lastNewest.current === ''
    lastNewest.current = newestKey
    if (first && initialSelected) return
    setSelected(newestKey)
    setPickedDeployment(newestKey)
    onSelectedChange?.(newestKey)
    // Only a new deployment should move the selection, not a new callback.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [newestKey])
  const focusKey = focus?.key
  const focusNonce = focus?.nonce
  // A new focus request selects its tab in the same render (asking again for
  // the same tab bumps the nonce), rather than one render late from an effect.
  const focusRequest = focusKey ? `${focusNonce}:${focusKey}` : null
  const [appliedFocus, setAppliedFocus] = useState<string | null>(null)
  if (focusKey && focusRequest !== appliedFocus) {
    setAppliedFocus(focusRequest)
    select(focusKey)
  }
  // Telling the parent is a side effect, so it stays out of render.
  useEffect(() => {
    if (focusKey) onSelectedChange?.(focusKey)
    // Only a new focus request should report, not a new callback.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusKey, focusNonce])

  if (
    deployments.length === 0 &&
    projectTabs.length === 0 &&
    !conversationPublicId
  ) {
    return (
      <aside
        aria-label="Working tabs"
        className="flex h-full min-h-0 min-w-0 flex-col items-center justify-center gap-2 bg-background p-6 text-center"
      >
        <Rocket className="size-7 text-muted-foreground" aria-hidden="true" />
        <p className="text-sm font-medium">
          Nothing running from this chat yet
        </p>
        <p className="max-w-xs text-sm text-muted-foreground">
          When Temps deploys something for you, its build stages and link open
          here. A project named in a reply opens here too. Try &ldquo;Deploy my
          app from a Git repository URL&rdquo;.
        </p>
        {onClose && (
          <Button type="button" variant="outline" size="sm" onClick={onClose}>
            Close
          </Button>
        )}
      </aside>
    )
  }

  const tabs = [
    ...(conversationPublicId ? CHAT_TABS.map((tab) => tab.value) : []),
    ...(deployments.length > 0 ? [DEPLOYMENTS_TAB] : []),
    ...projectTabs.map(projectKey),
  ]
  const { tab: current, deployment: shownKey } = sidebarSelection(selected, {
    tabs,
    deploymentKeys,
    fallback: tabs[0] ?? '',
  })
  const shown =
    deployments.find((deployment) => key(deployment) === shownKey) ??
    deployments[0]
  return (
    <aside
      aria-label="Working tabs"
      className="flex h-full min-h-0 min-w-0 flex-col bg-background"
    >
      <Tabs
        value={current}
        onValueChange={choose}
        className="flex min-h-0 flex-1 flex-col"
      >
        <div
          className={cn(
            'flex items-center gap-1 border-b px-2 py-2',
            inDrawer && 'pr-12'
          )}
        >
          {/* A narrow panel shows the tabs as icons, keeping the selected
              one's label, so the strip fits instead of scrolling. It still
              scrolls when many projects are open. */}
          <TabsList className="@container h-8 min-w-0 flex-1 justify-start overflow-x-auto">
            {conversationPublicId &&
              CHAT_TABS.map(({ value, label, Icon }) => (
                <TabsTrigger
                  key={value}
                  value={value}
                  title={label}
                  icon={Icon}
                  className="group text-xs"
                >
                  <span className={COLLAPSIBLE_LABEL}>{label}</span>
                </TabsTrigger>
              ))}
            {deployments.length > 0 && (
              <TabsTrigger
                value={DEPLOYMENTS_TAB}
                title="Deployments"
                icon={Rocket}
                className="group text-xs"
              >
                <span className={COLLAPSIBLE_LABEL}>Deployments</span>
                {deployments.length > 1 && (
                  <span className="text-[10px] tabular-nums text-muted-foreground">
                    {deployments.length}
                  </span>
                )}
              </TabsTrigger>
            )}
            {projectTabs.map((project) => (
              <TabsTrigger
                key={projectKey(project)}
                value={projectKey(project)}
                title={project.name}
                icon={FolderGit2}
                className="group text-xs"
              >
                <span className={COLLAPSIBLE_LABEL}>{project.name}</span>
              </TabsTrigger>
            ))}
          </TabsList>
          {onClose && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8 shrink-0"
              onClick={onClose}
              aria-label="Close working tabs"
            >
              <X className="size-4" />
            </Button>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {conversationPublicId && (
            <TabsContent value={ARCHITECTURE_TAB} className="mt-0">
              <ArchitectureTab conversationPublicId={conversationPublicId} />
            </TabsContent>
          )}
          {conversationPublicId && (
            <TabsContent value={MADE_TAB} className="mt-0">
              <ChatResourcesTab
                conversationPublicId={conversationPublicId}
                onOpenProject={onOpenProject ?? (() => undefined)}
              />
            </TabsContent>
          )}
          {conversationPublicId && (
            // Kept mounted so switching tabs does not reload the app.
            <TabsContent
              value={PREVIEW_TAB}
              forceMount
              className="mt-0 h-full data-[state=inactive]:hidden"
            >
              <ChatPreviewTab conversationPublicId={conversationPublicId} />
            </TabsContent>
          )}
          {conversationPublicId && (
            // Kept mounted so the open file and the expanded folders stay.
            <TabsContent
              value={FILES_TAB}
              forceMount
              className="mt-0 h-full data-[state=inactive]:hidden"
            >
              <ChatFilesTab
                conversationPublicId={conversationPublicId}
                active={current === FILES_TAB}
              />
            </TabsContent>
          )}
          {conversationPublicId && (
            <TabsContent
              value={CHANGES_TAB}
              forceMount
              className="mt-0 h-full data-[state=inactive]:hidden"
            >
              <ChatChangesTab
                conversationPublicId={conversationPublicId}
                active={current === CHANGES_TAB}
              />
            </TabsContent>
          )}
          {conversationPublicId && (
            // Kept mounted so the shell survives switching tabs.
            <TabsContent
              value={TERMINAL_TAB}
              forceMount
              className="mt-0 h-full data-[state=inactive]:hidden"
            >
              <ChatTerminalTab
                conversationPublicId={conversationPublicId}
                active={current === TERMINAL_TAB}
              />
            </TabsContent>
          )}
          {shown && (
            <TabsContent value={DEPLOYMENTS_TAB} className="mt-0">
              <DeploymentsTab
                deployments={deployments}
                shown={shown}
                onShow={(deployment) => choose(key(deployment))}
              />
            </TabsContent>
          )}
          {projectTabs.map((project) => (
            <TabsContent
              key={projectKey(project)}
              value={projectKey(project)}
              className="mt-0"
            >
              <ProjectTab projectId={project.id} />
            </TabsContent>
          ))}
        </div>
      </Tabs>
    </aside>
  )
}

/**
 * Every deployment the chat watched, newest first, with the one shown in
 * detail below. A single deployment needs no list.
 */
function DeploymentsTab({
  deployments,
  shown,
  onShow,
}: {
  deployments: WatchedDeployment[]
  shown: WatchedDeployment
  onShow: (deployment: WatchedDeployment) => void
}) {
  return (
    <div className="flex min-h-0 flex-col">
      {deployments.length > 1 && (
        <ul
          aria-label="Deployments from this chat"
          className="max-h-48 overflow-y-auto border-b py-1"
        >
          {deployments.map((deployment) => (
            <DeploymentRow
              key={key(deployment)}
              deployment={deployment}
              selected={key(deployment) === key(shown)}
              onSelect={() => onShow(deployment)}
            />
          ))}
        </ul>
      )}
      {/* Keyed so a different deployment starts from its own skeleton. */}
      <DeploymentTab
        key={key(shown)}
        projectId={shown.projectId}
        deploymentId={shown.deploymentId}
      />
    </div>
  )
}

/** `project · #id`, its status, and whether it is the one serving traffic. */
function DeploymentRow({
  deployment,
  selected,
  onSelect,
}: {
  deployment: WatchedDeployment
  selected: boolean
  onSelect: () => void
}) {
  const project = useQuery(
    getProjectOptions({ path: { id: deployment.projectId } })
  )
  const data = useWatchedDeployment(
    deployment.projectId,
    deployment.deploymentId
  ).data
  const name = project.data?.name ?? `Project ${deployment.projectId}`
  const id = data?.id ?? deployment.deploymentId
  const status = data ? statusPresentation(data.status) : null
  const live = data?.is_current === true && isReadyDeploymentStatus(data.status)
  return (
    <li>
      <button
        type="button"
        aria-pressed={selected}
        onClick={onSelect}
        className={cn(
          'flex w-full min-w-0 items-center gap-2 px-4 py-1.5 text-left text-xs transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
          selected && 'bg-muted'
        )}
      >
        <span className="min-w-0 flex-1 truncate font-medium">
          {name}
          {id !== null && (
            <span className="font-normal text-muted-foreground"> · #{id}</span>
          )}
        </span>
        {live && (
          <span className="shrink-0 rounded-sm bg-green-600/10 px-1.5 py-0.5 text-[10px] font-medium text-green-700 dark:text-green-400">
            Live
          </span>
        )}
        {status ? (
          <span className={cn('shrink-0', status.className)}>
            {status.label}
          </span>
        ) : (
          <Skeleton className="h-3 w-12 shrink-0" />
        )}
        {data && (
          <span className="w-16 shrink-0 text-right text-muted-foreground">
            <TimeAgo date={data.started_at ?? data.created_at} />
          </span>
        )}
      </button>
    </li>
  )
}

function key(d: WatchedDeployment): string {
  return `${d.projectId}:${d.deploymentId ?? 'latest'}`
}
