import { ContainerInfoResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronsUpDown,
  ExternalLink,
  FileText,
  Loader2,
  Play,
  RotateCw,
  Settings2,
  ServerIcon,
  Square,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useContainerMetricsStream } from './useContainerMetricsStream'
import { ContainerMetricHistory } from './ContainerMetricHistory'

type ContainerStatus = string
type ContainerTab = 'logs' | 'configuration'

interface ContainerHeaderBarProps {
  projectId: string
  environmentId: string
  containers: ContainerInfoResponse[]
  selectedContainer: ContainerInfoResponse | null
  onSelect: (id: string) => void
  tab: ContainerTab
  onTabChange: (tab: ContainerTab) => void
  onAction: (action: 'start' | 'stop' | 'restart') => void
  actionInFlight?: 'start' | 'stop' | 'restart' | null
}

export function ContainerHeaderBar({
  projectId,
  environmentId,
  containers,
  selectedContainer,
  onSelect,
  tab,
  onTabChange,
  onAction,
  actionInFlight,
}: ContainerHeaderBarProps) {
  const [statusFilter, setStatusFilter] = useState<
    'all' | 'running' | 'stopped'
  >('all')

  const runningCount = containers.filter((c) => c.status === 'running').length
  const stoppedCount = containers.length - runningCount

  const filtered = useMemo(() => {
    if (statusFilter === 'all') return containers
    return containers.filter((c) =>
      statusFilter === 'running'
        ? c.status === 'running'
        : c.status !== 'running'
    )
  }, [containers, statusFilter])

  const isRunning = selectedContainer?.status === 'running'

  const { metrics } = useContainerMetricsStream(
    projectId,
    environmentId,
    selectedContainer?.container_id ?? '',
    isRunning
  )

  const navItems: {
    id: ContainerTab
    title: string
    icon: typeof FileText
  }[] = [
    { id: 'logs', title: 'Logs', icon: FileText },
    { id: 'configuration', title: 'Configuration', icon: Settings2 },
  ]

  return (
    <div className="border-b bg-white/95 backdrop-blur dark:border-neutral-800 dark:bg-neutral-950/95">
      <div className="w-full px-4 sm:px-6 lg:px-8">
        <div className="flex flex-col gap-4 pt-5 pb-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2.5">
              <ContainerSwitcher
                containers={filtered}
                allCount={containers.length}
                runningCount={runningCount}
                stoppedCount={stoppedCount}
                statusFilter={statusFilter}
                onStatusFilterChange={setStatusFilter}
                selected={selectedContainer}
                onSelect={onSelect}
              />
              <StatusPill status={selectedContainer?.status} />
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-1.5 text-sm text-neutral-600 dark:text-neutral-400">
              {selectedContainer?.image_name && (
                <div className="inline-flex items-center gap-1.5 min-w-0">
                  <code className="font-mono text-[0.8125rem] truncate max-w-[28rem]">
                    {selectedContainer.image_name}
                  </code>
                </div>
              )}
              {selectedContainer?.node_name && (
                <div className="inline-flex items-center gap-1.5">
                  <ServerIcon className="size-3.5" aria-hidden="true" />
                  <span>{selectedContainer.node_name}</span>
                </div>
              )}
              {(metrics?.started_at || selectedContainer?.created_at) && (
                <UptimeInline
                  startedAt={
                    metrics?.started_at || selectedContainer?.created_at || null
                  }
                />
              )}
              {(metrics?.restart_count ?? 0) > 0 && (
                <RestartCountChip count={metrics?.restart_count ?? 0} />
              )}
              <ContainerMetricHistory
                projectId={parseInt(projectId)}
                environmentId={parseInt(environmentId)}
                containerId={selectedContainer?.container_id ?? ''}
                metric="container.cpu_percent"
                label="CPU"
                currentValue={metrics?.cpu_percent}
                format={(value) => `${value.toFixed(1)}%`}
                enabled={Boolean(isRunning && selectedContainer)}
              />
              <ContainerMetricHistory
                projectId={parseInt(projectId)}
                environmentId={parseInt(environmentId)}
                containerId={selectedContainer?.container_id ?? ''}
                metric="container.memory_used_bytes"
                label="Mem"
                currentValue={metrics?.memory_bytes}
                format={formatBytes}
                enabled={Boolean(isRunning && selectedContainer)}
              />
              {isRunning && metrics && (
                <>
                  <div className="inline-flex items-center gap-1.5 tabular-nums">
                    <ArrowDown className="size-3.5" aria-hidden="true" />
                    <span>{formatBytes(metrics.network_rx_rate)}/s</span>
                    <ArrowUp className="size-3.5 ml-1" aria-hidden="true" />
                    <span>{formatBytes(metrics.network_tx_rate)}/s</span>
                  </div>
                </>
              )}
              {selectedContainer?.service_url && (
                <a
                  href={selectedContainer.service_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  title={selectedContainer.service_url}
                  className="inline-flex items-center gap-1.5 text-neutral-900 hover:underline dark:text-white"
                >
                  <span className="truncate max-w-[18rem]">
                    {selectedContainer.service_url}
                  </span>
                  <ExternalLink className="size-3" aria-hidden="true" />
                </a>
              )}
            </div>
          </div>

          <div className="flex items-center gap-2">
            {isRunning ? (
              <>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={actionInFlight === 'restart'}
                  onClick={() => onAction('restart')}
                >
                  {actionInFlight === 'restart' ? (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  ) : (
                    <RotateCw className="mr-1.5 size-4" aria-hidden="true" />
                  )}
                  Restart
                </Button>
                <Button
                  type="button"
                  variant="destructive"
                  size="sm"
                  disabled={actionInFlight === 'stop'}
                  onClick={() => onAction('stop')}
                >
                  {actionInFlight === 'stop' ? (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  ) : (
                    <Square className="mr-1.5 size-4" aria-hidden="true" />
                  )}
                  Stop
                </Button>
              </>
            ) : (
              selectedContainer && (
                <Button
                  type="button"
                  size="sm"
                  disabled={actionInFlight === 'start'}
                  onClick={() => onAction('start')}
                >
                  {actionInFlight === 'start' ? (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  ) : (
                    <Play className="mr-1.5 size-4" aria-hidden="true" />
                  )}
                  Start
                </Button>
              )
            )}
          </div>
        </div>

        <nav
          className="-mb-px flex gap-6 overflow-x-auto"
          aria-label="Container sections"
        >
          {navItems.map((item) => {
            const Icon = item.icon
            const active = tab === item.id
            return (
              <button
                type="button"
                key={item.id}
                onClick={() => onTabChange(item.id)}
                className={`inline-flex items-center gap-2 whitespace-nowrap border-b-2 px-1 py-3 text-sm font-medium transition-colors ${
                  active
                    ? 'border-neutral-900 text-neutral-900 dark:border-white dark:text-white'
                    : 'border-transparent text-neutral-500 hover:border-neutral-300 hover:text-neutral-800 dark:text-neutral-400 dark:hover:border-neutral-600 dark:hover:text-neutral-200'
                }`}
                aria-current={active ? 'page' : undefined}
              >
                <Icon className="size-4" aria-hidden="true" />
                {item.title}
              </button>
            )
          })}
        </nav>
      </div>
    </div>
  )
}

interface ContainerSwitcherProps {
  containers: ContainerInfoResponse[]
  allCount: number
  runningCount: number
  stoppedCount: number
  statusFilter: 'all' | 'running' | 'stopped'
  onStatusFilterChange: (f: 'all' | 'running' | 'stopped') => void
  selected: ContainerInfoResponse | null
  onSelect: (id: string) => void
}

function ContainerSwitcher({
  containers,
  allCount,
  runningCount,
  stoppedCount,
  statusFilter,
  onStatusFilterChange,
  selected,
  onSelect,
}: ContainerSwitcherProps) {
  const canSwitch = allCount > 1
  const label =
    selected?.service_name || selected?.container_name || 'Select container'

  if (!canSwitch) {
    return (
      <h2 className="truncate text-2xl font-semibold tracking-tight text-neutral-950 dark:text-white">
        {label}
      </h2>
    )
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="group inline-flex items-center gap-1.5 rounded-md px-1.5 py-0.5 -ml-1.5 text-2xl font-semibold tracking-tight text-neutral-950 hover:bg-neutral-100 dark:text-white dark:hover:bg-white/5"
        >
          <span className="truncate max-w-[20rem]">{label}</span>
          <ChevronsUpDown
            className="size-4 text-neutral-400 group-hover:text-neutral-600 dark:group-hover:text-neutral-300"
            aria-hidden="true"
          />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-80">
        <div className="flex items-center gap-1 p-1">
          {(
            [
              { id: 'all', label: 'All', count: allCount },
              { id: 'running', label: 'Running', count: runningCount },
              { id: 'stopped', label: 'Stopped', count: stoppedCount },
            ] as const
          ).map((f) => (
            <button
              key={f.id}
              type="button"
              onClick={() => onStatusFilterChange(f.id)}
              className={`flex-1 rounded-sm px-2 py-1 text-xs font-medium transition-colors ${
                statusFilter === f.id
                  ? 'bg-neutral-100 text-neutral-900 dark:bg-white/10 dark:text-white'
                  : 'text-neutral-500 hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200'
              }`}
            >
              {f.label}
              <span className="ml-1 text-neutral-400 dark:text-neutral-500 tabular-nums">
                {f.count}
              </span>
            </button>
          ))}
        </div>
        <DropdownMenuSeparator />
        <DropdownMenuLabel className="text-xs uppercase tracking-wide text-neutral-500 dark:text-neutral-400">
          Containers
        </DropdownMenuLabel>
        <div className="max-h-80 overflow-y-auto">
          {containers.length === 0 ? (
            <div className="px-2 py-4 text-center text-sm text-neutral-500 dark:text-neutral-400">
              No containers match this filter
            </div>
          ) : (
            containers.map((c) => {
              const isSelected = c.container_id === selected?.container_id
              const running = c.status === 'running'
              return (
                <DropdownMenuItem
                  key={c.container_id}
                  onSelect={() => onSelect(c.container_id)}
                  className="flex items-start gap-2"
                >
                  <Check
                    className={`size-4 mt-0.5 shrink-0 ${
                      isSelected ? 'opacity-100' : 'opacity-0'
                    }`}
                    aria-hidden="true"
                  />
                  <div className="flex flex-1 min-w-0 flex-col">
                    <div className="flex items-center gap-2 min-w-0">
                      <span
                        className={`size-1.5 shrink-0 rounded-full ${
                          running ? 'bg-emerald-500' : 'bg-neutral-400'
                        }`}
                        aria-hidden="true"
                      />
                      <span className="truncate font-medium">
                        {c.service_name || c.container_name}
                      </span>
                    </div>
                    <span className="truncate font-mono text-xs text-neutral-500 dark:text-neutral-400 pl-3.5">
                      {c.image_name}
                    </span>
                  </div>
                </DropdownMenuItem>
              )
            })
          )}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function StatusPill({ status }: { status?: ContainerStatus }) {
  const running = status === 'running'
  const stopped = status && status !== 'running' && status !== 'error'
  const errored = status === 'error'

  const tone = errored
    ? 'bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-500/10 dark:text-red-400 dark:ring-red-500/30'
    : running
      ? 'bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-500/10 dark:text-emerald-400 dark:ring-emerald-500/30'
      : 'bg-neutral-100 text-neutral-700 ring-neutral-950/10 dark:bg-white/5 dark:text-neutral-300 dark:ring-white/10'

  const dotTone = errored
    ? 'bg-red-500'
    : running
      ? 'bg-emerald-500'
      : 'bg-neutral-400'

  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset ${tone}`}
    >
      <span
        className={`size-1.5 rounded-full ${dotTone} ${running ? 'animate-pulse' : ''}`}
        aria-hidden="true"
      />
      {status ?? 'unknown'}
      {stopped && ''}
    </span>
  )
}

function UptimeInline({ startedAt }: { startedAt: string | null }) {
  // The label is a pure derivation of `startedAt` + wall clock. We only need
  // an interval to *recompute* — no need to write derived state in an effect.
  const [tick, setTick] = useState(0)
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 30_000)
    return () => clearInterval(id)
  }, [])
  // `tick` is intentional — it forces the label to recompute against the
  // current wall clock every 30s without persisting derived state.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const label = useMemo(() => formatUptime(startedAt), [startedAt, tick])
  if (!label) return null
  return (
    <div className="inline-flex items-center gap-1.5 tabular-nums">
      <span>{label} uptime</span>
    </div>
  )
}

function formatUptime(startedAt: string | null): string {
  if (!startedAt) return ''
  const t = new Date(startedAt).getTime()
  if (!Number.isFinite(t)) return ''
  const elapsedMs = Date.now() - t
  if (elapsedMs < 0) return ''
  const s = Math.max(0, Math.floor(elapsedMs / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ${m % 60}m`
  const d = Math.floor(h / 24)
  return `${d}d ${h % 24}h`
}

function RestartCountChip({ count }: { count: number }) {
  // 1 restart is noteworthy, > 3 is alarming. Tone the chip accordingly so
  // a crash loop is visible from a distance without making a single planned
  // restart look like an emergency.
  const tone =
    count >= 3
      ? 'bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-500/10 dark:text-red-400 dark:ring-red-500/30'
      : 'bg-amber-50 text-amber-700 ring-amber-600/20 dark:bg-amber-500/10 dark:text-amber-400 dark:ring-amber-500/30'
  return (
    <span
      title={`Container has restarted ${count} time${count === 1 ? '' : 's'} since creation`}
      className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset tabular-nums ${tone}`}
    >
      <RotateCw className="size-3" aria-hidden="true" />
      Restarted {count}×
    </span>
  )
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes.toFixed(0)} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(0)} MB`
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
}
