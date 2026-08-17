import { Button } from '@/components/ui/button'
import {
  formatChartDateRange,
  type ChartDateRange,
} from '@/lib/chart-range-selection'
import { CalendarRange, Check, X } from 'lucide-react'

interface ChartRangeSelectionBarProps {
  range: ChartDateRange
  onApply: () => void
  onCancel: () => void
}

export function ChartRangeSelectionBar({
  range,
  onApply,
  onCancel,
}: ChartRangeSelectionBarProps) {
  return (
    <div
      className="mt-3 flex flex-col gap-3 rounded-lg border border-primary/25 bg-primary/[0.035] px-3 py-2.5 sm:flex-row sm:items-center sm:justify-between"
      role="status"
      aria-live="polite"
    >
      <div className="flex min-w-0 items-center gap-2.5">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-background text-primary shadow-sm">
          <CalendarRange className="size-4" />
        </span>
        <div className="min-w-0">
          <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            Selected timeframe
          </p>
          <p className="truncate font-mono text-xs font-medium text-foreground sm:text-sm">
            {formatChartDateRange(range)}
          </p>
        </div>
      </div>
      <div className="flex shrink-0 items-center justify-end gap-1.5">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          <X className="size-4" />
          Cancel
        </Button>
        <Button size="sm" onClick={onApply}>
          <Check className="size-4" />
          Apply range
        </Button>
      </div>
    </div>
  )
}
