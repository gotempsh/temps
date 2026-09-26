// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { LucideIcon } from 'lucide-react'
import { cn } from './lib/cn'

export interface ResourceStatProps {
  /** Metric icon — `Cpu`, `HardDrive`, `MemoryStick`, etc. from `lucide-react`. */
  icon: LucideIcon
  /** Headline value, already formatted — `fmtBytes(...)`, `fmtPercent(...)`, or a domain formatter like `formatCpuUsage(...)`. */
  value: string
  /** Optional trailing context rendered muted, e.g. "/ 42%" or "/ 2 cores". Not re-formatted — pass it pre-built. */
  limit?: string
  className?: string
}

/**
 * Inline CPU/memory/disk usage display: icon + formatted value + optional
 * muted limit/percent suffix. Generalizes the near-identical
 * `<Cpu className="size-3"/> {formatCpuUsage(...)}` / `<HardDrive .../>
 * {formatBytes(...)}` pairs duplicated across ~11 files (`ContainerList.tsx`,
 * `ContainerHeaderBar.tsx`, `ServiceResourcesPanel.tsx`,
 * `storage/MonitoringCard.tsx`, `project/ProjectStorage.tsx`,
 * `ProjectOverview.tsx`, `monitoring/EnvironmentMetricsCard.tsx`,
 * `ServerMonitoring.tsx`, `pages/ServiceMonitoring.tsx`,
 * `pages/settings/NodesPage.tsx`, `pages/Storage.tsx`).
 *
 * Deliberately does not format the value itself — callers already have
 * different raw shapes (a percent, a byte count, a cores-vs-cap combo) and
 * should format with `fmt.ts` (`fmtBytes`, `fmtPercent`, `fmtNumber`) or a
 * domain-specific formatter before handing the string in. This primitive
 * only owns the shared *display* shape, not every formatting rule.
 */
export function ResourceStat({ icon: Icon, value, limit, className }: ResourceStatProps) {
  return (
    <span className={cn('inline-flex items-center gap-1 text-xs tabular-nums', className)}>
      <Icon className="size-3" aria-hidden />
      <span>{value}</span>
      {limit ? <span className="text-muted-foreground">{limit}</span> : null}
    </span>
  )
}
