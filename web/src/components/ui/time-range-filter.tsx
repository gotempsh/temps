// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { DateTimeRange } from './date-time-range'
import { resolveTimeRange, serializeTimeRange } from '@/lib/time-range-filter'

/** Adapter for pages whose URL and state store a relative range string. */
export function TimeRangeFilter({
  value,
  onChange,
  disabled = false,
  maxRangeDays = 30,
  allowCustom = true,
}: {
  value: string
  onChange: (value: string) => void
  disabled?: boolean
  maxRangeDays?: number
  allowCustom?: boolean
}) {
  return (
    <fieldset disabled={disabled} className="min-w-0 disabled:opacity-50">
      <DateTimeRange
        value={resolveTimeRange(value)}
        onChange={(next) => onChange(serializeTimeRange(next))}
        maxRangeDays={maxRangeDays}
        allowCustom={allowCustom}
      />
    </fieldset>
  )
}
