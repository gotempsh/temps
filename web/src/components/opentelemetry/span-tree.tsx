import { ChevronDown, AlertCircle } from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'
import { Span } from './types'

interface SpanTreeProps {
  spans: Span[]
  startTimeUnixNano: number
  endTimeUnixNano: number
}

function formatNanoToMs(nanoTime: number): string {
  return (nanoTime / 1e6).toFixed(2)
}

function getServiceColor(serviceName: string): string {
  switch (serviceName.toLowerCase()) {
    case 'frontend':
      return 'bg-amber-600'
    case 'customer':
      return 'bg-teal-500'
    case 'mysql':
      return 'bg-orange-300'
    case 'redis':
      return 'bg-red-400'
    case 'route':
      return 'bg-blue-400'
    default:
      return 'bg-gray-400'
  }
}

function SpanRow({
  span,
  startTimeUnixNano,
  endTimeUnixNano,
}: {
  span: Span
  startTimeUnixNano: number
  endTimeUnixNano: number
}) {
  const [isExpanded, setIsExpanded] = useState(true)
  const hasChildren = Boolean(span?.children?.length)

  // First normalize all timestamps relative to trace start (in nanoseconds)
  const spanStartNano = span.startTimeUnixNano - startTimeUnixNano
  const spanEndNano = span.endTimeUnixNano - startTimeUnixNano
  const traceDurationNano = endTimeUnixNano - startTimeUnixNano

  // Then convert to milliseconds
  const spanStartMs = spanStartNano / 1_000_000
  const spanEndMs = spanEndNano / 1_000_000
  const traceDurationMs = traceDurationNano / 1_000_000
  const spanDurationMs =
    (span.endTimeUnixNano - span.startTimeUnixNano) / 1_000_000

  // Calculate percentages
  const leftPercent = (spanStartMs / traceDurationMs) * 100
  const widthPercent = (spanDurationMs / traceDurationMs) * 100
  const rightPercent = leftPercent + widthPercent

  // Ensure values are within bounds
  const boundedLeft = Math.max(0, leftPercent)
  const boundedRight = Math.min(100, rightPercent)
  const boundedWidth = Math.max(0.5, boundedRight - boundedLeft)

  console.log(`Span: ${span.operation}`)
  console.log(`Start: ${spanStartMs.toFixed(2)}ms`)
  console.log(`Duration: ${spanDurationMs.toFixed(2)}ms`)
  console.log(`Left: ${boundedLeft.toFixed(2)}%`)
  console.log(`Width: ${boundedWidth.toFixed(2)}%`)
  console.log(`Right: ${boundedRight.toFixed(2)}%`)

  return (
    <>
      <div className="flex items-center min-h-[32px] border-t border-border/30 group hover:bg-muted/30">
        <div className="w-[500px] flex items-center gap-1 pl-2">
          <div className="w-6">
            {hasChildren && (
              <button
                onClick={() => setIsExpanded(!isExpanded)}
                className="p-1 hover:bg-accent rounded-sm"
              >
                <ChevronDown
                  className={cn(
                    'h-3 w-3 transition-transform',
                    !isExpanded && '-rotate-90'
                  )}
                />
              </button>
            )}
          </div>
          <div
            className={cn('w-1 h-4 mr-1', getServiceColor(span.serviceName))}
          />
          {span.error && (
            <AlertCircle className="h-4 w-4 text-destructive mr-1" />
          )}
          <div className="min-w-0 flex items-center gap-2">
            <div>
              <div className="font-medium text-sm truncate">
                {span.serviceName}
              </div>
              <div className="text-xs text-muted-foreground truncate">
                {span.operation}
              </div>
            </div>
            <div className="text-xs text-muted-foreground whitespace-nowrap">
              {spanDurationMs.toFixed(2)}ms
            </div>
          </div>
        </div>
        <div className="flex-1 relative px-4">
          <div className="relative h-2 w-full">
            <div
              className={cn(
                'absolute h-full rounded-sm opacity-80',
                getServiceColor(span.serviceName)
              )}
              style={{
                left: `${boundedLeft}%`,
                width: `${boundedWidth}%`,
              }}
            />
          </div>
        </div>
      </div>
      {isExpanded && hasChildren && (
        <div className="ml-6">
          {span.children?.map((child) => (
            <SpanRow
              key={child.id}
              span={child}
              startTimeUnixNano={startTimeUnixNano}
              endTimeUnixNano={endTimeUnixNano}
            />
          ))}
        </div>
      )}
    </>
  )
}

export function SpanTree({
  spans,
  startTimeUnixNano,
  endTimeUnixNano,
}: SpanTreeProps) {
  const traceDuration = endTimeUnixNano - startTimeUnixNano
  const timeMarkers = [0, 0.25, 0.5, 0.75, 1].map((percent) => {
    const nanoOffset = traceDuration * percent
    return formatNanoToMs(nanoOffset)
  })

  return (
    <div className="bg-background">
      <div className="flex items-center px-4 py-2 border-y text-sm">
        <div className="w-[500px] font-medium">Service & Operation</div>
        <div className="flex-1 px-4">
          <div className="flex justify-between text-muted-foreground">
            {timeMarkers.map((time, index) => (
              <span key={index}>{time}ms</span>
            ))}
          </div>
        </div>
      </div>
      <div className="relative">
        <div className="absolute inset-0 flex-1 ml-[500px]">
          <div className="w-full h-full grid grid-cols-4">
            <div className="border-l border-r border-border/30" />
            <div className="border-r border-border/30" />
            <div className="border-r border-border/30" />
            <div className="border-r border-border/30" />
          </div>
        </div>
        <div className="relative">
          {spans.map((span) => (
            <SpanRow
              key={span.id}
              span={span}
              startTimeUnixNano={startTimeUnixNano}
              endTimeUnixNano={endTimeUnixNano}
            />
          ))}
        </div>
      </div>
    </div>
  )
}
