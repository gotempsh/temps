// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getOperationBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { SegmentedControl } from '@/components/ui/segmented-control'
import { Skeleton } from '@/components/ui/skeleton'
import { useDebounce } from '@/hooks/useDebounce'
import { formatShare } from '@/lib/operation-trend'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'

interface OperationBreakdownProps {
  projectId: number
  serviceName: string
  spanName: string
}

const SAMPLES = [
  { value: 'recent', label: 'Recent calls' },
  { value: 'slow', label: 'Slow calls (p95+)' },
] as const

type Sample = (typeof SAMPLES)[number]['value']

function formatMs(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  return `${ms.toFixed(ms >= 10 ? 0 : 1)}ms`
}

function ShareBar({ share }: { share: number }) {
  return (
    <div className="h-1.5 w-full rounded-full bg-muted" aria-hidden>
      <div
        className="h-1.5 rounded-full bg-primary"
        style={{ width: `${Math.min(100, Math.max(0, share * 100))}%` }}
      />
    </div>
  )
}

/**
 * Where the time of one operation goes: each downstream call's exclusive
 * time over a sample of traces, and optionally the calls split by an
 * attribute of their own.
 */
export function OperationBreakdown({
  projectId,
  serviceName,
  spanName,
}: OperationBreakdownProps) {
  const [sample, setSample] = useState<Sample>('recent')
  const [attribute, setAttribute] = useState('')
  const debouncedAttribute = useDebounce(attribute.trim(), 400)

  const { data, isLoading, isError, error } = useQuery({
    ...getOperationBreakdownOptions({
      query: {
        project_id: projectId,
        service_name: serviceName,
        span_name: spanName,
        sample,
        ...(debouncedAttribute ? { attribute: debouncedAttribute } : {}),
      },
    }),
  })

  return (
    <section className="space-y-3" aria-label="Where the time goes">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="text-sm font-medium">Where the time goes</h3>
        <SegmentedControl
          label="Calls to sample"
          value={sample}
          onValueChange={setSample}
          options={SAMPLES}
        />
      </div>
      {isLoading ? (
        <Skeleton className="h-32 w-full" />
      ) : isError ? (
        <p className="text-sm text-destructive">
          {error instanceof Error
            ? error.message
            : 'The breakdown could not be loaded.'}
        </p>
      ) : !data || data.instances === 0 ? (
        <p className="text-sm text-muted-foreground">
          No {sample === 'slow' ? 'slow ' : ''}call of this operation was traced
          in the last 24 hours, so there is nothing to break down.
        </p>
      ) : (
        <>
          <p className="text-xs text-muted-foreground">
            {data.instances.toLocaleString()} call
            {data.instances === 1 ? '' : 's'} in {data.traces} trace
            {data.traces === 1 ? '' : 's'} (at most {data.max_traces})
            {data.slow_threshold_ms != null
              ? `, each ${formatMs(data.slow_threshold_ms)} or slower`
              : ''}
            . Each span is charged only the time its own children do not cover;
            parallel calls can add up to more than 100%.
          </p>
          <ul className="space-y-2">
            <li className="space-y-1">
              <div className="flex items-center justify-between gap-2 text-sm">
                <span className="truncate">
                  {spanName}{' '}
                  <span className="text-muted-foreground">(own time)</span>
                </span>
                <span className="shrink-0 tabular-nums text-muted-foreground">
                  {formatShare(data.self_share)}
                </span>
              </div>
              <ShareBar share={data.self_share} />
            </li>
            {data.groups.map((group) => (
              <li
                key={`${group.service_name}-${group.span_name}-${group.category}`}
                className="space-y-1"
              >
                <div className="flex items-center justify-between gap-2 text-sm">
                  <span className="flex min-w-0 items-center gap-2">
                    <Badge variant="outline" className="shrink-0">
                      {group.category}
                    </Badge>
                    <span className="truncate" title={group.span_name}>
                      {group.span_name}
                    </span>
                    {group.service_name &&
                      group.service_name !== serviceName && (
                        <span className="hidden truncate text-xs text-muted-foreground sm:inline">
                          {group.service_name}
                        </span>
                      )}
                  </span>
                  <span className="shrink-0 tabular-nums text-muted-foreground">
                    {group.calls_per_instance >= 1.5
                      ? `${group.calls_per_instance.toFixed(1)}× · `
                      : ''}
                    {formatMs(group.avg_ms)} · {formatShare(group.share)}
                  </span>
                </div>
                <ShareBar share={group.share} />
              </li>
            ))}
          </ul>
        </>
      )}
      <div className="space-y-2">
        <Input
          aria-label="Split calls by attribute"
          placeholder="Split calls by an attribute, e.g. http.route or tenant.id"
          value={attribute}
          onChange={(e) => setAttribute(e.target.value)}
        />
        {data && data.attribute && data.by_attribute.length > 0 && (
          <ul className="space-y-1 text-sm">
            {data.by_attribute.map((slice) => (
              <li
                key={slice.value ?? '(none)'}
                className="flex items-center justify-between gap-2"
              >
                <span className="truncate">
                  {slice.value ?? (
                    <span className="text-muted-foreground">(not set)</span>
                  )}
                </span>
                <span className="shrink-0 tabular-nums text-muted-foreground">
                  {slice.calls}× · avg {formatMs(slice.avg_ms)} · max{' '}
                  {formatMs(slice.max_ms)} · {formatShare(slice.share)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  )
}
