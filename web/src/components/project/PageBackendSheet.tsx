// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PageBackendResponse } from '@/api/client'
import { getPageBackendOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { formatShare } from '@/lib/operation-trend'
import { traceDetailPath } from '@/lib/traces-time-window'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'

interface PageBackendSheetProps {
  projectId: number
  projectSlug: string
  /** The page path, or null when the sheet is closed. */
  path: string | null
  onClose: () => void
}

function formatMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  return `${Math.round(ms)}ms`
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border p-2">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="text-sm font-medium tabular-nums">{value}</div>
    </div>
  )
}

/**
 * A page's web vitals next to the backend requests that served its path in
 * the last 24 hours, with the slowest ones linked to their traces.
 */
export function PageBackendSheet({
  projectId,
  projectSlug,
  path,
  onClose,
}: PageBackendSheetProps) {
  const { data, isLoading, isError, error } = useQuery({
    ...getPageBackendOptions({
      query: { project_id: projectId, path: path ?? '/' },
    }),
    enabled: path !== null,
  })

  return (
    <Sheet open={path !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-xl">
        <SheetHeader>
          <SheetTitle className="break-all">{path}</SheetTitle>
          <SheetDescription>
            How much of the wait for this page is the backend, from the requests
            your server traced for this path in the last 24 hours
          </SheetDescription>
        </SheetHeader>
        <div className="space-y-4 px-4 pb-6">
          {isLoading ? (
            <Skeleton className="h-40 w-full" />
          ) : isError ? (
            <Alert variant="destructive">
              <AlertDescription>
                {error instanceof Error
                  ? error.message
                  : 'The backend timing could not be loaded.'}
              </AlertDescription>
            </Alert>
          ) : data ? (
            <PageBackendContent data={data} projectSlug={projectSlug} />
          ) : null}
        </div>
      </SheetContent>
    </Sheet>
  )
}

/** The figures and slowest requests of a loaded page backend report. */
export function PageBackendContent({
  data,
  projectSlug,
}: {
  data: PageBackendResponse
  projectSlug: string
}) {
  return (
    <>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Stat
          label="Page loads"
          value={data.vitals.page_loads.toLocaleString()}
        />
        <Stat label="TTFB p75" value={formatMs(data.vitals.ttfb_p75_ms)} />
        <Stat label="LCP p75" value={formatMs(data.vitals.lcp_p75_ms)} />
        <Stat label="Backend p75" value={formatMs(data.p75_ms)} />
      </div>
      {!data.configured ? (
        <Alert>
          <AlertDescription>
            {data.reason}{' '}
            <Link
              className="underline"
              to={`/projects/${projectSlug}/${data.setup_path ?? 'traces'}`}
            >
              See traces
            </Link>
          </AlertDescription>
        </Alert>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">
            {data.requests.toLocaleString()} backend request
            {data.requests === 1 ? '' : 's'} for this path
            {data.errors > 0 ? `, ${data.errors} failed` : ''}; p50{' '}
            {formatMs(data.p50_ms)}, p95 {formatMs(data.p95_ms)}.
            {data.backend_share_of_ttfb != null &&
              ` The backend accounts for about ${formatShare(
                Math.min(data.backend_share_of_ttfb, 1)
              )} of the time to first byte at p75.`}
          </p>
          <div className="space-y-1">
            <h3 className="text-sm font-medium">Slowest requests</h3>
            <ul className="divide-y rounded-md border">
              {data.slowest.map((request) => (
                <li key={`${request.trace_id}-${request.start_time}`}>
                  <Link
                    className="flex items-center justify-between gap-2 p-2 text-sm hover:bg-muted/50"
                    to={`/projects/${projectSlug}/traces/${traceDetailPath(request)}`}
                  >
                    <span className="min-w-0">
                      <span className="block truncate">
                        {request.span_name}
                      </span>
                      <span className="block text-xs text-muted-foreground">
                        {request.service_name} ·{' '}
                        {new Date(request.start_time).toLocaleString()}
                      </span>
                    </span>
                    <span className="flex shrink-0 items-center gap-2 tabular-nums">
                      {request.error && (
                        <Badge variant="destructive">error</Badge>
                      )}
                      {formatMs(request.duration_ms)}
                    </span>
                  </Link>
                </li>
              ))}
            </ul>
          </div>
        </>
      )}
      <p className="text-xs text-muted-foreground">
        Page loads carry no trace id, so requests are matched by path (the
        url.path or http.target attribute) and time, not one to one.
      </p>
    </>
  )
}
