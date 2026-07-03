import type { ReactNode } from 'react'
import { ChevronDown, CheckCircle2, XCircle } from 'lucide-react'
import { cn } from '@/lib/utils'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import type { SpanTreeNode } from '../../utils/spanTree'

// ── Shared span display helpers ─────────────────────────────────────
// Extracted alongside the waterfall so both the single-project
// (`TraceDetail`) and cross-project (`CrossProjectTraceDetail`) views
// format spans identically.

export function statusIcon(status?: string) {
  switch (status?.toUpperCase()) {
    case 'ERROR':
      return <XCircle className="h-3.5 w-3.5 text-red-500" />
    case 'OK':
      return <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />
    default:
      return null
  }
}

export function kindLabel(kind?: string): string {
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

export function formatDuration(ms: number): string {
  if (ms < 1) return `${(ms * 1000).toFixed(0)}µs`
  if (ms < 1000) return `${ms.toFixed(1)}ms`
  return `${(ms / 1000).toFixed(2)}s`
}

export function formatTimestamp(ts: string): string {
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
export function serviceColor(name?: string): string {
  if (!name) return '#94a3b8'
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) | 0
  return SERVICE_COLORS[Math.abs(h) % SERVICE_COLORS.length]
}

/** Shared span waterfall (tree + timeline bars). `colorBy="service"` colours each
 *  bar by its service and shows a service dot, like OpenObserve/Datadog.
 *
 *  `renderRowBadge` is an optional, backward-compatible hook that lets a caller
 *  (e.g. the cross-project unified view) render an extra badge in each span's
 *  name column — used to tag which project a span belongs to. Single-project
 *  callers omit it and render exactly as before.
 *
 *  `rowClassName` is an optional per-span class hook: a caller can emphasise or
 *  de-emphasise individual rows (e.g. accent the current project's spans and dim
 *  the rest inside a merged cross-project waterfall). Applied via `cn` so it can
 *  layer on/override the row's base classes. Omitting it renders as before. */
export function SpanWaterfall({
  flatSpans,
  traceStart,
  traceEnd,
  traceDuration,
  selectedSpanId,
  onSelect,
  colorBy = 'status',
  renderRowBadge,
  rowClassName,
  className,
}: {
  flatSpans: SpanTreeNode[]
  traceStart: number
  traceEnd: number
  traceDuration: number
  selectedSpanId: string | null
  onSelect: (spanId: string | null) => void
  colorBy?: 'status' | 'service'
  renderRowBadge?: (span: SpanTreeNode['span']) => ReactNode
  rowClassName?: (span: SpanTreeNode['span']) => string | undefined
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
                    rowClassName?.(node.span),
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
                    {renderRowBadge?.(node.span)}
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
