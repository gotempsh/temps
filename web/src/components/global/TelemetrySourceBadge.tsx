// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Cloud, CloudOff, HardDrive } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'

/**
 * Where a telemetry query's *page of results* came from — never inferred
 * from an unrelated setting (e.g. "Export telemetry to Cloud" being on).
 * The backend sets this per response, on the request that actually ran.
 */
export type TelemetrySourceKind = 'this_instance' | 'temps_cloud'

/** Whether the source that served this response is currently healthy. */
export type TelemetrySourceStatus = 'live' | 'unavailable'

export interface TelemetrySource {
  kind: TelemetrySourceKind
  /** Cloud region code, e.g. `"eu-1"`. Absent for `this_instance`. */
  region?: string | null
  status: TelemetrySourceStatus
}

/**
 * Best-effort short label for a region code. Falls back to the raw code
 * uppercased so an unrecognized region still renders something, rather than
 * the badge silently dropping the region.
 */
function regionLabel(region: string): string {
  const known: Record<string, string> = { 'eu-1': 'EU', 'us-1': 'US' }
  return known[region] ?? region.toUpperCase()
}

/**
 * Compact indicator for which store served a telemetry query's results.
 * Shared across Traces, Analytics, Errors, Logs and Metrics so the five
 * pages can never drift into inconsistent labelling.
 */
export function TelemetrySourceBadge({ source }: { source: TelemetrySource }) {
  if (source.kind === 'temps_cloud' && source.status === 'unavailable') {
    return (
      <TooltipProvider>
        <Tooltip>
          <TooltipTrigger asChild>
            <Badge
              variant="destructive"
              className="gap-1 font-normal"
              aria-label="Cloud unavailable"
            >
              <CloudOff className="size-3" />
              Cloud unavailable
            </Badge>
          </TooltipTrigger>
          <TooltipContent>
            Temps Cloud did not respond to this query. Results are not shown
            rather than silently substituting local data under a Cloud label.
          </TooltipContent>
        </Tooltip>
      </TooltipProvider>
    )
  }

  if (source.kind === 'temps_cloud') {
    const region = source.region ? regionLabel(source.region) : null
    return (
      <TooltipProvider>
        <Tooltip>
          <TooltipTrigger asChild>
            <Badge
              variant="outline"
              className="gap-1 font-normal"
              aria-label={region ? `Temps Cloud · ${region}` : 'Temps Cloud'}
            >
              <Cloud className="size-3" />
              Temps Cloud{region ? ` · ${region}` : ''}
            </Badge>
          </TooltipTrigger>
          <TooltipContent>
            Fetched from Temps Cloud. This OSS instance remains your control
            surface.
          </TooltipContent>
        </Tooltip>
      </TooltipProvider>
    )
  }

  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <Badge
            variant="secondary"
            className="gap-1 font-normal"
            aria-label="This instance"
          >
            <HardDrive className="size-3" />
            This instance
          </Badge>
        </TooltipTrigger>
        <TooltipContent>
          Fetched from this instance&apos;s local telemetry store.
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}
