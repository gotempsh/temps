// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo } from 'react'
import { Link, useParams } from 'react-router'
import { useGoBack } from '@/hooks/useGoBack'
import { useQuery } from '@tanstack/react-query'
import { getUnifiedTraceOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  ProblemDetails,
  ProjectRef,
  SpanRecord,
} from '@/api/client/types.gen'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { EmptyState } from '@/components/ui/empty-state'
import {
  SpanWaterfall,
  formatDuration,
  formatTimestamp,
  kindLabel,
  statusIcon,
} from '@/components/traces/SpanWaterfall'
import {
  ProjectBadge,
  ProjectDot,
  ProjectLegend,
} from '@/components/traces/ProjectBadge'
import { buildSpanTree, flattenTree, traceWindow } from '@/utils/spanTree'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  useUrlState,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import {
  AlertCircle,
  ArrowLeft,
  EyeOff,
  ExternalLink,
  Layers,
} from 'lucide-react'

/** Compact detail for the selected span in the unified view. It intentionally
 *  omits correlated logs (which are project-scoped) and instead surfaces a
 *  "View in project" link back into the owning project's single-project trace. */
function UnifiedSpanDetail({
  span,
  projectName,
  projectSlug,
  traceId,
}: {
  span: SpanRecord
  projectName: string
  projectSlug: string
  traceId: string
}) {
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        {statusIcon(span.status_code)}
        <ProjectBadge projectId={span.project_id} name={projectName} />
        <span className="text-xs text-muted-foreground">
          {kindLabel(span.kind)}
        </span>
      </div>

      {span.status_message && (
        <p className="break-words rounded bg-destructive/10 p-2 text-xs text-destructive">
          {span.status_message}
        </p>
      )}

      <div>
        <h4 className="mb-2 text-xs font-medium text-muted-foreground">
          Timing
        </h4>
        <div className="grid grid-cols-2 gap-2 text-xs">
          <div>
            <span className="text-muted-foreground">Start:</span>
            <span className="ml-1 font-mono">
              {formatTimestamp(span.start_time)}
            </span>
          </div>
          <div>
            <span className="text-muted-foreground">End:</span>
            <span className="ml-1 font-mono">
              {formatTimestamp(span.end_time)}
            </span>
          </div>
          <div>
            <span className="text-muted-foreground">Duration:</span>
            <span className="ml-1 font-mono">
              {formatDuration(span.duration_ms)}
            </span>
          </div>
        </div>
      </div>

      <div>
        <h4 className="mb-2 text-xs font-medium text-muted-foreground">IDs</h4>
        <div className="space-y-1 font-mono text-xs">
          <div className="flex gap-2">
            <span className="shrink-0 text-muted-foreground">span:</span>
            <span className="break-all">{span.span_id}</span>
          </div>
          {span.parent_span_id && (
            <div className="flex gap-2">
              <span className="shrink-0 text-muted-foreground">parent:</span>
              <span className="break-all">{span.parent_span_id}</span>
            </div>
          )}
        </div>
      </div>

      <Button asChild variant="outline" size="sm" className="w-full gap-1.5">
        <Link to={`/projects/${projectSlug}/traces/${traceId}`}>
          <ExternalLink className="h-3.5 w-3.5" />
          View in {projectName}
        </Link>
      </Button>
    </div>
  )
}

export default function CrossProjectTraceDetail() {
  const { traceId } = useParams()

  const { data, isPending, isError, error } = useQuery({
    ...getUnifiedTraceOptions({ path: { trace_id: traceId || '' } }),
    enabled: !!traceId,
    retry: false,
  })

  // Owning-project lookup for span badges and the detail panel.
  const projectById = useMemo(() => {
    const map = new Map<number, ProjectRef>()
    data?.projects.forEach((p) => map.set(p.project_id, p))
    return map
  }, [data])

  const projectName = (span: SpanRecord) =>
    projectById.get(span.project_id)?.project_name ??
    `Project ${span.project_id}`

  // This view is global, so there is no single list it belongs to. The first
  // contributing project's trace list is the closest thing; before the trace
  // loads (and in the error state) fall back to the project list.
  const goBack = useGoBack(
    data?.projects[0]
      ? `/projects/${data.projects[0].project_slug}/traces`
      : '/projects'
  )

  const spans: SpanRecord[] = useMemo(
    () => (data?.spans ?? []).map((a) => a.span),
    [data]
  )
  const tree = useMemo(() => buildSpanTree(spans), [spans])
  const flatSpans = useMemo(() => flattenTree(tree), [tree])
  usePageTitle(tree[0]?.span?.name ?? 'Unified trace')

  // Derived from the spans being rendered rather than the response's
  // start_time/end_time: those are millisecond-truncated, so the header would
  // round to a duration the root span's own bar disagrees with.
  const {
    start: traceStart,
    end: traceEnd,
    duration: traceDuration,
  } = useMemo(() => traceWindow(spans), [spans])

  const { get: getUrlState, patch: patchUrlState } = useUrlState<'span'>()
  const selectedSpanId = getUrlState('span')
  const setSelectedSpanId = (spanId: string | null) =>
    patchUrlState({ span: spanId ?? undefined })
  const selectedSpan = useMemo(
    () =>
      selectedSpanId
        ? (flatSpans.find((n) => n.span.span_id === selectedSpanId)?.span ??
          null)
        : null,
    [selectedSpanId, flatSpans]
  )

  // Dot, not badge: the legend above decodes the colour, so each row keeps its
  // width for the span name instead of repeating a truncated slug.
  const renderRowBadge = (span: SpanRecord) => (
    <ProjectDot projectId={span.project_id} name={projectName(span)} />
  )

  const backAction = (
    <Button
      variant="ghost"
      size="sm"
      onClick={() => goBack()}
      className="gap-2"
    >
      <ArrowLeft className="h-4 w-4" />
      Back
    </Button>
  )

  if (isPending) {
    return (
      <Detail
        title={<Skeleton className="h-7 w-72" />}
        actions={backAction}
        facts={[0, 1, 2, 3].map(() => ({
          label: <Skeleton className="h-3 w-16" />,
          value: <Skeleton className="h-4 w-16" />,
        }))}
        main={<Skeleton className="h-96 w-full" />}
      />
    )
  }

  if (isError) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Failed to load unified trace"
        description={
          (error as ProblemDetails)?.detail ??
          (error as ProblemDetails)?.title ??
          'Unknown error'
        }
        action={backAction}
      />
    )
  }

  if (spans.length === 0) {
    return (
      <div className="space-y-4">
        {backAction}
        <EmptyState
          icon={Layers}
          title="No spans available"
          description="Spans for this trace are not available or have expired across all projects."
        />
      </div>
    )
  }

  const showSidePanel = !!selectedSpan
  const verdict: { tone: StatusTone; label: string } =
    data.error_count > 0
      ? {
          tone: 'error',
          label: `${data.error_count} error${data.error_count === 1 ? '' : 's'}`,
        }
      : { tone: 'ok', label: 'No errors' }

  const facts: DetailFact[] = [
    { label: 'Duration', value: formatDuration(data.total_duration_ms) },
    { label: 'Spans', value: data.span_count },
    { label: 'Projects', value: data.projects.length },
    {
      label: 'Errors',
      value: (
        <span
          className={cn(data.error_count > 0 && 'font-medium text-destructive')}
        >
          {data.error_count}
        </span>
      ),
    },
  ]

  return (
    <Detail
      title="Unified trace"
      description={<span className="font-mono text-xs">{data.trace_id}</span>}
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={backAction}
      facts={facts}
      main={
        <>
          {/* Truncation callout */}
          {data.truncated && (
            <Callout tone="warning" title="Trace view truncated">
              This view was truncated to stay within cross-project limits.
              {data.truncated_projects.length > 0 && (
                <>
                  {' '}
                  Spans from {data.truncated_projects.length} project
                  {data.truncated_projects.length === 1 ? '' : 's'} were dropped
                  (project id{data.truncated_projects.length === 1 ? '' : 's'}:{' '}
                  {data.truncated_projects.join(', ')}).
                </>
              )}
            </Callout>
          )}

          {/* Redacted / opted-out note */}
          {data.has_redacted_spans && (
            <Callout tone="info" title="Some spans are hidden">
              <span className="inline-flex items-center gap-1.5">
                <EyeOff className="h-3.5 w-3.5 shrink-0" />
                Some projects opted out of cross-project trace sharing, so their
                spans are not shown here.
              </span>
            </Callout>
          )}

          {/* Project legend — decodes the per-span dots in the waterfall below. */}
          <ProjectLegend projects={data.projects} />

          {/* Waterfall + selected-span detail */}
          <div
            className={cn(
              'grid gap-3',
              showSidePanel &&
                'md:grid-cols-[minmax(0,1fr)_minmax(360px,0.8fr)]'
            )}
          >
            <Card className="min-w-0">
              <CardContent className="p-0">
                <SpanWaterfall
                  flatSpans={flatSpans}
                  traceStart={traceStart}
                  traceEnd={traceEnd}
                  traceDuration={traceDuration}
                  selectedSpanId={selectedSpanId}
                  onSelect={setSelectedSpanId}
                  colorBy="status"
                  renderRowBadge={renderRowBadge}
                  className="h-[400px] sm:h-[600px]"
                />
              </CardContent>
            </Card>

            {showSidePanel && selectedSpan && (
              <Card className="min-w-0 md:sticky md:top-4 md:max-h-[600px] md:self-start md:overflow-auto">
                <CardContent className="p-4">
                  <div className="mb-3 flex items-center justify-between gap-2">
                    <h3 className="truncate text-sm font-semibold">
                      {selectedSpan.name}
                    </h3>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setSelectedSpanId(null)}
                    >
                      Close
                    </Button>
                  </div>
                  <UnifiedSpanDetail
                    span={selectedSpan}
                    projectName={projectName(selectedSpan)}
                    projectSlug={
                      projectById.get(selectedSpan.project_id)?.project_slug ??
                      String(selectedSpan.project_id)
                    }
                    traceId={data.trace_id}
                  />
                </CardContent>
              </Card>
            )}
          </div>
        </>
      }
    />
  )
}
