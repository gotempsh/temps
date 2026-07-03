import {
  AlertTriangle,
  Box,
  Boxes,
  CheckCircle2,
  Clock,
  EyeOff,
  Layers,
  XCircle,
} from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { formatDuration } from '@/components/traces/SpanWaterfall'
import { cn } from '@/lib/utils'

/**
 * Compact trace-header stat row. Replaces the heavy 4-card metric grid with a
 * single wrap-able row of pills — the right surface treatment for sibling
 * metrics in a shared context (see design guidelines: Surfaces + Badges).
 *
 * Drives both the single-project header (service count) and the cross-project
 * unified header (project count + optional errors / redaction / truncation).
 */
export interface TraceStatBadgesProps {
  durationMs: number
  spanCount: number
  /** Cross-project / unified header: number of contributing projects. */
  projectCount?: number
  /** Single-project header: number of distinct services. */
  serviceCount?: number
  /** Trace-level status, e.g. "OK" | "ERROR" | "UNSET". */
  status?: string | null
  /** Error-span count (unified header). */
  errorCount?: number
  /** Some contributing projects opted out of sharing (spans hidden). */
  redacted?: boolean
  /** Result hit the project/span cap. */
  truncated?: boolean
  className?: string
}

// Leading-icon pill: icon side padding == vertical padding, larger on the text
// side (Badges guideline). Metric weight is medium so the value stays legible.
const PILL = 'gap-1.5 py-1 pl-2 pr-2.5 font-medium'
const ICON = 'size-3.5 shrink-0'

export function TraceStatBadges({
  durationMs,
  spanCount,
  projectCount,
  serviceCount,
  status,
  errorCount = 0,
  redacted = false,
  truncated = false,
  className,
}: TraceStatBadgesProps) {
  const normalized = (status ?? '').toUpperCase()
  const hasStatus = normalized !== '' && normalized !== 'UNSET'
  const isError = normalized === 'ERROR' || errorCount > 0

  return (
    <div className={cn('flex flex-wrap items-center gap-2', className)}>
      <Badge variant="secondary" className={PILL}>
        <Clock className={ICON} />
        {formatDuration(durationMs)}
      </Badge>

      <Badge variant="secondary" className={PILL}>
        <Layers className={ICON} />
        {spanCount} {spanCount === 1 ? 'span' : 'spans'}
      </Badge>

      {projectCount != null && (
        <Badge variant="secondary" className={PILL}>
          <Boxes className={ICON} />
          {projectCount} {projectCount === 1 ? 'project' : 'projects'}
        </Badge>
      )}

      {projectCount == null && serviceCount != null && (
        <Badge variant="secondary" className={PILL}>
          <Box className={ICON} />
          {serviceCount} {serviceCount === 1 ? 'service' : 'services'}
        </Badge>
      )}

      {errorCount > 0 && (
        <Badge variant="destructive" className={PILL}>
          <AlertTriangle className={ICON} />
          {errorCount} {errorCount === 1 ? 'error' : 'errors'}
        </Badge>
      )}

      {hasStatus && (
        <Badge variant={isError ? 'destructive' : 'success'} className={PILL}>
          {isError ? (
            <XCircle className={ICON} />
          ) : (
            <CheckCircle2 className={ICON} />
          )}
          {isError ? 'Error' : 'OK'}
        </Badge>
      )}

      {redacted && (
        <Badge
          variant="outline"
          className={cn(PILL, 'text-muted-foreground')}
          title="Some contributing projects have cross-project sharing turned off; their spans are hidden."
        >
          <EyeOff className={ICON} />
          Some projects hidden
        </Badge>
      )}

      {truncated && (
        <Badge
          variant="outline"
          className={cn(PILL, 'text-muted-foreground')}
          title="This trace exceeded the project/span cap; some spans were dropped."
        >
          <AlertTriangle className={ICON} />
          Truncated
        </Badge>
      )}
    </div>
  )
}
