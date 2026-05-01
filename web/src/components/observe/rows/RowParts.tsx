import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { format, formatDistanceToNowStrict } from 'date-fns'

/** One-line tabular row used by every event renderer. */
export function ObserveRowShell({
  ts,
  icon,
  primary,
  secondary,
  meta,
  onClick,
}: {
  ts: string
  icon: React.ReactNode
  primary: React.ReactNode
  secondary?: React.ReactNode
  meta?: React.ReactNode
  onClick?: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'group flex w-full items-center gap-3 px-4 py-2 text-left',
        'border-b border-border/50 last:border-b-0',
        'hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none',
        'transition-colors',
      )}
    >
      <Timestamp ts={ts} />
      <div className="flex h-5 w-5 shrink-0 items-center justify-center text-muted-foreground">
        {icon}
      </div>
      <div className="min-w-0 flex-1 truncate text-sm">
        <span className="font-medium">{primary}</span>
        {secondary != null && (
          <span className="ml-2 truncate text-muted-foreground">
            {secondary}
          </span>
        )}
      </div>
      {meta != null && (
        <div className="ml-auto flex shrink-0 items-center gap-2 text-xs text-muted-foreground">
          {meta}
        </div>
      )}
    </button>
  )
}

function Timestamp({ ts }: { ts: string }) {
  const d = new Date(ts)
  return (
    <time
      dateTime={ts}
      title={format(d, 'yyyy-MM-dd HH:mm:ss.SSS')}
      className="w-20 shrink-0 font-mono text-xs tabular-nums text-muted-foreground"
    >
      {formatDistanceToNowStrict(d, { addSuffix: false })}
    </time>
  )
}

export function StatusBadge({ status }: { status: number }) {
  const variant: 'default' | 'destructive' | 'secondary' | 'outline' =
    status >= 500
      ? 'destructive'
      : status >= 400
        ? 'secondary'
        : status >= 300
          ? 'outline'
          : 'default'
  return (
    <Badge variant={variant} className="font-mono tabular-nums">
      {status}
    </Badge>
  )
}

export function SeverityBadge({ severity }: { severity: string }) {
  const lower = severity.toLowerCase()
  const variant: 'default' | 'destructive' | 'secondary' | 'outline' =
    lower === 'error' || lower === 'fatal'
      ? 'destructive'
      : lower === 'warn' || lower === 'warning'
        ? 'secondary'
        : 'outline'
  return (
    <Badge variant={variant} className="uppercase tracking-wide">
      {severity}
    </Badge>
  )
}
