import { ProjectResponse } from '@/api/client'
import { getTraceOptions } from '@/api/client/@tanstack/react-query.gen'
import type { SpanRecord } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
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
    fractionalSecondDigits: 3,
  })
}

export default function TraceDetail({ project }: TraceDetailProps) {
  const { traceId } = useParams()
  const navigate = useNavigate()
  const [selectedSpanId, setSelectedSpanId] = useState<string | null>(null)

  const { data, isLoading, isFetching, error, refetch } = useQuery({
    ...getTraceOptions({
      path: {
        project_id: project.id!,
        trace_id: traceId || '',
      },
    }),
    enabled: !!project.id && !!traceId,
  })

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

  const selectedSpan = useMemo(() => {
    if (!selectedSpanId) return null
    return spans.find((s) => s.span_id === selectedSpanId) || null
  }, [spans, selectedSpanId])

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
      <div className="flex items-center gap-3">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate(-1)}
          className="gap-2"
        >
          <ArrowLeft className="h-4 w-4" />
          Back
        </Button>
        <div className="flex-1 min-w-0">
          <h2 className="text-lg font-semibold truncate">
            {rootSpan?.name || traceId}
          </h2>
          <p className="text-sm text-muted-foreground font-mono truncate">
            {traceId}
          </p>
        </div>
        <Button
          variant="ghost"
          size="icon"
          onClick={() => refetch()}
          disabled={isFetching}
        >
          <RefreshCw className={cn('h-4 w-4', isFetching && 'animate-spin')} />
        </Button>
      </div>

      {/* Summary cards */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <Card>
          <CardContent className="p-4">
            <p className="text-xs text-muted-foreground mb-1">Duration</p>
            <p className="text-lg font-semibold">
              {formatDuration(traceDuration)}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-4">
            <p className="text-xs text-muted-foreground mb-1">Spans</p>
            <p className="text-lg font-semibold">{spans.length}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-4">
            <p className="text-xs text-muted-foreground mb-1">Services</p>
            <p className="text-lg font-semibold">{services.length}</p>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="p-4">
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

      {/* Waterfall + Detail panel */}
      <div className="flex flex-col lg:flex-row gap-4">
        {/* Waterfall chart */}
        <Card className={cn('flex-1 min-w-0', selectedSpan && 'lg:w-1/2')}>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium">
              Span Waterfall
            </CardTitle>
          </CardHeader>
          <CardContent className="p-0 overflow-x-auto">
            <ScrollArea className="h-[600px]">
              {/* Timeline header */}
              <div className="flex items-center border-b px-4 py-2 text-xs text-muted-foreground sticky top-0 bg-background z-10 min-w-[500px]">
                <div className="w-[180px] md:w-[280px] shrink-0">Span Name</div>
                <div className="flex-1 flex justify-between">
                  <span>{formatTimestamp(new Date(traceStart).toISOString())}</span>
                  <span>{formatDuration(traceDuration)}</span>
                  <span>{formatTimestamp(new Date(traceEnd).toISOString())}</span>
                </div>
              </div>

              {/* Span rows */}
              {flatSpans.map((node) => {
                const spanStart =
                  new Date(node.span.start_time).getTime() - traceStart
                const spanDuration =
                  new Date(node.span.end_time).getTime() -
                  new Date(node.span.start_time).getTime()
                // Reserve 8% on the right for the duration label
                const maxBarPct = 92
                const leftPct =
                  traceDuration > 0 ? (spanStart / traceDuration) * maxBarPct : 0
                const widthPct =
                  traceDuration > 0
                    ? Math.max((spanDuration / traceDuration) * maxBarPct, 0.5)
                    : maxBarPct
                const isError = node.span.status_code?.toUpperCase() === 'ERROR'
                const isSelected = selectedSpanId === node.span.span_id

                return (
                  <TooltipProvider key={node.span.span_id}>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <button
                          type="button"
                          onClick={() =>
                            setSelectedSpanId(
                              isSelected ? null : node.span.span_id
                            )
                          }
                          className={cn(
                            'flex items-center w-full border-b px-4 py-1.5 text-left hover:bg-accent/50 transition-colors min-w-[500px]',
                            isSelected && 'bg-accent'
                          )}
                        >
                          {/* Span name with indent */}
                          <div
                            className="w-[180px] md:w-[280px] shrink-0 flex items-center gap-1.5 min-w-0"
                            style={{
                              paddingLeft: `${node.depth * 16}px`,
                            }}
                          >
                            {node.children.length > 0 ? (
                              <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
                            ) : (
                              <span className="w-3 shrink-0" />
                            )}
                            {statusIcon(node.span.status_code)}
                            <span className="text-xs truncate">
                              {node.span.name}
                            </span>
                          </div>

                          {/* Waterfall bar */}
                          <div className="flex-1 relative h-6">
                            <div
                              className={cn(
                                'absolute top-1 h-4 rounded-sm min-w-[2px]',
                                isError
                                  ? 'bg-red-500/80'
                                  : 'bg-primary/70'
                              )}
                              style={{
                                left: `${leftPct}%`,
                                width: `${widthPct}%`,
                              }}
                            />
                            {/* Duration label */}
                            <span
                              className="absolute top-0.5 text-[10px] text-muted-foreground whitespace-nowrap"
                              style={{
                                left: `${leftPct + widthPct + 0.5}%`,
                              }}
                            >
                              {formatDuration(spanDuration)}
                            </span>
                          </div>
                        </button>
                      </TooltipTrigger>
                      <TooltipContent side="top" className="max-w-xs">
                        <div className="space-y-1">
                          <p className="font-medium">{node.span.name}</p>
                          {node.span.resource?.service_name && (
                            <p className="text-xs">
                              Service: {node.span.resource.service_name}
                            </p>
                          )}
                          <p className="text-xs">
                            Duration: {formatDuration(spanDuration)}
                          </p>
                          <p className="text-xs">
                            Kind: {kindLabel(node.span.kind)}
                          </p>
                        </div>
                      </TooltipContent>
                    </Tooltip>
                  </TooltipProvider>
                )
              })}
            </ScrollArea>
          </CardContent>
        </Card>

        {/* Span detail panel */}
        {selectedSpan && (
          <Card className="w-full lg:w-[400px] shrink-0">
            <CardHeader className="pb-2">
              <div className="flex items-center justify-between">
                <CardTitle className="text-sm font-medium truncate">
                  {selectedSpan.name}
                </CardTitle>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setSelectedSpanId(null)}
                  className="h-6 w-6 p-0"
                >
                  ×
                </Button>
              </div>
            </CardHeader>
            <CardContent className="p-0">
              <ScrollArea className="h-[400px] lg:h-[560px]">
                <div className="p-4 space-y-4 overflow-x-auto">
                  {/* Basic info */}
                  <div className="space-y-2">
                    <div className="flex items-center gap-2">
                      {statusIcon(selectedSpan.status_code)}
                      {selectedSpan.status_code?.toUpperCase() !== 'UNSET' &&
                        selectedSpan.status_code && (
                          <Badge variant={statusBadgeVariant(selectedSpan.status_code)}>
                            {selectedSpan.status_code}
                          </Badge>
                        )}
                      <Badge variant="outline">
                        {kindLabel(selectedSpan.kind)}
                      </Badge>
                    </div>
                    {selectedSpan.status_message && (
                      <p className="text-xs text-destructive bg-destructive/10 rounded p-2">
                        {selectedSpan.status_message}
                      </p>
                    )}
                  </div>

                  {/* Timing */}
                  <div>
                    <h4 className="text-xs font-medium text-muted-foreground mb-2">
                      Timing
                    </h4>
                    <div className="grid grid-cols-2 gap-2 text-xs">
                      <div>
                        <span className="text-muted-foreground">Start:</span>
                        <span className="ml-1 font-mono">
                          {formatTimestamp(selectedSpan.start_time)}
                        </span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">End:</span>
                        <span className="ml-1 font-mono">
                          {formatTimestamp(selectedSpan.end_time)}
                        </span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">Duration:</span>
                        <span className="ml-1 font-mono">
                          {formatDuration(selectedSpan.duration_ms)}
                        </span>
                      </div>
                    </div>
                  </div>

                  {/* IDs */}
                  <div>
                    <h4 className="text-xs font-medium text-muted-foreground mb-2">
                      IDs
                    </h4>
                    <div className="space-y-1 text-xs font-mono">
                      <div className="flex gap-2">
                        <span className="text-muted-foreground shrink-0">
                          span:
                        </span>
                        <span className="truncate">{selectedSpan.span_id}</span>
                      </div>
                      <div className="flex gap-2">
                        <span className="text-muted-foreground shrink-0">
                          trace:
                        </span>
                        <span className="truncate">{selectedSpan.trace_id}</span>
                      </div>
                      {selectedSpan.parent_span_id && (
                        <div className="flex gap-2">
                          <span className="text-muted-foreground shrink-0">
                            parent:
                          </span>
                          <span className="truncate">
                            {selectedSpan.parent_span_id}
                          </span>
                        </div>
                      )}
                    </div>
                  </div>

                  {/* Resource */}
                  {selectedSpan.resource && (
                    <div>
                      <h4 className="text-xs font-medium text-muted-foreground mb-2">
                        Resource
                      </h4>
                      <div className="space-y-1 text-xs">
                        {selectedSpan.resource.service_name && (
                          <div className="flex gap-2">
                            <span className="text-muted-foreground">
                              service.name:
                            </span>
                            <span>{selectedSpan.resource.service_name}</span>
                          </div>
                        )}
                        {selectedSpan.resource.service_version && (
                          <div className="flex gap-2">
                            <span className="text-muted-foreground">
                              service.version:
                            </span>
                            <span>{selectedSpan.resource.service_version}</span>
                          </div>
                        )}
                      </div>
                    </div>
                  )}

                  {/* Attributes */}
                  {selectedSpan.attributes &&
                    Object.keys(selectedSpan.attributes).length > 0 && (
                      <Collapsible defaultOpen>
                        <CollapsibleTrigger className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
                          <ChevronRight className="h-3 w-3 transition-transform data-[state=open]:rotate-90" />
                          Attributes ({Object.keys(selectedSpan.attributes).length})
                        </CollapsibleTrigger>
                        <CollapsibleContent>
                          <div className="mt-2 space-y-1">
                            {Object.entries(selectedSpan.attributes).map(
                              ([key, value]) => (
                                <div
                                  key={key}
                                  className="flex gap-2 text-xs font-mono py-0.5 border-b border-border/50"
                                >
                                  <span className="text-muted-foreground shrink-0">
                                    {key}:
                                  </span>
                                  <span className="truncate">
                                    {typeof value === 'object'
                                      ? JSON.stringify(value)
                                      : String(value)}
                                  </span>
                                </div>
                              )
                            )}
                          </div>
                        </CollapsibleContent>
                      </Collapsible>
                    )}

                  {/* Events */}
                  {selectedSpan.events && selectedSpan.events.length > 0 && (
                    <Collapsible defaultOpen>
                      <CollapsibleTrigger className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
                        <ChevronRight className="h-3 w-3 transition-transform data-[state=open]:rotate-90" />
                        Events ({selectedSpan.events.length})
                      </CollapsibleTrigger>
                      <CollapsibleContent>
                        <div className="mt-2 space-y-2">
                          {selectedSpan.events.map((event) => (
                            <div
                              key={`evt-${event.name}-${event.timestamp}`}
                              className="rounded border p-2 text-xs space-y-1"
                            >
                              <div className="flex items-center gap-2">
                                <Clock className="h-3 w-3 text-muted-foreground" />
                                <span className="font-medium">
                                  {event.name}
                                </span>
                                <span className="text-muted-foreground font-mono">
                                  {formatTimestamp(event.timestamp)}
                                </span>
                              </div>
                              {event.attributes &&
                                Object.keys(event.attributes).length > 0 && (
                                  <div className="ml-5 space-y-0.5">
                                    {Object.entries(event.attributes).map(
                                      ([k, v]) => (
                                        <div
                                          key={k}
                                          className="flex gap-2 font-mono"
                                        >
                                          <span className="text-muted-foreground">
                                            {k}:
                                          </span>
                                          <span className="truncate">
                                            {typeof v === 'object'
                                              ? JSON.stringify(v)
                                              : String(v)}
                                          </span>
                                        </div>
                                      )
                                    )}
                                  </div>
                                )}
                            </div>
                          ))}
                        </div>
                      </CollapsibleContent>
                    </Collapsible>
                  )}
                </div>
              </ScrollArea>
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  )
}
