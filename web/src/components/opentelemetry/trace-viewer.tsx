import { ChevronDown } from 'lucide-react'
import { SpanTree } from './span-tree'
import { Span } from './types'
import { SpanData, TraceDetailsResponse } from '@/api/client'
import { cn } from '@/lib/utils'
import { format } from 'date-fns'

interface TraceViewerProps {
  trace: TraceDetailsResponse
}

function formatUnixNanoToDate(unixNano: number): string {
  const date = new Date(unixNano / 1e6)
  return date.toLocaleString('en-US', {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

export function TraceViewer({ trace }: TraceViewerProps) {
  const durationMs = (trace.trace.end_time - trace.trace.start_time) / 1e6

  // Convert flat spans array to hierarchical structure
  const buildSpanTree = (spans: SpanData[]): Span[] => {
    const spanMap = new Map<string, Span>()
    const roots: Span[] = []

    // First pass - create span objects and store in map
    spans.forEach((span) => {
      spanMap.set(span.span_id, {
        id: span.span_id,
        serviceName: span.name || 'unknown',
        name: span.name,
        operation: span.name,
        startTimeUnixNano: span.start_time,
        endTimeUnixNano: span.end_time,
        error: false,
        children: [],
      })
    })

    // Second pass - build tree structure
    spans.forEach((span) => {
      const spanNode = spanMap.get(span.span_id)
      if (!spanNode) return

      if (span.parent_span_id && spanMap.has(span.parent_span_id)) {
        // Add as child to parent
        const parent = spanMap.get(span.parent_span_id)
        parent?.children?.push(spanNode)
      } else {
        // No parent - this is a root span
        roots.push(spanNode)
      }
    })

    return roots
  }

  const spanTree = buildSpanTree(trace.spans)

  return (
    <div className="w-full space-y-6">
      <div className="border rounded-lg overflow-hidden">
        <div className="flex items-center gap-2 p-4 bg-background">
          <ChevronDown className="h-5 w-5" />
          <h2 className="text-xl font-semibold flex-1">
            {trace.trace.trace_id}
            <span className="text-muted-foreground ml-2">
              {trace.trace.trace_id}
            </span>
          </h2>
        </div>

        <div className="bg-muted/30 px-4 py-2 border-y">
          <div className="flex gap-8 text-sm">
            <div>
              <span className="text-muted-foreground">Trace Start</span>{' '}
              {formatUnixNanoToDate(trace.trace.start_time_unix_nano)}
            </div>
            <div>
              <span className="text-muted-foreground">Duration</span>{' '}
              {trace.trace.duration_ms}ms
            </div>
          </div>
        </div>

        <SpanTree
          spans={spanTree}
          startTimeUnixNano={trace.trace.start_time_unix_nano}
          endTimeUnixNano={trace.trace.end_time_unix_nano}
        />
      </div>

      {/* Logs Section */}
      <div className="border rounded-lg overflow-hidden">
        <div className="flex items-center gap-2 p-4 bg-background">
          <h2 className="text-xl font-semibold">Logs</h2>
        </div>
        <div className="divide-y">
          {trace.logs.map((log, index) => (
            <div
              key={index}
              className={cn('flex flex-col gap-2 p-4 hover:bg-muted/50')}
            >
              <div className="flex items-center gap-2">
                <span className="text-sm text-muted-foreground">
                  {format(
                    new Date(Math.floor(log.timestamp / 1_000_000)),
                    'HH:mm:ss'
                  )}
                </span>
                <span
                  className={cn(
                    'text-xs px-2 py-0.5 rounded-full',
                    log.severity_text === 'error' && 'bg-red-100 text-red-700',
                    log.severity_text === 'info' && 'bg-blue-100 text-blue-700'
                  )}
                >
                  {log.severity_text}
                </span>
                <span className="text-sm font-medium">
                  {(log.attributes as { service_name?: string })
                    ?.service_name ?? 'unknown'}
                </span>
              </div>
              <p className="text-sm">{log.body}</p>
              {(log.attributes as any) &&
                Object.keys(log.attributes as any).length > 0 && (
                  <pre className="text-xs bg-muted p-2 rounded-md overflow-x-auto">
                    {(() => {
                      try {
                        return JSON.stringify(log.attributes, null, 2)
                      } catch {
                        return '[Unable to display attributes]'
                      }
                    })()}
                  </pre>
                )}
            </div>
          ))}
          {trace.logs.length === 0 && (
            <div className="p-8 text-center text-muted-foreground">
              No logs found for this trace
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
