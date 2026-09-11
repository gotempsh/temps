import type { ReactNode } from 'react'

/** Shared ranked-bar presentation for project and global analytics. */
export function AnalyticsBreakdownRow({
  label,
  icon,
  count,
  percentage,
  subtitle,
  onClick,
}: {
  label: string
  icon: ReactNode
  count: number
  percentage: number
  subtitle?: string
  onClick?: () => void
}) {
  const share = Number.isFinite(percentage)
    ? Math.max(0, Math.min(100, percentage))
    : 0
  const content = (
    <>
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <span className="flex size-4 shrink-0 items-center justify-center [&>svg]:size-4 [&>img]:size-4">
            {icon}
          </span>
          <div className="flex min-w-0 items-baseline gap-2">
            <span className="block truncate text-sm font-medium" title={label}>
              {label}
            </span>
            {subtitle && (
              <span
                title={subtitle}
                className="max-w-20 shrink-0 truncate text-xs text-muted-foreground lg:max-w-32"
              >
                {subtitle}
              </span>
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-sm tabular-nums text-muted-foreground">
          <span>{share.toFixed(1)}%</span>
          <span>{count.toLocaleString()}</span>
        </div>
      </div>
      <div
        className="h-1 overflow-hidden rounded-full bg-muted"
        aria-hidden="true"
      >
        <div
          className="h-full rounded-full bg-primary"
          style={{ width: `${share}%` }}
        />
      </div>
    </>
  )
  return onClick ? (
    <button
      type="button"
      onClick={onClick}
      className="w-full space-y-1 rounded-md px-1 py-1.5 text-left hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring"
    >
      {content}
    </button>
  ) : (
    <div className="space-y-1 px-1 py-1.5">{content}</div>
  )
}
