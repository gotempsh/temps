// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * DiskHealthCard — software RAID state and drive SMART health of a node.
 *
 * Reads `GET /nodes/{id}/metrics/disk-health`. Always rendered: when nothing
 * could be read it says why (not Linux, not root, smartctl missing) instead
 * of disappearing, so the operator learns the check exists.
 */

import { nodeMetricsGetDiskHealthOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, CheckCircle2, Info } from 'lucide-react'
import {
  WEAR_WARN_PERCENT,
  describeDrive,
  driveTone,
  overallTone,
  summarize,
  type DiskHealthTone,
} from './disk-health'

/** SMART data is refreshed every two minutes on the node. */
const REFRESH_MS = 60_000

const TONE_CLASS: Record<DiskHealthTone, string> = {
  good: 'text-emerald-700 dark:text-emerald-500',
  warn: 'text-amber-700 dark:text-amber-500',
  poor: 'text-rose-700 dark:text-rose-500',
  unknown: 'text-muted-foreground',
}

function ToneIcon({ tone }: { tone: DiskHealthTone }) {
  const className = cn('h-3.5 w-3.5 shrink-0', TONE_CLASS[tone])
  if (tone === 'good') return <CheckCircle2 className={className} />
  if (tone === 'unknown') return <Info className={className} />
  return <AlertTriangle className={className} />
}

export function DiskHealthCard({ nodeId }: { nodeId: number }) {
  const q = useQuery({
    ...nodeMetricsGetDiskHealthOptions({ path: { id: nodeId } }),
    refetchInterval: REFRESH_MS,
    retry: false,
  })
  const health = q.data
  const tone = health ? overallTone(health) : 'unknown'

  return (
    <Card className="shadow-none">
      <CardHeader className="pb-2">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Disk health
        </CardTitle>
        <CardDescription>
          Software RAID arrays and drive wear / media errors (SMART)
        </CardDescription>
      </CardHeader>
      <CardContent>
        {q.isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-6 w-40" />
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        ) : q.isError || !health ? (
          <p className="text-sm text-destructive">
            {(q.error as { detail?: string } | null)?.detail ??
              'Could not load disk health.'}
          </p>
        ) : (
          <div className="space-y-3">
            <p
              className={cn(
                'flex items-center gap-1.5 text-sm font-medium',
                TONE_CLASS[tone]
              )}
            >
              <ToneIcon tone={tone} />
              {summarize(health)}
            </p>
            {health.raid_arrays.length > 0 && (
              <ul className="space-y-1 text-xs">
                {health.raid_arrays.map((a) => (
                  <li key={a.name} className="flex items-center gap-1.5">
                    <ToneIcon tone={a.degraded ? 'poor' : 'good'} />
                    <span className="font-medium">{a.name}</span>
                    <span className="text-muted-foreground">
                      {a.degraded
                        ? `degraded · ${a.devices_failed} member${a.devices_failed === 1 ? '' : 's'} missing or failed`
                        : 'all members active'}
                    </span>
                  </li>
                ))}
              </ul>
            )}
            {health.drives.length > 0 && (
              <ul className="space-y-1 text-xs">
                {health.drives.map((d) => (
                  <li key={d.device} className="flex items-center gap-1.5">
                    <ToneIcon tone={driveTone(d)} />
                    <span className="font-medium">{d.device}</span>
                    <span className="tabular-nums text-muted-foreground">
                      {describeDrive(d)}
                    </span>
                  </li>
                ))}
              </ul>
            )}
            {health.reason && (
              <p className="text-xs text-muted-foreground">{health.reason}</p>
            )}
            <p className="text-[11px] text-muted-foreground">
              Alerts fire on a degraded array, wear above {WEAR_WARN_PERCENT}%,
              or any media error.
            </p>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
