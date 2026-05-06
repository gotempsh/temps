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
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { formatBytes } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, Cpu, MemoryStick, Settings2 } from 'lucide-react'
import { useState } from 'react'
import { EditResourceLimitsDialog } from './EditResourceLimitsDialog'

interface ServiceResourcesPanelProps {
  serviceId: number
  serviceName: string
}

/**
 * Combined runtime + live-stats panel for an external service.
 *
 * Surfaces three things the operator needs when a database is misbehaving:
 * - Container restart count and OOM-killed flag (the smoking gun for
 *   silent crashes — kernel OOM never reaches the app's logs).
 * - Live CPU / memory usage with limit context, polled every 5s.
 * - The currently-applied resource caps + a button to change them.
 *
 * For cluster topologies, every member is rendered as its own row so a
 * promoted-then-demoted replica can be diagnosed in isolation.
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
    // 5-second poll is the sweet spot between "feels live" and "doesn't
    // hammer the docker socket". The bollard one-shot stats call is
    // cheap (single snapshot) so this scales fine for clusters.
    refetchInterval: 5_000,
    refetchIntervalInBackground: false,
    staleTime: 4_000,
  })

  // The first member's currentLimits is what the dialog seeds from. For
  // cluster services we trust the monitor + replicas to share caps —
  // every member is created with the same limits in init_cluster.
  const firstMember = runtimeQuery.data?.members?.[0]
  const currentLimits = firstMember?.resource_limits

  return (
    <>
      <Card>
        <CardHeader className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle>Resources</CardTitle>
            <CardDescription>
              Container runtime, live CPU/memory, and applied limits.
            </CardDescription>
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setDialogOpen(true)}
            disabled={runtimeQuery.isPending}
          >
            <Settings2 className="mr-2 h-4 w-4" />
            Edit limits
          </Button>
        </CardHeader>

        <CardContent className="space-y-4">
          {runtimeQuery.isPending ? (
            <ResourcesSkeleton />
          ) : runtimeQuery.isError ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>
                Failed to load runtime info. Showing the most recent
                snapshot if available — otherwise try reloading.
              </AlertDescription>
            </Alert>
          ) : runtimeQuery.data?.members.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No containers found for this service yet. Once it starts,
              runtime details will appear here.
            </p>
          ) : (
            <div className="space-y-4">
              {runtimeQuery.data?.members.map((member) => {
                const stats = statsQuery.data?.members.find(
                  (m) => m.container_name === member.container_name,
                )
                return (
                  <MemberRow
                    key={member.container_name}
                    member={member}
                    stats={stats}
                  />
                )
              })}
            </div>
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
// Member row: one container's runtime + stats. A standalone service has a
// single row labeled "standalone"; a cluster has one row per member.
// ---------------------------------------------------------------------------
function MemberRow({
  member,
  stats,
}: {
  member: ContainerRuntimeInfo
  stats: ContainerStatsSample | undefined
}) {
  const restartCount = member.restart_count ?? 0
  const oomKilled = member.oom_killed === true
  const status = member.status ?? null

  // We only flag a crash loop when restart_count > 0 — most healthy
  // containers stay at 0 forever. Any positive number deserves the
  // operator's attention even if not currently OOM-killed.
  const showRestartWarning = restartCount > 0
  return (
    <div className="space-y-3 rounded-md border p-4">
      {/* Identity row -------------------------------------------------- */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-2">
          <span className="font-medium">{member.container_name}</span>
          <Badge variant="outline">{member.role}</Badge>
          {status ? (
            <Badge
              variant={status === 'running' ? 'default' : 'secondary'}
              className="capitalize"
            >
              {status}
            </Badge>
          ) : (
            <Badge variant="secondary">missing</Badge>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
          {member.started_at ? (
            <span>
              Started <TimeAgo date={member.started_at} />
            </span>
          ) : null}
          <span
            className={
              showRestartWarning
                ? 'font-medium text-amber-600 dark:text-amber-400'
                : undefined
            }
          >
            {restartCount} restart{restartCount === 1 ? '' : 's'}
          </span>
        </div>
      </div>

      {/* OOM banner — high-value diagnostic, always shown when set. */}
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
            . Either raise the memory cap below, or investigate the
            workload that pushed past it.
          </AlertDescription>
        </Alert>
      ) : null}

      {/* Live stats ---------------------------------------------------- */}
      <div className="grid gap-4 md:grid-cols-2">
        <CpuMeter sample={stats} limits={member.resource_limits} />
        <MemoryMeter sample={stats} limits={member.resource_limits} />
      </div>

      {/* Applied limits summary --------------------------------------- */}
      <LimitsSummary limits={member.resource_limits} />
    </div>
  )
}

// ---------------------------------------------------------------------------
// Per-metric cells: CPU and memory each get their own progress bar with
// percent-of-limit and absolute usage labels. Falls back to a "—" line
// when stats are unavailable (container not running yet, docker socket
// blip, etc.) so the panel layout stays stable.
// ---------------------------------------------------------------------------
function CpuMeter({
  sample,
  limits,
}: {
  sample: ContainerStatsSample | undefined
  limits: ContainerRuntimeInfo['resource_limits']
}) {
  // Docker's `cpu_percent` is computed as
  //   (cpu_delta / system_delta) * online_cpus * 100
  // so 100% == "saturating every host core". When the operator has set a
  // CPU cap (`nano_cpus`), we want the user's mental model to be "% of my
  // cap", not "% of the host" — otherwise a container pinned to 4 of 16
  // cores can never read above 25% and the bar feels broken.
  const hostCpuPercent = sample?.cpu_percent ?? null
  const onlineCpus = sample?.online_cpus ?? null
  const capCores =
    limits?.nano_cpus != null && limits.nano_cpus > 0
      ? limits.nano_cpus / 1_000_000_000
      : null

  // Re-base the percent against the cap when one is set. Multiply by
  // (host / cap) — if you're capped at 4 of 16 cores, you reach 100% of
  // your cap when host_cpu_percent hits 25%.
  let displayedPercent = hostCpuPercent
  if (
    displayedPercent != null &&
    capCores != null &&
    onlineCpus != null &&
    capCores < onlineCpus
  ) {
    displayedPercent = displayedPercent * (onlineCpus / capCores)
  }

  // Cap the bar at 100% even if the raw computation overshoots — Docker's
  // formula can read >100% during the first sample window of a freshly
  // started container, and the rebase math can also briefly overshoot.
  const barValue =
    displayedPercent != null
      ? Math.min(100, Math.max(0, displayedPercent))
      : 0

  // Caption: prefer the cap when set, fall back to host cores. Always
  // include the host context when capped so the operator can verify the
  // cap is the bound (not coincidence).
  const caption =
    capCores != null
      ? `of ${capCores.toFixed(capCores % 1 === 0 ? 0 : 2)} core${capCores === 1 ? '' : 's'} capped${onlineCpus != null ? ` (host ${onlineCpus})` : ''}`
      : onlineCpus != null
        ? `of ${onlineCpus} core${onlineCpus === 1 ? '' : 's'}`
        : null

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 text-sm font-medium">
          <Cpu className="h-4 w-4 text-muted-foreground" />
          CPU
        </div>
        <span className="text-sm tabular-nums">
          {displayedPercent != null ? `${displayedPercent.toFixed(1)}%` : '—'}
          {caption != null ? (
            <span className="ml-1 text-xs text-muted-foreground">
              {caption}
            </span>
          ) : null}
        </span>
      </div>
      <Progress value={barValue} />
    </div>
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

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 text-sm font-medium">
          <MemoryStick className="h-4 w-4 text-muted-foreground" />
          Memory
        </div>
        <span className="text-sm tabular-nums">
          {usage != null ? formatBytes(usage) : '—'}
          {limit != null ? (
            <span className="ml-1 text-xs text-muted-foreground">
              / {formatBytes(limit)}
            </span>
          ) : null}
        </span>
      </div>
      <Progress value={barValue} />
      {!hasUserLimit && limit != null ? (
        // No user-set cap → Docker reports host RAM as the "limit" and the
        // percentage reads "5% of host". Make that explicit so operators
        // don't think this is a real bound.
        <p className="text-xs text-muted-foreground">
          Unlimited — percentage shown is of host RAM, not an applied cap.
        </p>
      ) : null}
    </div>
  )
}

function LimitsSummary({
  limits,
}: {
  limits: ContainerRuntimeInfo['resource_limits']
}) {
  const memory = limits?.memory_mb ?? null
  const swap = limits?.memory_swap_mb ?? null
  const nano = limits?.nano_cpus ?? null
  const cpuCores = nano != null ? nano / 1_000_000_000 : null

  if (memory == null && nano == null) {
    return (
      <p className="text-xs text-muted-foreground">
        No limits applied — this container runs unconstrained.
      </p>
    )
  }
  return (
    <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
      {memory != null ? (
        <span>
          <span className="text-foreground font-medium">{memory} MiB</span>
          {' memory'}
          {swap != null && swap > memory ? ` (+${swap - memory} MiB swap)` : ''}
        </span>
      ) : null}
      {cpuCores != null ? (
        <span>
          <span className="text-foreground font-medium">
            {cpuCores.toFixed(2)}
          </span>
          {' CPU cores'}
        </span>
      ) : null}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Skeleton matching the live row's layout so the page doesn't visibly
// collapse and re-expand when data arrives. Per project guidelines:
// "Skeletons over spinners for content loading".
// ---------------------------------------------------------------------------
function ResourcesSkeleton() {
  return (
    <div className="space-y-3 rounded-md border p-4">
      <div className="flex items-center justify-between">
        <Skeleton className="h-5 w-48" />
        <Skeleton className="h-4 w-24" />
      </div>
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Skeleton className="h-4 w-24" />
          <Skeleton className="h-2 w-full" />
        </div>
        <div className="space-y-2">
          <Skeleton className="h-4 w-32" />
          <Skeleton className="h-2 w-full" />
        </div>
      </div>
      <Skeleton className="h-3 w-64" />
    </div>
  )
}
