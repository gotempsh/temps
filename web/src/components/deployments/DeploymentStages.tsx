import {
  DeploymentJobResponse,
  DeploymentResponse,
  ProjectResponse,
} from '@/api/client'
import { getDeploymentJobsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
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
  Check,
  CheckCircle2,
  ChevronDownIcon,
  ChevronUpIcon,
  Copy,
  Loader2,
  Settings,
  XCircle,
} from 'lucide-react'
import { memo, useEffect, useMemo, useRef, useState } from 'react'
import { ElapsedTime } from '../global/ElapsedTime'

interface DeploymentStagesProps {
  project: ProjectResponse
  deployment: DeploymentResponse
}

interface LogViewerProps {
  project: ProjectResponse
  deployment: DeploymentResponse
  job: DeploymentJobResponse
}

interface LogEntry {
  level: string
  message: string
  timestamp: string
  line: number
}

function useLogWebSocket(
  project: ProjectResponse,
  deployment: DeploymentResponse,
  job: DeploymentJobResponse
) {
  const [logs, setLogs] = useState<LogEntry[]>([])
  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'connected' | 'error'
  >('connecting')
  const wsRef = useRef<WebSocket | null>(null)

  useEffect(() => {
    if (!project.slug || !deployment.id || !job.job_id) {
      console.error('Missing required parameters for WebSocket connection')
      return
    }

    let reconnectTimeoutId: ReturnType<typeof setTimeout> | null = null
    let isCleaningUp = false

    const connectWS = () => {
      // Don't reconnect if component is unmounting
      if (isCleaningUp) return

      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
      const wsUrl = `${protocol}//${window.location.host}/api/projects/${project.id}/deployments/${deployment.id}/jobs/${job.job_id}/logs/tail`
      setLogs([])

      wsRef.current = new WebSocket(wsUrl)
      setConnectionStatus('connecting')

      wsRef.current.onopen = () => {
        setConnectionStatus('connected')
      }

      wsRef.current.onmessage = (event) => {
        setLogs((prevLogs) => {
          try {
            const data = JSON.parse(event.data) as LogEntry
            // Validate that it's a proper log entry
            if (data.level && data.message && data.line !== undefined) {
              // Trim leading and trailing newlines/carriage returns from the message
              const cleanedMessage = data.message.replace(
                /^[\r\n]+|[\r\n]+$/g,
                ''
              )
              return [
                ...prevLogs,
                {
                  ...data,
                  message: cleanedMessage,
                },
              ]
            }
            // Fallback for old format
            return [
              ...prevLogs,
              {
                level: 'info',
                message: data.message?.replace(/^[\r\n]+|[\r\n]+$/g, '') || '',
                timestamp: new Date().toISOString(),
                line: prevLogs.length + 1,
              },
            ]
          } catch {
            // Fallback for non-JSON messages
            const message =
              typeof event.data === 'string' ? event.data : String(event.data)
            return [
              ...prevLogs,
              {
                level: 'info',
                message: message.replace(/^[\r\n]+|[\r\n]+$/g, ''),
                timestamp: new Date().toISOString(),
                line: prevLogs.length + 1,
              },
            ]
          }
        })
      }

      wsRef.current.onerror = () => {
        setConnectionStatus('error')
      }

      wsRef.current.onclose = (event) => {
        // Only reconnect if:
        // 1. Not a normal closure (code 1000)
        // 2. Component is not being cleaned up
        // 3. Connection was previously established or connecting
        if (!isCleaningUp && event.code !== 1000) {
          setConnectionStatus('error')
          // Retry connection after 10 seconds
          reconnectTimeoutId = setTimeout(connectWS, 10000)
        }
      }
    }

    connectWS()

    return () => {
      isCleaningUp = true
      if (reconnectTimeoutId) {
        clearTimeout(reconnectTimeoutId)
      }
      if (wsRef.current) {
        wsRef.current.close(1000, 'Component unmounting')
      }
    }
  }, [project.id, deployment.id, job.job_id, project.slug])

  return { logs, connectionStatus }
}

function LogViewer({ project, deployment, job }: LogViewerProps) {
  const scrollAreaRef = useRef<HTMLDivElement>(null)
  const { logs, connectionStatus } = useLogWebSocket(project, deployment, job)
  const [searchQuery, setSearchQuery] = useState('')
  const [activeFilters, setActiveFilters] = useState<Set<string>>(new Set())
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    if (logs.length > 0) {
      const viewport = scrollAreaRef.current?.querySelector(
        '[data-radix-scroll-area-viewport]'
      )
      if (viewport) {
        viewport.scrollTop = viewport.scrollHeight
      }
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

  // Get color class for log level
  const getLevelColor = (level: string) => {
    switch (level.toLowerCase()) {
      case 'error':
        return 'text-red-500 dark:text-red-400'
      case 'warning':
      case 'warn':
        return 'text-yellow-600 dark:text-yellow-500'
      case 'success':
        return 'text-green-600 dark:text-green-500'
      case 'info':
        return 'text-blue-500 dark:text-blue-400'
      default:
        return 'text-muted-foreground'
    }
  }

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
            <span className="text-blue-500">ℹ️</span>
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
            <span className="text-green-500">✅</span>
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
            <span className="text-yellow-500">⚠️</span>
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
            <span className="text-red-500">❌</span>
            Error
            {logCounts.error > 0 && (
              <span className="ml-1 px-1.5 py-0.5 text-xs rounded-full bg-red-500/20">
                {logCounts.error}
              </span>
            )}
          </Button>
        </div>
      </div>

      {/* Log Viewer */}
      <div className="relative group">
        {/* Copy Button - CodeBlock Style */}
        <Button
          size="sm"
          variant="ghost"
          className="absolute top-2 right-2 z-10 h-7 px-2 bg-background/80 dark:bg-zinc-800/50 hover:bg-background dark:hover:bg-zinc-800 text-muted-foreground hover:text-foreground opacity-0 group-hover:opacity-100 transition-all duration-200 backdrop-blur-sm"
          onClick={async () => {
            await navigator.clipboard.writeText(plainTextLogs)
            setCopied(true)
            setTimeout(() => setCopied(false), 2000)
          }}
          disabled={logs.length === 0 || connectionStatus === 'connecting'}
        >
          {copied ? (
            <>
              <Check className="h-3 w-3 mr-1" />
              <span className="text-xs">Copied</span>
            </>
          ) : (
            <>
              <Copy className="h-3 w-3 mr-1" />
              <span className="text-xs">Copy</span>
            </>
          )}
        </Button>

        <ScrollArea
          ref={scrollAreaRef}
          className={`h-96 border rounded-md bg-background overflow-auto ${connectionStatus === 'connecting' ? 'opacity-50' : 'opacity-100'}`}
        >
          <div className="text-xs font-mono p-4">
            {logs.length === 0 ? (
              <div className="text-muted-foreground">
                Connecting to log stream...
              </div>
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
                    className="whitespace-pre-wrap break-words flex-1 min-w-0"
                    dangerouslySetInnerHTML={{
                      __html: ansiConverter.toHtml(log.message),
                    }}
                  />
                  <span className="text-muted-foreground/40 text-[10px] whitespace-nowrap">
                    {formatTimestamp(log.timestamp)}
                  </span>
                </div>
              ))
            )}
          </div>
        </ScrollArea>
      </div>
    </div>
  )
}

// First, let's memoize the LogViewer component
const MemoizedLogViewer = memo(LogViewer)

// Config Modal Component
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
      job_config: stage.job_config,
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
          <DialogTitle>Stage Configuration</DialogTitle>
          <DialogDescription>
            Configuration details for{' '}
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

  const expandAll = () => {
    setManualOverrides(new Map())
    if (stagesQuery.data) {
      const allExpanded = new Map<number, boolean>()
      stagesQuery.data.jobs.forEach((stage) => {
        allExpanded.set(stage.id, true)
      })
      setManualOverrides(allExpanded)
    }
  }

  const collapseAll = () => {
    setManualOverrides(new Map())
    if (stagesQuery.data) {
      const allCollapsed = new Map<number, boolean>()
      stagesQuery.data.jobs.forEach((stage) => {
        allCollapsed.set(stage.id, false)
      })
      setManualOverrides(allCollapsed)
    }
  }

  const getStatusIcon = (status: string) => {
    switch (status) {
      case 'success':
        return <CheckCircle2 className="h-5 w-5 text-green-500" />
      case 'failure':
        return <XCircle className="h-5 w-5 text-red-500" />
      case 'running':
        return <Loader2 className="h-5 w-5 text-orange-500 animate-spin" />
      case 'pending':
        return <Loader2 className="h-5 w-5 text-muted-foreground" />
      case 'cancelled':
        return <XCircle className="h-5 w-5 text-muted-foreground" />
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
      <div className="space-y-4 px-2 sm:px-0">
        {stagesQuery.data?.jobs.map((stage) => (
          <div
            key={stage.id}
            className="border rounded-lg overflow-hidden bg-card"
          >
            {/* Fat Header */}
            <div className="flex items-center justify-between px-6 py-4 bg-muted/30 hover:bg-muted/50 transition-colors">
              <div
                className="flex items-center gap-3 flex-1 cursor-pointer"
                onClick={() => toggleStage(stage.id)}
              >
                {getStatusIcon(stage.status)}
                <div className="flex items-center gap-3">
                  <h3 className="font-medium text-base">
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
              <div className="flex items-center gap-3">
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
                  title="View stage configuration"
                >
                  <Settings className="h-4 w-4" />
                </Button>
                <button
                  onClick={() => toggleStage(stage.id)}
                  className="cursor-pointer"
                >
                  {expandedStageIds.has(stage.id) ? (
                    <ChevronUpIcon className="h-5 w-5 text-muted-foreground" />
                  ) : (
                    <ChevronDownIcon className="h-5 w-5 text-muted-foreground" />
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

      {/* Config Modal */}
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
