// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { missingLogLines } from './job-log-state'

import {
  DeploymentJobResponse,
  DeploymentResponse,
  ProjectResponse,
} from '@/api/client'
import { getDeploymentJobsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import AnsiToHtml from 'ansi-to-html'
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDownIcon,
  ChevronUpIcon,
  Info,
  Loader2,
  Settings,
  Sparkles,
  XCircle,
} from 'lucide-react'
import { memo, useEffect, useMemo, useRef, useState } from 'react'
import { useAiAssistant } from '../ai/AiAssistantContext'
import { ElapsedTime } from '../global/ElapsedTime'
import {
  JobLogNoticeBar,
  JobLogPlaceholder,
  JobLogTruncationNote,
} from './JobLogStatus'
import { useDeploymentJobLogs } from './useDeploymentJobLogs'

interface DeploymentStagesProps {
  project: ProjectResponse
  deployment: DeploymentResponse
}

interface LogViewerProps {
  project: ProjectResponse
  deployment: DeploymentResponse
  job: DeploymentJobResponse
}

function LogViewer({ project, deployment, job }: LogViewerProps) {
  const scrollAreaRef = useRef<HTMLDivElement>(null)
  const {
    entries: logs,
    view,
    problemDetail,
    retry,
  } = useDeploymentJobLogs({
    projectId: project.id,
    deploymentId: deployment.id,
    jobId: job.job_id,
    jobStatus: job.status,
  })
  const [searchQuery, setSearchQuery] = useState('')
  const [activeFilters, setActiveFilters] = useState<Set<string>>(new Set())

  useEffect(() => {
    if (logs.length > 0 && scrollAreaRef.current) {
      // Native scroll container (not Radix) so mobile gets two-axis touch
      // scrolling; keep pinned to the latest line.
      scrollAreaRef.current.scrollTop = scrollAreaRef.current.scrollHeight
    }
  }, [logs])

  // Create ansi converter instance
  const ansiConverter = useMemo(
    () =>
      new AnsiToHtml({
        fg: 'var(--foreground)',
        bg: 'var(--muted)',
        newline: true,
        escapeXML: true,
      }),
    []
  )

  // Get icon for log level
  const getLevelIcon = (level: string) => {
    switch (level.toLowerCase()) {
      case 'error':
        return '●'
      case 'warning':
      case 'warn':
        return '●'
      case 'success':
        return '●'
      case 'info':
        return '●'
      default:
        return '●'
    }
  }

  // Get color for log level icon
  const getLevelIconColor = (level: string) => {
    switch (level.toLowerCase()) {
      case 'error':
        return 'text-red-500'
      case 'warning':
      case 'warn':
        return 'text-yellow-500'
      case 'success':
        return 'text-green-500'
      case 'info':
        return 'text-blue-500'
      default:
        return 'text-muted-foreground'
    }
  }

  // Format timestamp to show time with milliseconds
  const formatTimestamp = (timestamp: string) => {
    try {
      const date = new Date(timestamp)
      const hours = date.getHours().toString().padStart(2, '0')
      const minutes = date.getMinutes().toString().padStart(2, '0')
      const seconds = date.getSeconds().toString().padStart(2, '0')
      const milliseconds = date.getMilliseconds().toString().padStart(3, '0')
      return `${hours}:${minutes}:${seconds}.${milliseconds}`
    } catch {
      return timestamp
    }
  }

  // Calculate log counts by level
  const logCounts = useMemo(() => {
    const counts = {
      info: 0,
      success: 0,
      warning: 0,
      error: 0,
    }
    logs.forEach((log) => {
      const level = log.level.toLowerCase()
      if (level === 'info') counts.info++
      else if (level === 'success') counts.success++
      else if (level === 'warning' || level === 'warn') counts.warning++
      else if (level === 'error') counts.error++
    })
    return counts
  }, [logs])

  // Filter logs based on active filters and search query
  const filteredLogs = useMemo(() => {
    let filtered = logs

    // Apply level filters
    if (activeFilters.size > 0) {
      filtered = filtered.filter((log) => {
        const level = log.level.toLowerCase()
        const normalizedLevel = level === 'warn' ? 'warning' : level
        return activeFilters.has(normalizedLevel)
      })
    }

    // Apply search filter
    if (searchQuery.trim()) {
      const query = searchQuery.toLowerCase()
      filtered = filtered.filter((log) =>
        log.message.toLowerCase().includes(query)
      )
    }

    return filtered
  }, [logs, activeFilters, searchQuery])

  // Convert plain text logs to copyable string
  const plainTextLogs = useMemo(() => {
    return logs
      .map(
        (log) =>
          `[${log.timestamp}] [${log.level.toUpperCase()}] ${log.message}`
      )
      .join('\n')
  }, [logs])

  const toggleFilter = (level: string) => {
    setActiveFilters((prev) => {
      const newFilters = new Set(prev)
      if (newFilters.has(level)) {
        newFilters.delete(level)
      } else {
        newFilters.add(level)
      }
      return newFilters
    })
  }

  return (
    <div className="space-y-3">
      {/* Search and Filter Bar */}
      <div className="flex flex-col sm:flex-row gap-3">
        {/* Search Input */}
        <div className="relative flex-1">
          <input
            type="text"
            placeholder="Search logs"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full h-9 px-3 py-2 text-sm bg-background border border-input rounded-md focus:outline-none focus:ring-2 focus:ring-ring"
          />
        </div>

        {/* Filter Buttons */}
        <div className="flex gap-2 flex-wrap">
          <Button
            variant={activeFilters.has('info') ? 'default' : 'outline'}
            size="sm"
            onClick={() => toggleFilter('info')}
            className="gap-2"
          >
            <Info className="text-blue-500" />
            Info
            {logCounts.info > 0 && (
              <span className="ml-1 px-1.5 py-0.5 text-xs rounded-full bg-blue-500/20">
                {logCounts.info}
              </span>
            )}
          </Button>
          <Button
            variant={activeFilters.has('success') ? 'default' : 'outline'}
            size="sm"
            onClick={() => toggleFilter('success')}
            className="gap-2"
          >
            <CheckCircle2 className="text-green-500" />
            Success
            {logCounts.success > 0 && (
              <span className="ml-1 px-1.5 py-0.5 text-xs rounded-full bg-green-500/20">
                {logCounts.success}
              </span>
            )}
          </Button>
          <Button
            variant={activeFilters.has('warning') ? 'default' : 'outline'}
            size="sm"
            onClick={() => toggleFilter('warning')}
            className="gap-2"
          >
            <AlertTriangle className="text-yellow-500" />
            Warning
            {logCounts.warning > 0 && (
              <span className="ml-1 px-1.5 py-0.5 text-xs rounded-full bg-yellow-500/20">
                {logCounts.warning}
              </span>
            )}
          </Button>
          <Button
            variant={activeFilters.has('error') ? 'default' : 'outline'}
            size="sm"
            onClick={() => toggleFilter('error')}
            className="gap-2"
          >
            <XCircle className="text-red-500" />
            Error
            {logCounts.error > 0 && (
              <span className="ml-1 px-1.5 py-0.5 text-xs rounded-full bg-red-500/20">
                {logCounts.error}
              </span>
            )}
          </Button>
        </div>
      </div>

      <JobLogNoticeBar notice={view.notice} onRetry={retry} />
      <JobLogTruncationNote
        firstLine={logs[0]?.line}
        missingLines={missingLogLines(logs)}
      />

      {/* Log Viewer */}
      <div className="relative group">
        <CopyButton
          value={plainTextLogs}
          label="Copy logs"
          disabled={logs.length === 0}
          className="absolute top-2 right-2 z-10 h-7 gap-1 rounded-md px-2 bg-background/80 text-muted-foreground opacity-0 backdrop-blur-sm group-hover:opacity-100 focus-visible:opacity-100 dark:bg-zinc-800/50 [&_svg]:h-3 [&_svg]:w-3"
        >
          <span className="text-xs">Copy</span>
        </CopyButton>

        {/* Native scroll container (not Radix ScrollArea) — Radix only wires
            up a vertical scrollbar and its display:table viewport blocks
            horizontal touch scrolling on mobile, so long log lines got
            clipped with no way to reach them. A plain overflow-auto div gives
            reliable two-axis touch scrolling. */}
        <div
          ref={scrollAreaRef}
          className="h-96 overflow-auto border rounded-md bg-background overscroll-contain"
        >
          {/* w-max + min-w-full lets rows grow to the longest line so it can be
              scrolled to horizontally, while never shrinking below the viewport. */}
          <div className="text-xs font-mono p-4 w-max min-w-full">
            {view.body !== 'lines' ? (
              <JobLogPlaceholder
                body={view.body}
                jobStatus={job.status}
                detail={problemDetail}
                onRetry={retry}
              />
            ) : filteredLogs.length === 0 ? (
              <div className="text-muted-foreground">
                No logs match the current filters
              </div>
            ) : (
              filteredLogs.map((log) => (
                <div
                  key={log.line}
                  className="flex gap-2 items-start hover:bg-muted/50 leading-relaxed"
                >
                  <span className="text-muted-foreground/50 select-none min-w-[3ch] text-right shrink-0">
                    {log.line}
                  </span>
                  <span
                    className={`min-w-[1ch] shrink-0 ${getLevelIconColor(log.level)}`}
                  >
                    {getLevelIcon(log.level)}
                  </span>
                  <span
                    className="whitespace-pre flex-1"
                    dangerouslySetInnerHTML={{
                      __html: ansiConverter.toHtml(log.message),
                    }}
                  />
                  <span className="text-muted-foreground/40 text-[10px] whitespace-nowrap shrink-0">
                    {formatTimestamp(log.timestamp)}
                  </span>
                </div>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

// First, let's memoize the LogViewer component
const MemoizedLogViewer = memo(LogViewer)

// Stage details modal
interface ConfigModalProps {
  isOpen: boolean
  onClose: () => void
  stage: DeploymentJobResponse
}

function ConfigModal({ isOpen, onClose, stage }: ConfigModalProps) {
  const configJson = useMemo(() => {
    const config = {
      id: stage.id,
      name: stage.name,
      description: stage.description,
      job_type: stage.job_type,
      job_id: stage.job_id,
      status: stage.status,
      execution_order: stage.execution_order,
      dependencies: stage.dependencies,
      outputs: stage.outputs,
      started_at: stage.started_at,
      finished_at: stage.finished_at,
      error_message: stage.error_message,
    }
    return JSON.stringify(config, null, 2)
  }, [stage])

  return (
    <Dialog open={isOpen} onOpenChange={onClose}>
      <DialogContent className="max-w-3xl max-h-[80vh] flex flex-col gap-4 p-6">
        <DialogHeader>
          <DialogTitle>Stage Details</DialogTitle>
          <DialogDescription>
            Execution details for{' '}
            <span className="font-mono">{stage.name}</span>
          </DialogDescription>
        </DialogHeader>
        <ScrollArea className="flex-1 h-full overflow-auto">
          <CodeBlock
            code={configJson}
            language="json"
            showCopy={true}
            defaultWrap={true}
            disableWrapToggle={true}
          />
        </ScrollArea>
      </DialogContent>
    </Dialog>
  )
}

export function DeploymentStages({
  project,
  deployment,
}: DeploymentStagesProps) {
  const stagesQuery = useQuery({
    ...getDeploymentJobsOptions({
      path: {
        project_id: project.id,
        deployment_id: deployment.id,
      },
    }),
    refetchInterval: (query) => {
      // Continue polling if any job is still pending or running
      const jobs = query.state.data?.jobs
      if (!jobs) return 2500

      const hasActiveJobs = jobs.some(
        (job) => job.status === 'pending' || job.status === 'running'
      )

      // Stop polling only when all jobs are in terminal states (success, failure, cancelled)
      return hasActiveJobs ? 2500 : false
    },
  })

  // Track user's manual toggle overrides (true = force expanded, false = force collapsed)
  const [manualOverrides, setManualOverrides] = useState<Map<number, boolean>>(
    new Map()
  )

  // Track which stage's config modal is open
  const [configModalStage, setConfigModalStage] =
    useState<DeploymentJobResponse | null>(null)

  // AI debugging chat (ADR-023), opened from a failed stage into the persistent
  // app-level dock. The chat is scoped to the whole deployment.
  const { open: openAiAssistant } = useAiAssistant()
  const debugWithAi = () => {
    openAiAssistant({
      projectId: project.id,
      context: {
        contextType: 'deployment',
        contextId: deployment.id,
        title: `Debug deployment #${deployment.id}`,
        description:
          "AI reads this deployment's failed stages and build logs to explain what went wrong. Ask follow-up questions to dig deeper.",
        startPrompt:
          'Diagnose this deployment failure and suggest concrete fixes.',
        projectSlug: project.slug,
        projectName: project.name,
      },
    })
  }

  // Compute which stages should be expanded based on their status and manual overrides
  const expandedStageIds = useMemo(() => {
    if (!stagesQuery.data) return new Set<number>()

    const result = new Set<number>()

    // Find the last failed stage (highest execution_order with failure status)
    const failedStages = stagesQuery.data.jobs.filter(
      (stage) => stage.status === 'failure'
    )
    const lastFailedStage = failedStages.sort(
      (a, b) => (b.execution_order || 0) - (a.execution_order || 0)
    )[0]

    stagesQuery.data.jobs.forEach((stage) => {
      // Check if user has manually overridden this stage
      const manualOverride = manualOverrides.get(stage.id)

      if (manualOverride !== undefined) {
        // User has manually toggled - respect their choice
        if (manualOverride) {
          result.add(stage.id)
        }
      } else {
        // Auto-expand stages that are running or the last failed stage
        // Success, cancelled, and pending stages are collapsed by default
        if (
          stage.status === 'running' ||
          (stage.status === 'failure' && stage.id === lastFailedStage?.id)
        ) {
          result.add(stage.id)
        }
      }
    })

    return result
  }, [stagesQuery.data, manualOverrides])

  const toggleStage = (stageId: number) => {
    setManualOverrides((prev) => {
      const newMap = new Map(prev)
      const isCurrentlyExpanded = expandedStageIds.has(stageId)
      // Toggle: if expanded, collapse; if collapsed, expand
      newMap.set(stageId, !isCurrentlyExpanded)
      return newMap
    })
  }

  if (stagesQuery.isLoading) {
    return <Skeleton className="w-full h-48" />
  }

  if (stagesQuery.isError) {
    return (
      <div className="p-4">
        Error loading deployment stages: {stagesQuery.error.message}
      </div>
    )
  }

  const getStatusIcon = (status: string) => {
    switch (status) {
      case 'success':
        return <CheckCircle2 className="h-4 w-4 text-green-500" />
      case 'failure':
        return <XCircle className="h-4 w-4 text-red-500" />
      case 'running':
        return <Loader2 className="h-4 w-4 text-orange-500 animate-spin" />
      case 'pending':
        return <Loader2 className="h-4 w-4 text-muted-foreground" />
      case 'cancelled':
        return <XCircle className="h-4 w-4 text-muted-foreground" />
      default:
        return null
    }
  }

  const getStatusBadge = (status: string) => {
    switch (status) {
      case 'success':
        return null
      case 'failure':
        return (
          <Badge variant="destructive" className="capitalize">
            Failed
          </Badge>
        )
      case 'running':
        return (
          <Badge
            variant="secondary"
            className="capitalize bg-orange-500/10 text-orange-600 border-orange-500/20"
          >
            In Progress
          </Badge>
        )
      case 'pending':
        return (
          <Badge variant="outline" className="capitalize">
            Pending
          </Badge>
        )
      case 'cancelled':
        return (
          <Badge variant="outline" className="capitalize">
            Cancelled
          </Badge>
        )
      default:
        return null
    }
  }

  return (
    <div className="space-y-4">
      {/* Stages */}
      <div className="space-y-2">
        {stagesQuery.data?.jobs.map((stage) => (
          <div
            key={stage.id}
            className="border rounded-lg overflow-hidden bg-card"
          >
            {/* Header */}
            <div className="flex items-center justify-between px-4 py-2.5 bg-muted/30 hover:bg-muted/50 transition-colors">
              <div
                className="flex min-w-0 items-center gap-2.5 flex-1 cursor-pointer"
                onClick={() => toggleStage(stage.id)}
              >
                {getStatusIcon(stage.status)}
                <div className="flex min-w-0 items-center gap-3">
                  <h3 className="font-medium text-sm">
                    {stage.name}
                    {stage.description && (
                      <span className="ml-2 font-normal text-sm text-muted-foreground">
                        {stage.description}
                      </span>
                    )}
                  </h3>
                  {getStatusBadge(stage.status)}
                </div>
              </div>
              <div className="flex items-center gap-2 sm:gap-3">
                {stage.status === 'failure' && (
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-8 gap-1.5 border-primary/30 text-primary hover:bg-primary/10 hover:text-primary"
                    onClick={(e) => {
                      e.stopPropagation()
                      debugWithAi()
                    }}
                    title="Debug this failure with AI"
                  >
                    <Sparkles className="h-4 w-4" />
                    <span className="hidden sm:inline">Debug with AI</span>
                  </Button>
                )}
                <ElapsedTime
                  startedAt={stage.started_at!}
                  endedAt={stage.finished_at!}
                />
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-8 w-8 p-0"
                  onClick={(e) => {
                    e.stopPropagation()
                    setConfigModalStage(stage)
                  }}
                  title="View stage details"
                >
                  <Settings className="h-4 w-4" />
                </Button>
                <button
                  onClick={() => toggleStage(stage.id)}
                  className="cursor-pointer"
                >
                  {expandedStageIds.has(stage.id) ? (
                    <ChevronUpIcon className="h-4 w-4 text-muted-foreground" />
                  ) : (
                    <ChevronDownIcon className="h-4 w-4 text-muted-foreground" />
                  )}
                </button>
              </div>
            </div>

            {expandedStageIds.has(stage.id) && (
              <div className="p-4">
                <MemoizedLogViewer
                  key={`${stage.id}-${deployment.id}`}
                  project={project}
                  deployment={deployment}
                  job={stage}
                />
              </div>
            )}
          </div>
        ))}
      </div>

      {/* Stage details modal */}
      {configModalStage && (
        <ConfigModal
          isOpen={!!configModalStage}
          onClose={() => setConfigModalStage(null)}
          stage={configModalStage}
        />
      )}
    </div>
  )
}
