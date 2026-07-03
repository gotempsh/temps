import { useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { getUnifiedTraceOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectRef, SpanRecord } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
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
import { ProjectBadge } from '@/components/traces/ProjectBadge'
import { buildSpanTree, flattenTree } from '@/utils/spanTree'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import {
  AlertCircle,
  AlertTriangle,
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
        <h4 className="mb-2 text-xs font-medium text-muted-foreground">Timing</h4>
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
  const navigate = useNavigate()
  usePageTitle('Unified trace')

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
    projectById.get(span.project_id)?.project_name ?? `Project ${span.project_id}`

  const spans: SpanRecord[] = useMemo(
    () => (data?.spans ?? []).map((a) => a.span),
    [data]
  )
  const tree = useMemo(() => buildSpanTree(spans), [spans])
  const flatSpans = useMemo(() => flattenTree(tree), [tree])

  const traceStart = useMemo(
    () => (data ? new Date(data.start_time).getTime() : 0),
    [data]
  )
  const traceEnd = useMemo(
    () => (data ? new Date(data.end_time).getTime() : 0),
    [data]
  )
  const traceDuration = traceEnd - traceStart

  const [selectedSpanId, setSelectedSpanId] = useState<string | null>(null)
  const selectedSpan = useMemo(
    () =>
      selectedSpanId
        ? (flatSpans.find((n) => n.span.span_id === selectedSpanId)?.span ?? null)
        : null,
    [selectedSpanId, flatSpans]
  )

  const renderRowBadge = (span: SpanRecord) => (
    <ProjectBadge
      projectId={span.project_id}
      name={projectName(span)}
      className="shrink-0"
    />
  )

  if (isPending) {
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-3">
          <Skeleton className="h-9 w-9" />
          <Skeleton className="h-7 w-72" />
        </div>
        <Skeleton className="h-8 w-full max-w-md" />
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          {['duration', 'spans', 'projects', 'errors'].map((label) => (
            <Skeleton key={`stat-skel-${label}`} className="h-20" />
          ))}
        </div>
        <Skeleton className="h-96" />
      </div>
    )
  }

  if (isError) {
    return (
      <div className="space-y-4">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate(-1)}
          className="gap-2"
        >
          <ArrowLeft className="h-4 w-4" />
          Back
        </Button>
        <Card>
          <CardContent className="flex items-center gap-3 p-6 text-destructive">
            <AlertCircle className="h-5 w-5" />
            <span>Failed to load unified trace: {(error as Error).message}</span>
          </CardContent>
        </Card>
      </div>
    )
  }

  if (spans.length === 0) {
    return (
      <div className="space-y-4">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate(-1)}
          className="gap-2"
        >
          <ArrowLeft className="h-4 w-4" />
          Back
        </Button>
        <EmptyState
          icon={Layers}
          title="No spans available"
          description="Spans for this trace are not available or have expired across all projects."
        />
      </div>
    )
  }

  const showSidePanel = !!selectedSpan

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center gap-2 sm:gap-3">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate(-1)}
          className="shrink-0 gap-2"
        >
          <ArrowLeft className="h-4 w-4" />
          <span className="hidden sm:inline">Back</span>
        </Button>
        <div className="min-w-0 flex-1">
          <h2 className="flex items-center gap-2 truncate text-base font-semibold sm:text-lg">
            <Layers className="h-4 w-4 shrink-0 text-muted-foreground" />
            Unified trace
          </h2>
          <p className="truncate font-mono text-xs text-muted-foreground sm:text-sm">
            {data.trace_id}
          </p>
        </div>
      </div>

      {/* Summary cards */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="mb-1 text-xs text-muted-foreground">Duration</p>
            <p className="text-lg font-semibold">
              {formatDuration(data.total_duration_ms)}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="mb-1 text-xs text-muted-foreground">Spans</p>
            <p className="text-lg font-semibold">{data.span_count}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="mb-1 text-xs text-muted-foreground">Projects</p>
            <p className="text-lg font-semibold">{data.projects.length}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="mb-1 text-xs text-muted-foreground">Errors</p>
            <p
              className={cn(
                'text-lg font-semibold',
                data.error_count > 0 && 'text-destructive'
              )}
            >
              {data.error_count}
            </p>
          </CardContent>
        </Card>
      </div>

      {/* Truncation callout */}
      {data.truncated && (
        <div className="flex items-start gap-2 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-900 dark:border-amber-900/50 dark:bg-amber-900/10 dark:text-amber-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>
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
          </span>
        </div>
      )}

      {/* Redacted / opted-out note */}
      {data.has_redacted_spans && (
        <div className="flex items-start gap-2 rounded-md border border-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
          <EyeOff className="mt-0.5 h-4 w-4 shrink-0" />
          <span>
            Some projects opted out of cross-project trace sharing, so their
            spans are not shown here.
          </span>
        </div>
      )}

      {/* Project legend */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-muted-foreground">Projects:</span>
        {data.projects.map((p) => (
          <ProjectBadge
            key={p.project_id}
            projectId={p.project_id}
            name={p.project_name}
          />
        ))}
      </div>

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
    </div>
  )
}
