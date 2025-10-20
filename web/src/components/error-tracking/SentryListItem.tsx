import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { SentryEvent } from '@/types/sentry'
import { format } from 'date-fns'
import { Clock, Code, Globe, Layers, Monitor } from 'lucide-react'

interface SentryListItemProps {
  event: SentryEvent
  onClick?: () => void
  className?: string
}

export function SentryListItem({
  event,
  onClick,
  className,
}: SentryListItemProps) {
  const sentryData = event.sentry
  const mainException = sentryData.exception?.values?.[0]
  const contexts = sentryData.contexts || {}

  const getSeverityColor = (level: string) => {
    switch (level?.toLowerCase()) {
      case 'error':
      case 'fatal':
        return 'destructive'
      case 'warning':
        return 'secondary'
      case 'info':
        return 'default'
      default:
        return 'outline'
    }
  }

  return (
    <div
      className={cn(
        'border rounded-lg p-4 hover:bg-accent/50 transition-colors',
        onClick && 'cursor-pointer',
        className
      )}
      onClick={onClick}
    >
      <div className="space-y-3">
        {/* Header Row: Time, Level, Exception */}
        <div className="flex items-start justify-between gap-3">
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <Clock className="h-3.5 w-3.5 text-muted-foreground flex-shrink-0" />
              <span className="text-sm font-medium">
                {format(new Date(sentryData.timestamp * 1000), 'PPpp')}
              </span>
              {sentryData.level && (
                <Badge variant={getSeverityColor(sentryData.level)}>
                  {sentryData.level}
                </Badge>
              )}
            </div>
            <p className="text-sm text-muted-foreground font-mono truncate">
              {mainException?.value ||
                sentryData.logentry?.formatted ||
                'Event'}
            </p>
          </div>
          <div className="text-xs text-muted-foreground font-mono">
            {sentryData.event_id.slice(0, 8)}
          </div>
        </div>

        {/* Exception Type */}
        {mainException?.type && (
          <div className="flex items-center gap-2">
            <Code className="h-3.5 w-3.5 text-muted-foreground" />
            <span className="text-xs font-mono text-muted-foreground">
              {mainException.type}
            </span>
            {mainException.module && (
              <span className="text-xs text-muted-foreground">
                in {mainException.module}
              </span>
            )}
          </div>
        )}

        {/* Context Information Grid */}
        <div className="grid grid-cols-2 gap-2 text-xs">
          {/* Environment */}
          {sentryData.environment && (
            <div className="flex items-center gap-1.5">
              <Globe className="h-3 w-3 text-muted-foreground" />
              <span className="text-muted-foreground">Env:</span>
              <span className="font-medium">{sentryData.environment}</span>
            </div>
          )}

          {/* Platform/Runtime */}
          {contexts.runtime && (
            <div className="flex items-center gap-1.5">
              <Layers className="h-3 w-3 text-muted-foreground" />
              <span className="text-muted-foreground">Runtime:</span>
              <span className="font-medium">
                {contexts.runtime.name} {contexts.runtime.version}
              </span>
            </div>
          )}

          {/* OS */}
          {contexts.os && (
            <div className="flex items-center gap-1.5">
              <Monitor className="h-3 w-3 text-muted-foreground" />
              <span className="text-muted-foreground">OS:</span>
              <span className="font-medium">
                {contexts.os.name} {contexts.os.version}
              </span>
            </div>
          )}

          {/* Transaction */}
          {sentryData.transaction && (
            <div className="flex items-center gap-1.5 col-span-2">
              <Layers className="h-3 w-3 text-muted-foreground" />
              <span className="text-muted-foreground">Transaction:</span>
              <span className="font-medium font-mono text-xs truncate">
                {sentryData.transaction}
              </span>
            </div>
          )}
        </div>

        {/* Tags */}
        {sentryData.tags && sentryData.tags.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {sentryData.tags.slice(0, 5).map(([key, value], index) => (
              <Badge key={index} variant="outline" className="text-xs">
                {key}: {value}
              </Badge>
            ))}
            {sentryData.tags.length > 5 && (
              <Badge variant="outline" className="text-xs">
                +{sentryData.tags.length - 5} more
              </Badge>
            )}
          </div>
        )}

        {/* Request Info */}
        {sentryData.request && (
          <div className="flex items-center gap-2 text-xs">
            <Badge variant="outline" className="font-mono">
              {sentryData.request.method}
            </Badge>
            <span className="font-mono text-muted-foreground truncate">
              {sentryData.request.url}
            </span>
          </div>
        )}

        {/* Breadcrumbs Count */}
        {sentryData.breadcrumbs?.values &&
          sentryData.breadcrumbs.values.length > 0 && (
            <div className="text-xs text-muted-foreground">
              {sentryData.breadcrumbs.values.length} breadcrumb
              {sentryData.breadcrumbs.values.length !== 1 ? 's' : ''}
            </div>
          )}
      </div>
    </div>
  )
}
