import { LogSeverity, ProjectResponse } from '@/api/client'
import {
  getTraceOptions,
  queryLogsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { LogRecord, SpanRecord } from '@/api/client/types.gen'
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
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  ChevronDown,
  ChevronRight,
  Clock,
  AlertCircle,
  CheckCircle2,
  XCircle,
  RefreshCw,
  Bot,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'

interface TraceDetailProps {
  project: ProjectResponse
}

interface SpanTreeNode {
  span: SpanRecord
  children: SpanTreeNode[]
  depth: number
}

// Build a tree of spans from flat list using parent_span_id
function buildSpanTree(spans: SpanRecord[]): SpanTreeNode[] {
  const spanMap = new Map<string, SpanTreeNode>()
  const roots: SpanTreeNode[] = []

  // Create nodes
  for (const span of spans) {
    spanMap.set(span.span_id, { span, children: [], depth: 0 })
  }

  // Build tree
  for (const span of spans) {
    const node = spanMap.get(span.span_id)!
    if (span.parent_span_id && spanMap.has(span.parent_span_id)) {
      const parent = spanMap.get(span.parent_span_id)!
      node.depth = parent.depth + 1
      parent.children.push(node)
    } else {
      roots.push(node)
    }
  }

  // Sort children by start_time
  function sortChildren(node: SpanTreeNode) {
    node.children.sort(
      (a, b) =>
        new Date(a.span.start_time).getTime() -
        new Date(b.span.start_time).getTime()
    )
    node.children.forEach(sortChildren)
  }
  roots.sort(
    (a, b) =>
      new Date(a.span.start_time).getTime() -
      new Date(b.span.start_time).getTime()
  )
  roots.forEach(sortChildren)

  return roots
}

// Flatten tree into ordered list for rendering
function flattenTree(nodes: SpanTreeNode[]): SpanTreeNode[] {
  const result: SpanTreeNode[] = []
  function walk(node: SpanTreeNode) {
    result.push(node)
    node.children.forEach(walk)
  }
  nodes.forEach(walk)
  return result
}

function statusIcon(status?: string) {
  switch (status?.toUpperCase()) {
    case 'ERROR':
      return <XCircle className="h-3.5 w-3.5 text-red-500" />
    case 'OK':
      return <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />
    default:
      return null
  }
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

function kindLabel(kind?: string): string {
  switch (kind) {
    case 'Server':
      return 'SERVER'
    case 'Client':
      return 'CLIENT'
    case 'Producer':
      return 'PRODUCER'
    case 'Consumer':
      return 'CONSUMER'
    case 'Internal':
      return 'INTERNAL'
    default:
      return kind || 'UNSPECIFIED'
  }
}

function formatDuration(ms: number): string {
  if (ms < 1) return `${(ms * 1000).toFixed(0)}µs`
  if (ms < 1000) return `${ms.toFixed(1)}ms`
  return `${(ms / 1000).toFixed(2)}s`
}

function formatTimestamp(ts: string): string {
  const d = new Date(ts)
  return d.toLocaleString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

// Stable per-service colour (Datadog/OpenObserve colour-code spans by service).
const SERVICE_COLORS = [
  '#6366f1', // indigo
  '#10b981', // emerald
  '#f59e0b', // amber
  '#ec4899', // pink
  '#06b6d4', // cyan
  '#8b5cf6', // violet
  '#ef4444', // red
  '#14b8a6', // teal
]
function serviceColor(name?: string): string {
  if (!name) return '#94a3b8'
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) | 0
  return SERVICE_COLORS[Math.abs(h) % SERVICE_COLORS.length]
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

/** Shared span waterfall (tree + timeline bars). `colorBy="service"` colours each
 *  bar by its service and shows a service dot, like OpenObserve/Datadog. */
function SpanWaterfall({
  flatSpans,
  traceStart,
  traceEnd,
  traceDuration,
  selectedSpanId,
  onSelect,
  colorBy = 'status',
  className,
}: {
  flatSpans: SpanTreeNode[]
  traceStart: number
  traceEnd: number
  traceDuration: number
  selectedSpanId: string | null
  onSelect: (spanId: string | null) => void
  colorBy?: 'status' | 'service'
  className?: string
}) {
  return (
    <div className={cn('overflow-auto', className)}>
      <div className="sticky top-0 z-10 flex min-w-[420px] items-center border-b bg-background px-4 py-2 text-xs text-muted-foreground sm:min-w-[500px]">
        <div className="w-[120px] shrink-0 sm:w-[180px] md:w-[280px]">Span Name</div>
        <div className="flex flex-1 justify-between">
          <span>{formatTimestamp(new Date(traceStart).toISOString())}</span>
          <span>{formatDuration(traceDuration)}</span>
          <span>{formatTimestamp(new Date(traceEnd).toISOString())}</span>
        </div>
      </div>
      {flatSpans.map((node) => {
        const spanStart = new Date(node.span.start_time).getTime() - traceStart
        const spanDuration =
          new Date(node.span.end_time).getTime() -
          new Date(node.span.start_time).getTime()
        const maxBarPct = 92
        const leftPct = traceDuration > 0 ? (spanStart / traceDuration) * maxBarPct : 0
        const widthPct =
          traceDuration > 0
            ? Math.max((spanDuration / traceDuration) * maxBarPct, 0.5)
            : maxBarPct
        const isError = node.span.status_code?.toUpperCase() === 'ERROR'
        const isSelected = selectedSpanId === node.span.span_id
        const svc = node.span.resource?.service_name
        const barColor =
          colorBy === 'service' && !isError ? serviceColor(svc) : undefined
        return (
          <TooltipProvider key={node.span.span_id}>
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  type="button"
                  onClick={() => onSelect(isSelected ? null : node.span.span_id)}
                  className={cn(
                    'flex w-full min-w-[420px] items-center border-b px-4 py-1.5 text-left transition-colors hover:bg-accent/50 sm:min-w-[500px]',
                    isSelected && 'bg-accent'
                  )}
                >
                  <div
                    className="flex w-[120px] shrink-0 items-center gap-1.5 sm:w-[180px] md:w-[280px]"
                    style={{ paddingLeft: `${node.depth * 16}px` }}
                  >
                    {node.children.length > 0 ? (
                      <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
                    ) : (
                      <span className="w-3 shrink-0" />
                    )}
                    {colorBy === 'service' ? (
                      <span
                        className="h-2.5 w-2.5 shrink-0 rounded-full"
                        style={{ backgroundColor: serviceColor(svc) }}
                      />
                    ) : (
                      statusIcon(node.span.status_code)
                    )}
                    <span className="truncate text-xs">{node.span.name}</span>
                  </div>
                  <div className="relative h-6 flex-1">
                    <div
                      className={cn(
                        'absolute top-1 h-4 min-w-[2px] rounded-sm',
                        isError ? 'bg-red-500/80' : !barColor && 'bg-primary/70'
                      )}
                      style={{
                        left: `${leftPct}%`,
                        width: `${widthPct}%`,
                        backgroundColor: barColor,
                      }}
                    />
                    <span
                      className="absolute top-0.5 whitespace-nowrap text-[10px] text-muted-foreground"
                      style={{ left: `${leftPct + widthPct + 0.5}%` }}
                    >
                      {formatDuration(spanDuration)}
                    </span>
                  </div>
                </button>
              </TooltipTrigger>
              <TooltipContent side="top" className="max-w-xs">
                <div className="space-y-1">
                  <p className="font-medium">{node.span.name}</p>
                  {svc && <p className="text-xs">Service: {svc}</p>}
                  <p className="text-xs">Duration: {formatDuration(spanDuration)}</p>
                  <p className="text-xs">Kind: {kindLabel(node.span.kind)}</p>
                </div>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        )
      })}
    </div>
  )
}

interface LayoutProps {
  flatSpans: SpanTreeNode[]
  traceStart: number
  traceEnd: number
  traceDuration: number
  correlatedLogs: LogRecord[]
}

/** Shared selection hook for the layout variants. */
function useSpanSelection(flatSpans: SpanTreeNode[], correlatedLogs: LogRecord[]) {
  const [selectedSpanId, setSelectedSpanId] = useState<string | null>(null)
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
  const { flatSpans, traceStart, traceEnd, traceDuration, correlatedLogs } = props
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

  const tree = useMemo(() => buildSpanTree(spans), [spans])
  const flatSpans = useMemo(() => flattenTree(tree), [tree])

  // Calculate trace-level timing for waterfall positioning
  const traceStart = useMemo(() => {
    if (spans.length === 0) return 0
    return Math.min(...spans.map((s) => new Date(s.start_time).getTime()))
  }, [spans])

  const traceEnd = useMemo(() => {
    if (spans.length === 0) return 0
    return Math.max(...spans.map((s) => new Date(s.end_time).getTime()))
  }, [spans])

  const traceDuration = traceEnd - traceStart

  // Shared props for every trace-detail layout variant (ui.sh picker below).
  const layoutProps: LayoutProps = {
    flatSpans,
    traceStart,
    traceEnd,
    traceDuration,
    correlatedLogs,
  }

  // Services involved
  const services = useMemo(() => {
    const set = new Set<string>()
    spans.forEach((s) => {
      if (s.resource?.service_name) set.add(s.resource.service_name)
    })
    return Array.from(set)
  }, [spans])

  // Trace-level status: error if any span has ERROR, otherwise OK
  const hasError = useMemo(
    () => spans.some((s) => s.status_code?.toUpperCase() === 'ERROR'),
    [spans]
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
          <CardContent className="flex items-center justify-center p-12 text-muted-foreground">
            No spans found for this trace.
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

      {/* Summary cards */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="text-xs text-muted-foreground mb-1">Duration</p>
            <p className="text-lg font-semibold">
              {formatDuration(traceDuration)}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="text-xs text-muted-foreground mb-1">Spans</p>
            <p className="text-lg font-semibold">{spans.length}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="text-xs text-muted-foreground mb-1">Services</p>
            <p className="text-lg font-semibold">{services.length}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-3 sm:p-4">
            <p className="text-xs text-muted-foreground mb-1">Status</p>
            <div className="flex items-center gap-2">
              {statusIcon(traceStatus)}
              <span className="text-lg font-semibold">
                {hasError ? 'Error' : 'OK'}
              </span>
            </div>
          </CardContent>
        </Card>
      </div>

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
            <span className="text-xs text-muted-foreground">{spans.length}</span>
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
