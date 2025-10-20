import { getDeploymentJobsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { CopyButton } from '@/components/ui/copy-button'
import { useQuery } from '@tanstack/react-query'
import { ChevronDownIcon } from 'lucide-react'
import { useEffect, useRef, useState, memo, useMemo } from 'react'
import { ElapsedTime } from '../global/ElapsedTime'
import { StatusIndicator } from './StatusIndicator'
import {
  DeploymentJobResponse,
  DeploymentResponse,
  ProjectResponse,
} from '@/api/client'
import AnsiToHtml from 'ansi-to-html'

interface DeploymentStagesProps {
  project: ProjectResponse
  deployment: DeploymentResponse
}

interface LogViewerProps {
  project: ProjectResponse
  deployment: DeploymentResponse
  job: DeploymentJobResponse
}

function useLogSSE(
  project: ProjectResponse,
  deployment: DeploymentResponse,
  job: DeploymentJobResponse
) {
  const [logs, setLogs] = useState<string>('')
  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'connected' | 'error'
  >('connecting')
  const eventSourceRef = useRef<EventSource | null>(null)

  useEffect(() => {
    if (!project.slug || !deployment.id || !job.job_id) {
      console.error('Missing required parameters for SSE connection')
      return
    }

    const connectSSE = () => {
      const sseUrl = `/api/projects/${project.id}/deployments/${deployment.id}/jobs/${job.job_id}/logs/tail`
      setLogs('')

      eventSourceRef.current = new EventSource(sseUrl)
      setConnectionStatus('connecting')

      eventSourceRef.current.onopen = () => {
        setConnectionStatus('connected')
      }

      eventSourceRef.current.onmessage = (event) => {
        setLogs((prevLogs) => {
          try {
            const data = JSON.parse(event.data)
            if (data.log) {
              return prevLogs + data.log + '\n'
            }
            return prevLogs + event.data + '\n'
          } catch (_error) {
            return prevLogs + event.data + '\n'
          }
        })
      }

      eventSourceRef.current.onerror = () => {
        setConnectionStatus('error')
        eventSourceRef.current?.close()
        // Retry connection after 10 seconds
        setTimeout(connectSSE, 10000)
      }
    }

    connectSSE()

    return () => {
      if (eventSourceRef.current) {
        eventSourceRef.current.close()
      }
    }
  }, [project.id, deployment.id, job.job_id])

  return { logs, connectionStatus }
}

function LogViewer({ project, deployment, job }: LogViewerProps) {
  const scrollAreaRef = useRef<HTMLDivElement>(null)
  const { logs, connectionStatus } = useLogSSE(project, deployment, job)

  useEffect(() => {
    if (logs) {
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

  // Convert ANSI codes to HTML
  const htmlLogs = useMemo(() => {
    if (!logs) return ''
    return ansiConverter.toHtml(logs)
  }, [logs, ansiConverter])

  return (
    <div className="space-y-2">
      {/* {connectionStatus === 'error' && (
				<Alert variant="destructive">
					<AlertCircle className="h-4 w-4" />
					<AlertDescription>Connection lost. Attempting to reconnect...</AlertDescription>
				</Alert>
			)} */}
      <div className="relative">
        <CopyButton
          value={logs}
          className="absolute top-2 right-2 z-10 h-8 w-8 rounded-md p-0"
          disabled={!logs || connectionStatus === 'connecting'}
        />
        <ScrollArea
          ref={scrollAreaRef}
          className={`h-64 border rounded-md bg-muted overflow-auto ${connectionStatus === 'connecting' ? 'opacity-50' : 'opacity-100'}`}
        >
          <pre
            className="text-xs whitespace-pre-wrap font-mono p-4 max-w-[calc(100vw-4rem)]"
            dangerouslySetInnerHTML={{
              __html: htmlLogs || 'Connecting to log stream...',
            }}
          />
        </ScrollArea>
      </div>
    </div>
  )
}

// First, let's memoize the LogViewer component
const MemoizedLogViewer = memo(LogViewer)

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
    refetchInterval:
      deployment.status === 'completed' ||
        deployment.status === 'failed' ||
        deployment.status === 'cancelled'
        ? false
        : 2500,
  })

  // Change to Set to track multiple expanded stages
  const [expandedStageIds, setExpandedStageIds] = useState<Set<number>>(
    new Set()
  )

  // Update expanded stages when query data changes
  useEffect(() => {
    if (stagesQuery.data) {
      setExpandedStageIds((prev) => {
        const newSet = new Set(prev)
        stagesQuery.data.jobs.forEach((stage) => {
          // Auto-expand stages that are completed or pending
          if (
            stage.status === 'running' ||
            stage.status === 'pending' ||
            stage.status === 'failure' ||
            stage.status === 'cancelled'
          ) {
            newSet.add(stage.id)
          }
        })
        return newSet
      })
    }
  }, [stagesQuery.data])

  const toggleStage = (stageId: number) => {
    setExpandedStageIds((prev) => {
      const newSet = new Set(prev)
      if (newSet.has(stageId)) {
        newSet.delete(stageId)
      } else {
        newSet.add(stageId)
      }
      return newSet
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

  return (
    <div className="space-y-4 px-2 sm:px-0">
      {stagesQuery.data?.jobs.map((stage) => (
        <div
          key={stage.id}
          className="border rounded-lg p-4 flex flex-col space-y-2"
        >
          <div
            className="flex flex-col sm:flex-row sm:items-center sm:justify-between w-full cursor-pointer"
            onClick={() => toggleStage(stage.id)}
          >
            <div className="flex items-center">
              <ChevronDownIcon
                className={`h-4 w-4 mr-2 transition-transform ${expandedStageIds.has(stage.id) ? 'rotate-180' : ''}`}
              />
              <span className="font-medium">{stage.name}</span>
            </div>
            <div className="flex items-center space-x-2">
              <ElapsedTime
                startedAt={stage.started_at!}
                endedAt={stage.finished_at!}
              />
              <StatusIndicator
                status={
                  stage.status as
                  | 'success'
                  | 'failure'
                  | 'running'
                  | 'pending'
                  | 'cancelled'
                }
              />
            </div>
          </div>

          {expandedStageIds.has(stage.id) && (
            <MemoizedLogViewer
              key={`${stage.id}-${deployment.id}`}
              project={project}
              deployment={deployment}
              job={stage}
            />
          )}
        </div>
      ))}
    </div>
  )
}
