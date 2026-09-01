// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { cn } from '@/lib/utils'

interface MetricCardProps {
  title: string
  value: string | number
  change: string
  icon: React.ReactNode
  changeDisplay?: {
    icon: React.ReactNode
    className: string
    isPositive?: boolean
  }
  error?: boolean
  locked?: boolean
  lockedLabel?: string
  sparkline?: React.ReactNode
}

export function MetricCard({
  title,
  value,
  change,
  icon,
  changeDisplay,
  error,
  locked,
  lockedLabel = 'Coming soon',
  sparkline,
}: MetricCardProps) {
  return (
    <Card
      className={cn(
        'relative h-full w-full overflow-hidden transition-colors',
        error && 'border-destructive/50',
        locked && 'bg-muted/30'
      )}
    >
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {title}
        </CardTitle>
        <span className="text-muted-foreground [&_svg]:h-4 [&_svg]:w-4">
          {icon}
        </span>
      </CardHeader>
      <CardContent className="space-y-1">
        <div
          className={cn(
            'text-2xl font-semibold tracking-tight tabular-nums',
            locked && 'text-muted-foreground/60'
          )}
        >
          {value}
        </div>
        {change ? (
          changeDisplay ? (
            <p
              className={cn(
                'flex items-center text-xs',
                changeDisplay.className
              )}
            >
              {changeDisplay.icon}
              {change}
            </p>
          ) : (
            <p className="text-xs text-muted-foreground">{change}</p>
          )
        ) : (
          <p className="text-xs text-muted-foreground/60">&nbsp;</p>
        )}
        {sparkline && !locked ? (
          <div className="pt-2">{sparkline}</div>
        ) : null}
      </CardContent>
      {locked && (
        <div className="pointer-events-none absolute right-3 top-3">
          <span className="rounded-full border bg-background/80 px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
            {lockedLabel}
          </span>
        </div>
      )}
    </Card>
  )
}
