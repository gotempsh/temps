// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router'
import {
  patchObservationFilters,
  positiveInteger,
  readObservationWindow,
  normalizeObservationWindow,
} from '@/lib/global-observability'
import {
  quickTimeRange,
  type QuickTimeRange,
  type DateTimeRangeValue,
} from '@/lib/date-time-range'

export function useGlobalView() {
  const [params, setParams] = useSearchParams()
  const [now, setNow] = useState(Date.now)
  const window = useMemo(
    () => readObservationWindow(params, now),
    [params, now]
  )
  useEffect(() => {
    if (
      (window.range === 'custom' &&
        (params.get('from') !== window.from ||
          params.get('to') !== window.to)) ||
      (window.range !== 'custom' && (params.has('from') || params.has('to'))) ||
      params.get('range') !== window.range
    ) {
      setParams((current) => normalizeObservationWindow(current, now), {
        replace: true,
      })
    }
  }, [params, setParams, now, window.from, window.to, window.range])
  const patch = (values: Record<string, string | undefined>) =>
    setParams((current) => patchObservationFilters(current, values), {
      replace: true,
    })
  const setTimeRange = (value: DateTimeRangeValue) => {
    if (value.preset !== 'custom') setNow(Date.parse(value.to))
    patch({
      range: value.preset,
      from: value.preset === 'custom' ? value.from : undefined,
      to: value.preset === 'custom' ? value.to : undefined,
    })
  }
  const setRange = (range: QuickTimeRange) =>
    setTimeRange(quickTimeRange(range))
  return {
    params,
    patch,
    ...window,
    page: positiveInteger(params.get('page')) ?? 1,
    projectId: positiveInteger(params.get('project_id')),
    search: params.get('q') ?? '',
    setRange,
    setTimeRange,
    setPage: (page: number) =>
      setParams((current) => {
        const next = new URLSearchParams(current)
        next.set('page', String(page))
        return next
      }),
    setCursor: (cursor?: string) =>
      setParams((current) => {
        // A cursor belongs to one exact window. Freeze a relative preset
        // before attaching it so a later reload cannot reuse a stale cursor.
        const next = normalizeObservationWindow(current, now)
        if (cursor && window.range !== 'custom') {
          next.set('from', window.from)
          next.set('to', window.to)
          next.set('range', 'custom')
        }
        if (cursor) next.set('cursor', cursor)
        else next.delete('cursor')
        return next
      }),
  }
}
export type GlobalView = ReturnType<typeof useGlobalView>
