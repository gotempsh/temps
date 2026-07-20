import { ContainerInfoResponse, ProjectResponse } from '@/api/client'
import {
  containerMetricsGetHistoryOptions,
  getContainerMetricsOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { MetricSparkline } from '@/components/charts/metric-sparkline'
import { formatCpuUsage } from '@/lib/cpu-format'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useQuery } from '@tanstack/react-query'
import {
  Box,
  ChevronRight,
  Cpu,
  ExternalLink,
  HardDrive,
  MoreHorizontal,
  Play,
  RotateCw,
  ServerIcon,
  Square,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'

interface ContainerListProps {
  project: ProjectResponse
  environmentId: string
  onAction?: (containerId: string, action: 'start' | 'stop' | 'restart') => void
}

export function ContainerList({
  project,
  environmentId,
  onAction,
}: ContainerListProps) {
  const navigate = useNavigate()

  const { data, isLoading } = useQuery({
    ...listContainersOptions({
      path: {
        project_id: project.id,
        environment_id: parseInt(environmentId),
      },
    }),
    staleTime: 5000,
  })

  const containers = data?.containers ?? []

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-48">
        <p className="text-sm text-muted-foreground">Loading containers...</p>
      </div>
    )
  }

  if (containers.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-72 rounded-lg border border-neutral-950/10 bg-neutral-50 p-6 dark:border-white/10 dark:bg-white/5">
        <div className="text-center space-y-1">
          <p className="text-sm font-semibold text-neutral-900 dark:text-white">
            No containers yet
          </p>
          <p className="text-sm text-neutral-600 dark:text-neutral-400">
            This environment doesn&apos;t have any running containers
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="rounded-lg border border-neutral-950/10 bg-white divide-y divide-neutral-950/10 dark:border-white/10 dark:bg-neutral-900/40 dark:divide-white/10 overflow-hidden">
      {containers.map((container) => (
        <ContainerRow
          key={container.container_id}
          project={project}
          environmentId={environmentId}
          container={container}
          onClick={() =>
            navigate(
              `/projects/${project.slug}/environments/containers/${container.container_id}?env=${environmentId}`
            )
          }
          onAction={(action) => onAction?.(container.container_id, action)}
        />
      ))}
    </div>
  )
}

interface ContainerRowProps {
  project: ProjectResponse
  environmentId: string
  container: ContainerInfoResponse
  onClick: () => void
  onAction?: (action: 'start' | 'stop' | 'restart') => void
}

function ContainerRow({
  project,
  environmentId,
  container,
  onClick,
  onAction,
}: ContainerRowProps) {
  const running = container.status === 'running'
  const errored = container.status === 'error'
  const statusText = errored
    ? 'Error'
    : running
      ? 'Running'
      : container.status || 'Stopped'
  const statusTone = errored
    ? 'bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-500/10 dark:text-red-400 dark:ring-red-500/30'
    : running
      ? 'bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-500/10 dark:text-emerald-400 dark:ring-emerald-500/30'
      : 'bg-neutral-100 text-neutral-700 ring-neutral-950/10 dark:bg-white/5 dark:text-neutral-300 dark:ring-white/10'

  const { data: metrics } = useQuery({
    ...getContainerMetricsOptions({
      path: {
        project_id: project.id,
        environment_id: parseInt(environmentId),
        container_id: container.container_id,
      },
    }),
    enabled: running,
    refetchInterval: running ? 5000 : false,
    staleTime: 4000,
  })

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          onClick()
        }
      }}
      className="group flex items-center gap-4 px-4 py-4 cursor-pointer hover:bg-neutral-50 dark:hover:bg-white/5 transition-colors"
    >
      <div className="shrink-0 flex size-10 items-center justify-center rounded-md bg-neutral-100 text-neutral-600 dark:bg-white/5 dark:text-neutral-300">
        <Box className="size-5" aria-hidden="true" />
      </div>

      <div className="flex-1 min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <span className="truncate font-semibold text-neutral-900 dark:text-white">
            {container.service_name || container.container_name}
          </span>
          <span
            className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset ${statusTone}`}
          >
            <span
              className={`size-1.5 rounded-full ${errored ? 'bg-red-500' : running ? 'bg-emerald-500' : 'bg-neutral-400'} ${running ? 'animate-pulse' : ''}`}
              aria-hidden="true"
            />
            {statusText}
          </span>
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-neutral-500 dark:text-neutral-400">
          <code className="font-mono truncate max-w-[24rem]">
            {container.image_name}
          </code>
          {container.node_name && (
            <span className="inline-flex items-center gap-1">
              <ServerIcon className="size-3" aria-hidden="true" />
              {container.node_name}
            </span>
          )}
          {container.created_at && (
            <UptimeInline createdAt={container.created_at} />
          )}
          {running && metrics && (
            <>
              <span className="inline-flex items-center gap-1 tabular-nums">
                <Cpu className="size-3" aria-hidden="true" />
                {formatCpuUsage(metrics.cpu_percent, metrics.cpu_limit_cores)}
              </span>
              <span className="inline-flex items-center gap-1 tabular-nums">
                <HardDrive className="size-3" aria-hidden="true" />
                {formatBytes(metrics.memory_bytes)}
                {(metrics.memory_limit_bytes ?? 0) > 0 &&
                  metrics.memory_percent != null && (
                    <span className="text-neutral-400 dark:text-neutral-500">
                      {' '}
                      / {metrics.memory_percent.toFixed(0)}%
                    </span>
                  )}
              </span>
            </>
          )}
        </div>
      </div>

      {running && (
        <>
          <HistorySparkline
            project={project}
            environmentId={environmentId}
            containerId={container.container_id}
            metric="container.cpu_percent"
            label="CPU"
            format={(v) => `${v.toFixed(1)}%`}
          />
          <HistorySparkline
            project={project}
            environmentId={environmentId}
            containerId={container.container_id}
            metric="container.memory_used_bytes"
            label="Mem"
            format={formatBytes}
          />
        </>
      )}

      <div
        className="flex items-center gap-1 shrink-0"
        onClick={(e) => e.stopPropagation()}
      >
        {container.service_url && (
          <a
            href={container.service_url}
            target="_blank"
            rel="noopener noreferrer"
            className="hidden md:inline-flex items-center gap-1.5 rounded-md border border-neutral-950/10 bg-white px-2.5 py-1.5 text-xs font-medium text-neutral-700 hover:bg-neutral-50 dark:border-white/10 dark:bg-white/5 dark:text-neutral-200 dark:hover:bg-white/10"
          >
            <ExternalLink className="size-3.5" aria-hidden="true" />
            Visit
          </a>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              aria-label="Container actions"
              className="inline-flex size-8 items-center justify-center rounded-md text-neutral-500 hover:bg-neutral-100 hover:text-neutral-900 dark:text-neutral-400 dark:hover:bg-white/10 dark:hover:text-white"
            >
              <MoreHorizontal className="size-4" aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-44">
            {running ? (
              <>
                <DropdownMenuItem onSelect={() => onAction?.('restart')}>
                  <RotateCw className="mr-2 size-4" aria-hidden="true" />
                  Restart
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  onSelect={() => onAction?.('stop')}
                  className="text-red-600 focus:text-red-600 dark:text-red-400 dark:focus:text-red-400"
                >
                  <Square className="mr-2 size-4" aria-hidden="true" />
                  Stop
                </DropdownMenuItem>
              </>
            ) : (
              <DropdownMenuItem onSelect={() => onAction?.('start')}>
                <Play className="mr-2 size-4" aria-hidden="true" />
                Start
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
        <ChevronRight
          className="size-4 text-neutral-400 group-hover:text-neutral-600 dark:group-hover:text-neutral-300"
          aria-hidden="true"
        />
      </div>
    </div>
  )
}

/**
 * Compact 1h sparkline + current value for one container resource metric
 * (history recorded every ~30s by the container health monitor). Renders
 * nothing when there's no history yet (metrics store disabled, container
 * just started) so rows stay clean.
 */
function HistorySparkline({
  project,
  environmentId,
  containerId,
  metric,
  label,
  format,
}: {
  project: ProjectResponse
  environmentId: string
  containerId: string
  metric: string
  label: string
  format: (value: number) => string
}) {
  const { data } = useQuery({
    ...containerMetricsGetHistoryOptions({
      path: {
        project_id: project.id,
        environment_id: parseInt(environmentId),
        container_id: containerId,
      },
      query: { metric, range: '1h' },
    }),
    staleTime: 30_000,
    refetchInterval: 30_000,
    // Metrics store disabled → the endpoint 503s; don't retry-spam.
    retry: false,
  })

  if (!data?.length) return null

  const values = data.map((p) => p.value)
  const last = values[values.length - 1]

  return (
    <div className="hidden w-24 shrink-0 flex-col items-stretch gap-0.5 lg:flex">
      <MetricSparkline data={values} height={16} />
      <span className="text-right text-[10px] tabular-nums text-neutral-500 dark:text-neutral-400">
        {label} {format(last)}
      </span>
    </div>
  )
}

function UptimeInline({ createdAt }: { createdAt: string }) {
  // The label is derived from `createdAt` at render time; the interval only
  // forces a periodic re-render so the elapsed time stays fresh.
  const [, setTick] = useState(0)
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 30_000)
    return () => clearInterval(id)
  }, [])
  return <span className="tabular-nums">{formatUptime(createdAt)} uptime</span>
}

function formatUptime(createdAt: string): string {
  const elapsedMs = Date.now() - new Date(createdAt).getTime()
  const s = Math.max(0, Math.floor(elapsedMs / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ${m % 60}m`
  const d = Math.floor(h / 24)
  return `${d}d ${h % 24}h`
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(0)} MB`
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
}
