// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import type { DateRange } from 'react-day-picker'
import { DateTimeRange } from './date-time-range'
import { quickTimeRange, type DateTimeRangeValue } from '@/lib/date-time-range'

/** Compatibility adapter: date-filter callers receive exact Date bounds. */
export function DateRangePicker({
  date,
  onDateChange,
  className,
}: {
  date?: DateRange
  onDateChange?: (date: DateRange | undefined) => void
  className?: string
  showTime?: boolean
}) {
  const [selection, setSelection] = useState<DateTimeRangeValue>()
  const from = date?.from?.toISOString()
  const to = date?.to?.toISOString()
  const value: DateTimeRangeValue =
    from && to
      ? {
          from,
          to,
          preset:
            selection?.from === from && selection?.to === to
              ? selection.preset
              : 'custom',
        }
      : quickTimeRange('1d')
  return (
    <div className={className}>
      <DateTimeRange
        value={value}
        active={Boolean(from && to)}
        maxRangeDays={3650}
        onChange={(next) => {
          setSelection(next)
          onDateChange?.({ from: new Date(next.from), to: new Date(next.to) })
        }}
      />
    </div>
  )
}
