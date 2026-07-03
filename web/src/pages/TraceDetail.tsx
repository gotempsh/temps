import { LogSeverity, ProjectResponse } from '@/api/client'
import {
  getCrossProjectTraceSiblingsOptions,
  getTraceOptions,
  getUnifiedTraceOptions,
  queryLogsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  CrossProjectSiblingRef,
  LogRecord,
  ProjectRef,
  SpanRecord,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { useIsMobile } from '@/components/hooks/use-mobile'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { useAssistantPageContext } from '@/components/ai/AiAssistantContext'
import {
  SpanWaterfall,
  formatDuration,
  formatTimestamp,
  kindLabel,
  serviceColor,
  statusIcon,
} from '@/components/traces/SpanWaterfall'
import { ProjectBadge } from '@/components/traces/ProjectBadge'
import { TraceStatBadges } from '@/components/traces/TraceStatBadges'
import { buildSpanTree, flattenTree } from '@/utils/spanTree'
import type { SpanTreeNode } from '@/utils/spanTree'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  ChevronRight,
  Clock,
  AlertCircle,
  Info,
  Layers,
  RefreshCw,
  Bot,
} from 'lucide-react'
import { useCallback, useMemo, type ReactNode } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'

interface TraceDetailProps {
  project: ProjectResponse
}

function logSeverityBadge(severity: LogSeverity, text?: string) {
  const label = text || severity
  switch (severity) {
    case 'FATAL':
    case 'ERROR':
      return <Badge variant="destructive">{label}</Badge>
    case 'WARN':
      return <Badge variant="warning">{label}</Badge>
    case 'INFO':
      return <Badge variant="secondary">{label}</Badge>
    default:
      return (
        <Badge variant="outline" className="text-muted-foreground">
          {label}
        </Badge>
      )
  }
}

function statusBadgeVariant(
  status?: string
): 'destructive' | 'default' | 'secondary' | 'outline' {
  switch (status?.toUpperCase()) {
    case 'ERROR':
      return 'destructive'
    case 'OK':
      return 'default'
    default:
      return 'secondary'
  }
}

/** Full detail for one span — reused by every layout (drawer, side panel, or
 *  bottom panel). Includes its own OTel logs (correlated by `span_id`). */
function SpanDetailBody({
  span,
  spanLogs,
}: {
  span: SpanRecord
  spanLogs: LogRecord[]
}) {
  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          {statusIcon(span.status_code)}
          {span.status_code?.toUpperCase() !== 'UNSET' && span.status_code && (
            <Badge variant={statusBadgeVariant(span.status_code)}>
              {span.status_code}
            </Badge>
          )}
          <Badge variant="outline">{kindLabel(span.kind)}</Badge>
          {span.resource?.service_name && (
            <span
              className="inline-flex items-center gap-1.5 text-xs text-muted-foreground"
              title={span.resource.service_name}
            >
              <span
                className="h-2.5 w-2.5 rounded-full"
                style={{ backgroundColor: serviceColor(span.resource.service_name) }}
              />
              {span.resource.service_name}
            </span>
          )}
        </div>
        {span.status_message && (
          <p className="break-words rounded bg-destructive/10 p-2 text-xs text-destructive">
            {span.status_message}
          </p>
        )}
      </div>

      <div>
        <h4 className="mb-2 text-xs font-medium text-muted-foreground">Timing</h4>
        <div className="grid grid-cols-2 gap-2 text-xs">
          <div>
            <span className="text-muted-foreground">Start:</span>
            <span className="ml-1 font-mono">{formatTimestamp(span.start_time)}</span>
          </div>
          <div>
            <span className="text-muted-foreground">End:</span>
            <span className="ml-1 font-mono">{formatTimestamp(span.end_time)}</span>
          </div>
          <div>
            <span className="text-muted-foreground">Duration:</span>
            <span className="ml-1 font-mono">{formatDuration(span.duration_ms)}</span>
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
          <div className="flex gap-2">
            <span className="shrink-0 text-muted-foreground">trace:</span>
            <span className="break-all">{span.trace_id}</span>
          </div>
          {span.parent_span_id && (
            <div className="flex gap-2">
              <span className="shrink-0 text-muted-foreground">parent:</span>
              <span className="break-all">{span.parent_span_id}</span>
            </div>
          )}
        </div>
      </div>

      {span.attributes && Object.keys(span.attributes).length > 0 && (
        <Collapsible defaultOpen>
          <CollapsibleTrigger className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
            <ChevronRight className="h-3 w-3 transition-transform data-[state=open]:rotate-90" />
            Attributes ({Object.keys(span.attributes).length})
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="mt-2 space-y-1">
              {Object.entries(span.attributes).map(([key, value]) => (
                <div
                  key={key}
                  className="flex gap-2 border-b border-border/50 py-0.5 font-mono text-xs"
                >
                  <span className="shrink-0 text-muted-foreground">{key}:</span>
                  <span className="break-all">
                    {typeof value === 'object' ? JSON.stringify(value) : String(value)}
                  </span>
                </div>
              ))}
            </div>
          </CollapsibleContent>
        </Collapsible>
      )}

      {span.events && span.events.length > 0 && (
        <Collapsible defaultOpen>
          <CollapsibleTrigger className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
            <ChevronRight className="h-3 w-3 transition-transform data-[state=open]:rotate-90" />
            Events ({span.events.length})
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="mt-2 space-y-2">
              {span.events.map((event) => (
                <div
                  key={`evt-${event.name}-${event.timestamp}`}
                  className="space-y-1 rounded border p-2 text-xs"
                >
                  <div className="flex items-center gap-2">
                    <Clock className="h-3 w-3 text-muted-foreground" />
                    <span className="font-medium">{event.name}</span>
                    <span className="font-mono text-muted-foreground">
                      {formatTimestamp(event.timestamp)}
                    </span>
                  </div>
                  {event.attributes && Object.keys(event.attributes).length > 0 && (
                    <div className="ml-5 space-y-0.5">
                      {Object.entries(event.attributes).map(([k, v]) => (
                        <div key={k} className="flex gap-2 font-mono">
                          <span className="shrink-0 text-muted-foreground">{k}:</span>
                          <span className="break-all">
                            {typeof v === 'object' ? JSON.stringify(v) : String(v)}
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          </CollapsibleContent>
        </Collapsible>
      )}

      {spanLogs.length > 0 && (
        <Collapsible defaultOpen>
          <CollapsibleTrigger className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
            <ChevronRight className="h-3 w-3 transition-transform data-[state=open]:rotate-90" />
            Logs ({spanLogs.length})
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="mt-2 space-y-1 font-mono text-xs">
              {spanLogs.map((log, i) => (
                <div
                  key={`spanlog-${i}`}
                  className="flex items-start gap-2 border-b border-border/50 py-1 last:border-b-0"
                >
                  <span className="shrink-0 tabular-nums text-muted-foreground">
                    {formatTimestamp(log.timestamp)}
                  </span>
                  <span className="shrink-0">
                    {logSeverityBadge(log.severity, log.severity_text)}
                  </span>
                  <span className="min-w-0 flex-1 break-words text-foreground">
                    {log.body}
                  </span>
                </div>
              ))}
            </div>
          </CollapsibleContent>
        </Collapsible>
      )}
    </div>
  )
}

interface LayoutProps {
  flatSpans: SpanTreeNode[]
  traceStart: number
  traceEnd: number
  traceDuration: number
  correlatedLogs: LogRecord[]
  /** Cross-project unified view: tag each span with its owning project. */
  renderRowBadge?: (span: SpanRecord) => ReactNode
  /** Cross-project unified view: accent/dim rows (e.g. de-emphasise other projects). */
  rowClassName?: (span: SpanRecord) => string | undefined
}

/** Shared selection hook for the layout variants. */
function useSpanSelection(flatSpans: SpanTreeNode[], correlatedLogs: LogRecord[]) {
  // Keep the selected span in the URL (`?span=<id>`) so a specific span is
  // shareable/linkable — but write it with `replace` (not push) so clicking
  // through spans never stacks history entries. The Back button then returns to
  // the previous page (e.g. the trace list), it doesn't undo span selections.
  const [searchParams, setSearchParams] = useSearchParams()
  const selectedSpanId = searchParams.get('span')
  const setSelectedSpanId = useCallback(
    (id: string | null) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev)
          if (id) next.set('span', id)
          else next.delete('span')
          return next
        },
        { replace: true }
      )
    },
    [setSearchParams]
  )
  const span = useMemo(
    () =>
      selectedSpanId
        ? (flatSpans.find((n) => n.span.span_id === selectedSpanId)?.span ?? null)
        : null,
    [selectedSpanId, flatSpans]
  )
  const logs = useMemo(
    () =>
      selectedSpanId
        ? correlatedLogs.filter((l) => l.span_id === selectedSpanId)
        : [],
    [selectedSpanId, correlatedLogs]
  )
  return { selectedSpanId, setSelectedSpanId, span, logs }
}

/** The trace Spans view: a service-coloured waterfall on the left and the
 *  selected span's detail on the right (full width when none is selected). */
function SpansView(props: LayoutProps) {
  const {
    flatSpans,
    traceStart,
    traceEnd,
    traceDuration,
    correlatedLogs,
    renderRowBadge,
    rowClassName,
  } = props
  const { selectedSpanId, setSelectedSpanId, span, logs } = useSpanSelection(
    flatSpans,
    correlatedLogs
  )
  // On a phone there isn't room for a side panel, and an inline panel below the
  // waterfall would land off-screen (unfocused). So below `md` the detail opens
  // in a focused drawer; at `md`+ it's the persistent side panel.
  const isMobile = useIsMobile()
  const showSidePanel = !!span && !isMobile
  return (
    <div
      className={cn(
        'grid gap-3',
        showSidePanel && 'md:grid-cols-[minmax(0,1fr)_minmax(440px,0.9fr)]'
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
            colorBy="service"
            renderRowBadge={renderRowBadge}
            rowClassName={rowClassName}
            className="h-[400px] sm:h-[600px]"
          />
        </CardContent>
      </Card>

      {/* Desktop / tablet: persistent side panel. */}
      {showSidePanel && span && (
        <Card className="min-w-0 md:sticky md:top-4 md:max-h-[600px] md:self-start md:overflow-auto">
          <CardContent className="p-4">
            <div className="mb-3 flex items-center justify-between gap-2">
              <h3 className="truncate text-sm font-semibold">{span.name}</h3>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setSelectedSpanId(null)}
              >
                Close
              </Button>
            </div>
            <SpanDetailBody span={span} spanLogs={logs} />
          </CardContent>
        </Card>
      )}

      {/* Mobile: focused drawer so the selected span's detail is brought to the
          front instead of stacking off-screen below the waterfall. */}
      <Sheet
        open={!!span && isMobile}
        onOpenChange={(o) => !o && setSelectedSpanId(null)}
      >
        <SheetContent side="right" className="w-full overflow-y-auto sm:max-w-md">
          <SheetHeader>
            <SheetTitle className="truncate pr-6 text-left text-base">
              {span?.name}
            </SheetTitle>
          </SheetHeader>
          {span && (
            <div className="mt-4">
              <SpanDetailBody span={span} spanLogs={logs} />
            </div>
          )}
        </SheetContent>
      </Sheet>
    </div>
  )
}

/** ADR-027: a trace that spans multiple projects. Surfaces the sibling projects
 *  and an inline switch between "this project only" and the whole merged trace —
 *  no navigation to a separate page; the choice lives in the `?view` param. */
function CrossProjectBar({
  siblings,
  traceId,
  showUnified,
  onSetView,
}: {
  siblings: CrossProjectSiblingRef[]
  traceId: string
  showUnified: boolean
  onSetView: (v: 'project' | 'unified') => void
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1.5 rounded-md border border-blue-200 bg-blue-50 px-3 py-2 text-sm text-blue-900 dark:border-blue-900/50 dark:bg-blue-900/10 dark:text-blue-200">
      <Info className="h-4 w-4 shrink-0" />
      <span className="font-medium">This trace also has spans in:</span>
      {siblings.map((s) => (
        <Link
          key={s.project_id}
          to={`/projects/${s.project_slug}/traces/${traceId}`}
          className="rounded bg-blue-100 px-1.5 py-0.5 font-medium underline-offset-2 hover:underline dark:bg-blue-900/40"
        >
          {s.project_name}
        </Link>
      ))}
      <div className="ml-auto inline-flex rounded-md border border-blue-300/60 bg-white/70 p-0.5 dark:border-blue-800 dark:bg-blue-950/40">
        <button
          type="button"
          onClick={() => onSetView('project')}
          aria-pressed={!showUnified}
          className={cn(
            'rounded px-2.5 py-1 text-xs font-medium transition-colors',
            !showUnified
              ? 'bg-blue-600 text-white'
              : 'hover:bg-blue-100 dark:hover:bg-blue-900/50'
          )}
        >
          This project
        </button>
        <button
          type="button"
          onClick={() => onSetView('unified')}
          aria-pressed={showUnified}
          className={cn(
            'inline-flex items-center gap-1 rounded px-2.5 py-1 text-xs font-medium transition-colors',
            showUnified
              ? 'bg-blue-600 text-white'
              : 'hover:bg-blue-100 dark:hover:bg-blue-900/50'
          )}
        >
          <Layers className="h-3.5 w-3.5" />
          Whole trace
        </button>
      </div>
    </div>
  )
}

export default function TraceDetail({ project }: TraceDetailProps) {
  const { traceId } = useParams()
  const navigate = useNavigate()

  const { data, isLoading, isFetching, error, refetch } = useQuery({
    ...getTraceOptions({
      path: {
        project_id: project.id!,
        trace_id: traceId || '',
      },
    }),
    enabled: !!project.id && !!traceId,
  })

  // OTel logs correlated to this trace (emitted within its spans). The query is
  // by trace_id only — no time window — so logs are matched regardless of when
  // the trace ran. Returned newest-first; we render oldest-first to read along
  // with the waterfall.
  const { data: logsData } = useQuery({
    ...queryLogsOptions({
      query: { project_id: project.id, trace_id: traceId || '', limit: 200 },
    }),
    enabled: !!project.id && !!traceId,
  })
  const correlatedLogs = useMemo(
    () => [...(logsData?.data ?? [])].reverse(),
    [logsData],
  )

  const spans: SpanRecord[] = useMemo(() => {
    if (!data) return []
    // TracesResponse has { data: SpanRecord[], count: number }
    if (data.data) return data.data
    return []
  }, [data])

  // Phase 1 cross-project discovery: does this same trace_id have spans in OTHER
  // projects? Fired only once the primary spans have loaded so it never delays
  // the waterfall. `retry: false` + swallowed errors mean a failure renders
  // nothing (the banner is purely additive, never blocking).
  const { data: siblingsData } = useQuery({
    ...getCrossProjectTraceSiblingsOptions({
      path: { trace_id: traceId || '' },
      query: { exclude_project_id: project.id },
    }),
    enabled: !!traceId && spans.length > 0,
    retry: false,
  })
  const siblings: CrossProjectSiblingRef[] = siblingsData?.siblings ?? []

  // Cross-project "whole trace" view. When this trace_id also has spans in other
  // projects we default to the merged trace inline; `?view=project` collapses to
  // just this project. The param keeps the view shareable and is written with
  // `replace` so it never stacks history.
  const [searchParams, setSearchParams] = useSearchParams()
  const isCrossProject = siblings.length > 0
  const showUnified = isCrossProject && searchParams.get('view') !== 'project'
  const setView = useCallback(
    (v: 'project' | 'unified') => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev)
          next.set('view', v)
          return next
        },
        { replace: true }
      )
    },
    [setSearchParams]
  )

  const { data: unifiedData } = useQuery({
    ...getUnifiedTraceOptions({ path: { trace_id: traceId || '' } }),
    enabled: !!traceId && showUnified,
    retry: false,
  })
  const usingUnified = showUnified && !!unifiedData

  // Spans actually rendered: the whole cross-project trace when unified,
  // otherwise just this project's spans.
  const displaySpans: SpanRecord[] = usingUnified
    ? (unifiedData?.spans ?? []).map((a) => a.span)
    : spans

  const projectById = useMemo(() => {
    const m = new Map<number, ProjectRef>()
    unifiedData?.projects.forEach((p) => m.set(p.project_id, p))
    return m
  }, [unifiedData])

  const tree = useMemo(() => buildSpanTree(displaySpans), [displaySpans])
  const flatSpans = useMemo(() => flattenTree(tree), [tree])

  // Calculate trace-level timing for waterfall positioning
  const traceStart = useMemo(() => {
    if (displaySpans.length === 0) return 0
    return Math.min(...displaySpans.map((s) => new Date(s.start_time).getTime()))
  }, [displaySpans])

  const traceEnd = useMemo(() => {
    if (displaySpans.length === 0) return 0
    return Math.max(...displaySpans.map((s) => new Date(s.end_time).getTime()))
  }, [displaySpans])

  const traceDuration = traceEnd - traceStart

  // Shared props for every trace-detail layout variant (ui.sh picker below).
  // In the unified view, tag each span with its project and de-emphasise spans
  // that belong to other projects so the entry-point project stays legible.
  const layoutProps: LayoutProps = {
    flatSpans,
    traceStart,
    traceEnd,
    traceDuration,
    correlatedLogs,
    renderRowBadge: usingUnified
      ? (span) => (
          <ProjectBadge
            projectId={span.project_id}
            name={
              projectById.get(span.project_id)?.project_name ??
              `Project ${span.project_id}`
            }
            className="shrink-0"
          />
        )
      : undefined,
    rowClassName: usingUnified
      ? (span) => (span.project_id === project.id ? undefined : 'opacity-70')
      : undefined,
  }

  // Services involved
  const services = useMemo(() => {
    const set = new Set<string>()
    spans.forEach((s) => {
      if (s.resource?.service_name) set.add(s.resource.service_name)
    })
    return Array.from(set)
  }, [spans])

  // Trace-level status: error if any displayed span has ERROR, otherwise OK
  const hasError = useMemo(
    () => displaySpans.some((s) => s.status_code?.toUpperCase() === 'ERROR'),
    [displaySpans]
  )
  const traceStatus = hasError ? 'ERROR' : 'OK'

  // Count spans that carry OpenTelemetry GenAI semantic-convention attributes
  // (`gen_ai.*`). When present, this trace has LLM calls whose prompts and
  // responses read far better in the dedicated AI conversation view than in the
  // raw waterfall — so we surface a jump to it.
  const genAiSpanCount = useMemo(
    () =>
      spans.filter((s) =>
        Object.keys(s.attributes ?? {}).some((k) => k.startsWith('gen_ai.'))
      ).length,
    [spans]
  )

  // Tell the assistant what the user is looking at, so "this trace"/"these
  // logs" resolve without restating. Ephemeral framing — see the chat dock.
  const assistantContext = useMemo(() => {
    if (!traceId || spans.length === 0) return null
    const rootName = tree[0]?.span?.name ?? '(root span)'
    return [
      'The user is viewing a distributed trace (OpenTelemetry) in the Temps console.',
      `Project: "${project.name}" (slug: ${project.slug}, id: ${project.id}).`,
      `Trace id: ${traceId}.`,
      `Root span: ${rootName}. Duration: ${formatDuration(traceDuration)}. ${spans.length} span(s) across service(s): ${services.join(', ') || 'unknown'}. Status: ${hasError ? 'ERROR' : 'OK'}.`,
      'If they say "this trace"/"this span"/"these logs", they mean this one. You can pull details with the temps CLI: `traces get_trace --trace_id <id>` and `telemetry-logs query_logs --trace_id <id>`.',
    ].join('\n')
  }, [traceId, project, tree, traceDuration, spans.length, services, hasError])
  useAssistantPageContext(assistantContext, 'this trace')

  if (isLoading) {
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-3">
          <Skeleton className="h-9 w-9" />
          <Skeleton className="h-7 w-64" />
        </div>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          {['duration', 'spans', 'services', 'status'].map((label) => (
            <Skeleton key={`stat-skel-${label}`} className="h-20" />
          ))}
        </div>
        <Skeleton className="h-96" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="space-y-4">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate(-1)}
          className="gap-2"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to Traces
        </Button>
        <Card>
          <CardContent className="flex items-center gap-3 p-6 text-destructive">
            <AlertCircle className="h-5 w-5" />
            <span>Failed to load trace: {(error as Error).message}</span>
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
          Back to Traces
        </Button>
        <Card>
          <CardContent className="flex items-center justify-center p-12 text-center text-muted-foreground">
            Spans for this trace are not available or have expired.
          </CardContent>
        </Card>
      </div>
    )
  }

  const rootSpan = flatSpans[0]?.span

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
        <div className="flex-1 min-w-0">
          <h2 className="truncate text-base font-semibold sm:text-lg">
            {rootSpan?.name || traceId}
          </h2>
          <p className="truncate font-mono text-xs text-muted-foreground sm:text-sm">
            {traceId}
          </p>
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="shrink-0"
          onClick={() => refetch()}
          disabled={isFetching}
        >
          <RefreshCw className={cn('h-4 w-4', isFetching && 'animate-spin')} />
        </Button>
      </div>

      {/* Summary badges (compact metric row instead of a heavy card grid). */}
      <TraceStatBadges
        durationMs={traceDuration}
        spanCount={displaySpans.length}
        projectCount={usingUnified ? unifiedData?.projects.length : undefined}
        serviceCount={usingUnified ? undefined : services.length}
        status={traceStatus}
        errorCount={usingUnified ? unifiedData?.error_count : undefined}
        redacted={usingUnified ? unifiedData?.has_redacted_spans : false}
        truncated={usingUnified ? unifiedData?.truncated : false}
      />

      {/* Cross-project view switch (ADR-027): this trace has spans in other
          projects — toggle between this project and the whole merged trace,
          inline (no navigation). The choice is in the shareable `?view` param. */}
      {isCrossProject && (
        <CrossProjectBar
          siblings={siblings}
          traceId={traceId || ''}
          showUnified={showUnified}
          onSetView={setView}
        />
      )}

      {/* AI conversation jump — when this trace has GenAI (LLM) spans, the
          prompts/responses read far better in the dedicated AI view than in the
          raw waterfall below. One click, trace pre-selected. */}
      {genAiSpanCount > 0 && (
        <button
          type="button"
          onClick={() =>
            navigate(
              `/projects/${project.slug}/ai-gateway?tab=activity&trace=${traceId}`
            )
          }
          className="group flex w-full items-center gap-3 rounded-lg border border-primary/30 bg-primary/5 p-3 text-left transition-colors hover:bg-primary/10"
        >
          <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
            <Bot className="h-4 w-4" />
          </span>
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">
              This trace has {genAiSpanCount} AI invocation
              {genAiSpanCount === 1 ? '' : 's'}
            </p>
            <p className="text-xs text-muted-foreground">
              See the prompts, responses, tokens, and tool calls as a conversation.
            </p>
          </div>
          <span className="hidden items-center gap-1 text-xs font-medium text-primary sm:flex">
            View AI conversation
            <ChevronRight className="h-4 w-4 transition-transform group-hover:translate-x-0.5" />
          </span>
          <ChevronRight className="h-4 w-4 shrink-0 text-primary sm:hidden" />
        </button>
      )}

      {/* Spans / Logs tabs */}
      <Tabs defaultValue="spans" className="w-full">
        <TabsList>
          <TabsTrigger value="spans" className="gap-1.5">
            Spans
            <span className="text-xs text-muted-foreground">
              {displaySpans.length}
            </span>
          </TabsTrigger>
          <TabsTrigger value="logs" className="gap-1.5">
            Logs
            {correlatedLogs.length > 0 && (
              <span className="text-xs text-muted-foreground">
                {correlatedLogs.length}
              </span>
            )}
          </TabsTrigger>
        </TabsList>

        {/* Spans: waterfall on the left, span detail on the right when one is
            selected (full width otherwise). */}
        <TabsContent value="spans" className="mt-4">
          <SpansView {...layoutProps} />
        </TabsContent>

        {/* Logs: the correlated logs, full width. */}
        <TabsContent value="logs" className="mt-4">
          <Card>
            <CardContent className="p-0">
              {correlatedLogs.length === 0 ? (
                <p className="p-6 text-sm text-muted-foreground">
                  No logs are correlated to this trace. Logs appear here when your
                  app emits them within the trace&apos;s spans via OpenTelemetry.
                </p>
              ) : (
                <div className="font-mono text-xs">
                  {correlatedLogs.map((log, i) => (
                    <div
                      key={i}
                      className="flex items-start gap-3 border-b px-4 py-1.5 last:border-b-0 sm:px-6"
                    >
                      <span className="shrink-0 tabular-nums text-muted-foreground">
                        {formatTimestamp(log.timestamp)}
                      </span>
                      <span className="shrink-0">
                        {logSeverityBadge(log.severity, log.severity_text)}
                      </span>
                      <span className="min-w-0 flex-1 break-words text-foreground">
                        {log.body}
                      </span>
                    </div>
                  ))}
                  <div className="px-4 py-2 sm:px-6">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() =>
                        navigate(
                          `/projects/${project.slug}/telemetry-logs?trace=${traceId}`,
                        )
                      }
                    >
                      Open in Logs explorer →
                    </Button>
                  </div>
                </div>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>

    </div>
  )
}
