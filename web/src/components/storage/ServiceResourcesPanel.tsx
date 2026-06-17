import {
  getServiceRuntimeOptions,
  getServiceStatsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ContainerRuntimeInfo,
  ContainerStatsSample,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { formatBytes, cn } from '@/lib/utils'
import { formatCpuUsage } from '@/lib/cpu-format'
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, Cpu, MemoryStick, Settings2 } from 'lucide-react'
import { useState } from 'react'
import { EditResourceLimitsDialog } from './EditResourceLimitsDialog'

interface ServiceResourcesPanelProps {
  serviceId: number
  serviceName: string
}

/**
 * Compact runtime + live-stats panel for an external service.
 *
 * Per-member layout is a single horizontal row with inline CPU/memory meters
 * so a standalone service occupies one line and a 3-node cluster occupies
 * three. The OOM alert is the only thing that breaks vertical rhythm — it
 * earns the room because it's the smoking gun for silent crashes.
 */
export function ServiceResourcesPanel({
  serviceId,
  serviceName,
}: ServiceResourcesPanelProps) {
  const [dialogOpen, setDialogOpen] = useState(false)

  const runtimeQuery = useQuery({
    ...getServiceRuntimeOptions({ path: { id: serviceId } }),
    refetchInterval: 30_000,
    staleTime: 25_000,
  })

  const statsQuery = useQuery({
    ...getServiceStatsOptions({ path: { id: serviceId } }),
    refetchInterval: 5_000,
    refetchIntervalInBackground: false,
    staleTime: 4_000,
  })

  const firstMember = runtimeQuery.data?.members?.[0]
  const currentLimits = firstMember?.resource_limits
  const isLive = statsQuery.isSuccess && !statsQuery.isStale

  return (
    <>
      <Card className="py-0">
        <CardContent className="px-4 py-2 sm:px-6">
          {runtimeQuery.isPending ? (
            <ResourcesSkeleton />
          ) : runtimeQuery.isError ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>
                Failed to load runtime info. Try reloading.
              </AlertDescription>
            </Alert>
          ) : runtimeQuery.data?.members.length === 0 ? (
            <div className="flex items-center justify-between py-1">
              <p className="text-sm text-muted-foreground">
                No containers yet — runtime details appear once it starts.
              </p>
              <EditLimitsButton onClick={() => setDialogOpen(true)} />
            </div>
          ) : (
            <ul role="list" className="-mx-2 divide-y divide-border/60">
              {runtimeQuery.data?.members.map((member, idx) => {
                const stats = statsQuery.data?.members.find(
                  (m) => m.container_name === member.container_name,
                )
                return (
                  <MemberRow
                    key={member.container_name}
                    member={member}
                    stats={stats}
                    isLive={isLive && idx === 0}
                    onEditLimits={
                      idx === 0 ? () => setDialogOpen(true) : undefined
                    }
                  />
                )
              })}
            </ul>
          )}
        </CardContent>
      </Card>

      <EditResourceLimitsDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        serviceId={serviceId}
        serviceName={serviceName}
        currentLimits={currentLimits}
      />
    </>
  )
}

// ---------------------------------------------------------------------------
// Member row: identity left, inline meters right, edit-limits trailing on
// the first row. Live pulse rides on the first row's status dot to avoid a
// separate header. OOM alert is full-width because the operator must see it.
// ---------------------------------------------------------------------------
function MemberRow({
  member,
  stats,
  isLive,
  onEditLimits,
}: {
  member: ContainerRuntimeInfo
  stats: ContainerStatsSample | undefined
  isLive: boolean
  onEditLimits?: () => void
}) {
  const restartCount = member.restart_count ?? 0
  const oomKilled = member.oom_killed === true
  const status = member.status ?? null
  const showRestartWarning = restartCount > 0

  return (
    <li className="space-y-2 px-2 py-2.5">
      <div className="flex flex-col gap-2 lg:flex-row lg:items-center lg:gap-4">
        {/* Identity: name • role • status • limits hint */}
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1">
          <span className="truncate text-sm font-medium">
            {member.container_name}
          </span>
          <Badge variant="outline" className="h-5 px-1.5 text-[10px]">
            {member.role}
          </Badge>
          <StatusDot status={status} live={isLive} />
          <LimitsInline limits={member.resource_limits} />
        </div>

        {/* Live meters — sit on a single line above lg, stack tight below */}
        <div className="grid flex-1 grid-cols-2 gap-3 sm:gap-6 lg:max-w-[420px] lg:flex-none lg:shrink-0 lg:basis-[420px]">
          <CpuMeter sample={stats} limits={member.resource_limits} />
          <MemoryMeter sample={stats} limits={member.resource_limits} />
        </div>

        {onEditLimits && (
          <EditLimitsButton onClick={onEditLimits} />
        )}
      </div>

      {/* Started + restart counter — third-tier metadata, only shown when
          there's a signal worth surfacing (started_at or non-zero restarts). */}
      {(member.started_at || showRestartWarning) && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          {member.started_at ? (
            <span>
              Started <TimeAgo date={member.started_at} />
            </span>
          ) : null}
          {showRestartWarning && (
            <span className="font-medium text-amber-600 dark:text-amber-400">
              {restartCount} restart{restartCount === 1 ? '' : 's'}
            </span>
          )}
        </div>
      )}

      {oomKilled ? (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Last termination was an OOM kill</AlertTitle>
          <AlertDescription>
            The kernel killed this container because it exceeded its memory
            limit
            {member.exit_code != null ? ` (exit ${member.exit_code})` : ''}
            {member.finished_at ? (
              <>
                {' '}
                <TimeAgo date={member.finished_at} />
              </>
            ) : null}
            . Either raise the memory cap, or investigate the workload that
            pushed past it.
          </AlertDescription>
        </Alert>
      ) : null}
    </li>
  )
}

// Tiny status pill — saves the horizontal space a full Badge eats. When
// `live` is set on a running container the dot pulses, replacing the
// separate "live" indicator the old card header used to show.
function StatusDot({
  status,
  live = false,
}: {
  status: string | null
  live?: boolean
}) {
  if (!status) {
    return (
      <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
        <span className="size-1.5 rounded-full bg-muted-foreground/40" />
        missing
      </span>
    )
  }
  const tone =
    status === 'running'
      ? 'bg-emerald-500'
      : status === 'exited' || status === 'dead'
        ? 'bg-red-500'
        : 'bg-amber-500'
  return (
    <span className="inline-flex items-center gap-1 text-xs capitalize text-muted-foreground">
      <span className="relative inline-flex size-1.5">
        {live && status === 'running' && (
          <span className="absolute inline-flex size-full animate-ping rounded-full bg-emerald-500/70" />
        )}
        <span className={cn('relative size-1.5 rounded-full', tone)} />
      </span>
      {status}
    </span>
  )
}

// Compact icon-only "Edit limits" trigger — title attribute carries the
// label since the panel header that used to spell it out is gone.
function EditLimitsButton({ onClick }: { onClick: () => void }) {
  return (
    <Button
      variant="ghost"
      size="icon"
      onClick={onClick}
      title="Edit limits"
      aria-label="Edit limits"
      className="size-7 shrink-0 text-muted-foreground hover:text-foreground"
    >
      <Settings2 className="size-3.5" />
    </Button>
  )
}

// ---------------------------------------------------------------------------
// CPU meter: inline value + thin bar. Value reads as cores used (and "/ cap"
// when a CPU cap is configured) so a 2-core-pinned container says "2.00 / 2
// cores" rather than the raw "200%" Docker reports. The bar fills against
// the cap when set, otherwise against host cores.
// ---------------------------------------------------------------------------
function CpuMeter({
  sample,
  limits,
}: {
  sample: ContainerStatsSample | undefined
  limits: ContainerRuntimeInfo['resource_limits']
}) {
  const hostCpuPercent = sample?.cpu_percent ?? null
  const onlineCpus = sample?.online_cpus ?? null
  const capCores =
    limits?.nano_cpus != null && limits.nano_cpus > 0
      ? limits.nano_cpus / 1_000_000_000
      : null

  // Bar fills against the cap when set; otherwise against host cores so
  // pinning a single core on an 8-core host doesn't look like 100% load.
  let barValue = 0
  if (hostCpuPercent != null) {
    const denomCores = capCores ?? onlineCpus
    if (denomCores != null && denomCores > 0) {
      const pctOfDenom = (hostCpuPercent / 100 / denomCores) * 100
      barValue = Math.min(100, Math.max(0, pctOfDenom))
    } else {
      barValue = Math.min(100, Math.max(0, hostCpuPercent))
    }
  }

  return (
    <Meter
      icon={<Cpu className="size-3.5" />}
      label="CPU"
      value={
        hostCpuPercent != null
          ? formatCpuUsage(hostCpuPercent, capCores)
          : '—'
      }
      barPercent={barValue}
      tone={barValue >= 90 ? 'danger' : barValue >= 70 ? 'warn' : 'normal'}
    />
  )
}

function MemoryMeter({
  sample,
  limits,
}: {
  sample: ContainerStatsSample | undefined
  limits: ContainerRuntimeInfo['resource_limits']
}) {
  const usage = sample?.memory_usage_bytes ?? null
  const limit = sample?.memory_limit_bytes ?? null
  const percent = sample?.memory_percent ?? null
  const hasUserLimit = (limits?.memory_mb ?? null) != null

  const barValue = percent != null ? Math.min(100, Math.max(0, percent)) : 0
  const tone = !hasUserLimit
    ? 'normal'
    : barValue >= 90
      ? 'danger'
      : barValue >= 70
        ? 'warn'
        : 'normal'

  return (
    <Meter
      icon={<MemoryStick className="size-3.5" />}
      label="Memory"
      value={
        usage != null
          ? hasUserLimit && limit != null
            ? `${formatBytes(usage)} / ${formatBytes(limit)}`
            : formatBytes(usage)
          : '—'
      }
      barPercent={barValue}
      tone={tone}
      hint={!hasUserLimit && limit != null ? 'unlimited' : undefined}
    />
  )
}

// Shared inline meter primitive — small icon + label on top, value on right,
// thin bar below. Tone shifts the bar color past 70% / 90%.
function Meter({
  icon,
  label,
  value,
  barPercent,
  tone = 'normal',
  hint,
}: {
  icon: React.ReactNode
  label: string
  value: string
  barPercent: number
  tone?: 'normal' | 'warn' | 'danger'
  hint?: string
}) {
  const barClass =
    tone === 'danger'
      ? 'bg-red-500'
      : tone === 'warn'
        ? 'bg-amber-500'
        : 'bg-foreground/70'
  return (
    <div className="min-w-0">
      <div className="flex items-baseline justify-between gap-2">
        <div className="flex items-center gap-1 text-xs font-medium text-muted-foreground">
          <span className="text-muted-foreground/80">{icon}</span>
          {label}
          {hint && (
            <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
              {hint}
            </span>
          )}
        </div>
        <span className="truncate text-xs font-medium tabular-nums">
          {value}
        </span>
      </div>
      <div
        className="mt-1 h-1 w-full overflow-hidden rounded-full bg-muted"
        role="progressbar"
        aria-valuenow={Math.round(barPercent)}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div
          className={cn('h-full w-(--meter) transition-all', barClass)}
          style={{ '--meter': `${barPercent}%` } as React.CSSProperties}
        />
      </div>
    </div>
  )
}

// Inline applied-limits chip — three small pills max (mem / cpu / swap).
function LimitsInline({
  limits,
}: {
  limits: ContainerRuntimeInfo['resource_limits']
}) {
  const memory = limits?.memory_mb ?? null
  const swap = limits?.memory_swap_mb ?? null
  const nano = limits?.nano_cpus ?? null
  const shm = limits?.shm_size_mb ?? null
  const cpuCores = nano != null ? nano / 1_000_000_000 : null

  if (memory == null && nano == null && shm == null) {
    return (
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground/60">
        Unlimited
      </span>
    )
  }
  return (
    <span className="inline-flex flex-wrap items-center gap-1.5 text-[11px] tabular-nums text-muted-foreground">
      {cpuCores != null && (
        <span>
          <span className="text-foreground/80">
            {cpuCores.toFixed(cpuCores % 1 === 0 ? 0 : 2)}
          </span>{' '}
          cpu
        </span>
      )}
      {memory != null && (
        <span>
          <span className="text-foreground/80">{memory}</span> MiB
          {swap != null && swap > memory ? ` (+${swap - memory} swap)` : ''}
        </span>
      )}
      {shm != null && (
        <span>
          <span className="text-foreground/80">{shm}</span> MiB shm
        </span>
      )}
    </span>
  )
}

// Skeleton matches the new compact row layout.
function ResourcesSkeleton() {
  return (
    <div className="space-y-3 py-2">
      <div className="flex items-center gap-3">
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-4 w-12" />
        <Skeleton className="h-4 w-16" />
        <div className="ml-auto flex gap-6">
          <Skeleton className="h-4 w-24" />
          <Skeleton className="h-4 w-32" />
        </div>
      </div>
      <div className="grid grid-cols-2 gap-6">
        <Skeleton className="h-1 w-full" />
        <Skeleton className="h-1 w-full" />
      </div>
    </div>
  )
}
