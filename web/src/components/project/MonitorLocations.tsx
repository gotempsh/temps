// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MonitorLocationResult, MonitorResponse } from '@/api/client'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { formatDistanceToNow } from 'date-fns'
import { locationLabel } from './target-monitor'

const STATE: Record<string, { label: string; dot: string }> = {
  operational: { label: 'Up', dot: 'bg-emerald-500' },
  degraded: { label: 'Degraded', dot: 'bg-amber-500' },
  major_outage: { label: 'Down', dot: 'bg-red-500' },
}

function LocationRow({ result }: { result: MonitorLocationResult }) {
  const state = result.status
    ? STATE[result.status]
    : { label: 'No answer', dot: 'bg-muted-foreground/40' }
  return (
    <li className="flex flex-col gap-1 py-2 sm:flex-row sm:items-start sm:gap-4">
      <div className="flex min-w-0 items-center gap-2 sm:w-56">
        <span className={`h-2 w-2 shrink-0 rounded-full ${state.dot}`} />
        <span className="truncate text-sm font-medium">
          {locationLabel(result)}
        </span>
      </div>
      <div className="flex min-w-0 flex-1 flex-col text-sm text-muted-foreground">
        <span>
          {state.label}
          {result.response_time_ms != null &&
            ` · ${result.response_time_ms} ms`}
        </span>
        {result.message && (
          <span className="[overflow-wrap:anywhere]">{result.message}</span>
        )}
      </div>
    </li>
  )
}

/** What each location saw on the latest check (ADR 108 D5). */
export function MonitorLocations({ monitor }: { monitor: MonitorResponse }) {
  if (!monitor.probe_from_nodes) return null
  const latest = monitor.location_results
  return (
    <Card>
      <CardHeader>
        <CardTitle>Locations</CardTitle>
        <CardDescription>
          {latest
            ? `Latest check ${formatDistanceToNow(new Date(latest.checked_at), { addSuffix: true })}`
            : 'Not checked since its last change'}
        </CardDescription>
      </CardHeader>
      {latest && (
        <CardContent>
          <ul className="divide-y">
            {latest.results.map((result) => (
              <LocationRow
                key={`${result.node_id ?? 'control'}-${result.location}`}
                result={result}
              />
            ))}
          </ul>
        </CardContent>
      )}
    </Card>
  )
}
