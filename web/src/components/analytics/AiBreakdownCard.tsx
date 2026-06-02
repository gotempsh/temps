import { AiAgentLogo } from '@/components/ui/ai-agent-logo'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import * as React from 'react'

export interface AiBreakdownRow {
  /** Stable key for React. */
  id: string
  /** Primary label (agent name, provider, purpose, status class). */
  label: string
  /** Optional secondary badge text (e.g. provider next to an agent). */
  badge?: string
  /** Provider name for the logo (preferred). */
  logoProvider?: string
  /** Agent name for the logo (fallback when provider is unknown). */
  logoAgent?: string
  count: number
  /** 0–100. */
  percentage: number
  /** Optional colour override for the share bar (e.g. status classes). */
  barColor?: string
}

interface AiBreakdownCardProps {
  title: string
  description?: string
  rows: AiBreakdownRow[]
  isLoading?: boolean
  error?: boolean
  /** Footer caption, e.g. "Showing top 7 agents by requests". */
  footer?: string
  /** Right-aligned header slot (toggles, "View all", etc.). */
  action?: React.ReactNode
  /** Optional per-row click (drill-down). */
  onRowClick?: (row: AiBreakdownRow) => void
  /** Empty-state copy. */
  emptyText?: string
}

/**
 * Shared "ranked share-bar list" card matching the analytics overview cards
 * (Top Pages / Browsers / …). Generic over the row shape so every AI breakdown
 * — agents, providers, crawl purpose, status — renders identically. Callers
 * pre-compute counts + percentages; this component is presentational.
 */
export function AiBreakdownCard({
  title,
  description,
  rows,
  isLoading,
  error,
  footer,
  action,
  onRowClick,
  emptyText = 'No AI crawler activity in this period',
}: AiBreakdownCardProps) {
  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between gap-2">
          <div>
            <CardTitle>{title}</CardTitle>
            {description && <CardDescription>{description}</CardDescription>}
          </div>
          {action}
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-2 py-2">
            {[...Array(5)].map((_, i) => (
              <div
                key={`sk-${i}`}
                className="flex items-center justify-between"
              >
                <div className="h-4 w-[150px] animate-pulse rounded bg-muted" />
                <div className="h-4 w-[60px] animate-pulse rounded bg-muted" />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">Failed to load</p>
          </div>
        ) : !rows.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">{emptyText}</p>
          </div>
        ) : (
          <div className="space-y-3">
            {rows.map((row) => {
              const RowTag = onRowClick ? 'button' : 'div'
              return (
                <RowTag
                  key={row.id}
                  type={onRowClick ? 'button' : undefined}
                  onClick={onRowClick ? () => onRowClick(row) : undefined}
                  className={
                    onRowClick
                      ? '-mx-1 w-full space-y-2 rounded-lg p-1 text-left transition-colors hover:bg-muted/50'
                      : 'space-y-2'
                  }
                >
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex min-w-0 items-center gap-2">
                      {(row.logoProvider || row.logoAgent) && (
                        <AiAgentLogo
                          provider={row.logoProvider}
                          agent={row.logoAgent}
                          className="size-4 shrink-0"
                        />
                      )}
                      <span className="truncate text-sm font-medium">
                        {row.label}
                      </span>
                      {row.badge && (
                        <Badge
                          variant="secondary"
                          className="shrink-0 text-[10px] font-normal"
                        >
                          {row.badge}
                        </Badge>
                      )}
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      <span className="text-sm text-muted-foreground">
                        {row.percentage.toFixed(1)}%
                      </span>
                      <span className="font-mono text-sm text-muted-foreground tabular-nums">
                        {row.count.toLocaleString()}
                      </span>
                    </div>
                  </div>
                  <div className="relative h-2 overflow-hidden rounded-full bg-muted">
                    <div
                      className="absolute inset-y-0 left-0 rounded-full transition-all duration-500"
                      style={{
                        width: `${Math.max(row.percentage, 1.5)}%`,
                        backgroundColor: row.barColor ?? 'var(--primary)',
                      }}
                    />
                  </div>
                </RowTag>
              )
            })}
          </div>
        )}
      </CardContent>
      {footer && !isLoading && !error && rows.length > 0 && (
        <CardFooter className="text-sm leading-none text-muted-foreground">
          {footer}
        </CardFooter>
      )}
    </Card>
  )
}
