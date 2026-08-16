'use client'

// Shared display components for the email management UI.
// Extracted from EmailsSentList / EmailDetail (StatusBadge) and
// EmailEventTimeline / EmailAnalytics (EventIcon / EventBadge) to avoid
// copy-pasted logic drifting out of sync.

import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import {
  AlertCircle,
  AlertTriangle,
  Archive,
  CheckCircle2,
  Clock,
  Eye,
  Mail,
  MailWarning,
  MousePointerClick,
  Send,
} from 'lucide-react'

export function StatusBadge({ status }: { status: string }) {
  switch (status) {
    case 'sent':
      return (
        <Badge variant="default" className="gap-1">
          <CheckCircle2 className="h-3 w-3" />
          Sent
        </Badge>
      )
    case 'queued':
      return (
        <Badge variant="secondary" className="gap-1">
          <Clock className="h-3 w-3" />
          Queued
        </Badge>
      )
    case 'failed':
      return (
        <Badge variant="destructive" className="gap-1">
          <AlertCircle className="h-3 w-3" />
          Failed
        </Badge>
      )
    case 'captured':
      return (
        <Badge variant="outline" className="gap-1 border-blue-500 text-blue-600">
          <Archive className="h-3 w-3" />
          Captured
        </Badge>
      )
    default:
      return <Badge variant="outline">{status}</Badge>
  }
}

// Canonical event-type -> icon mapping. This is the more complete of the two
// copies that previously existed (EmailEventTimeline's), which correctly
// recognizes Gmail/Yahoo proxy user-agents further down in parseUserAgent;
// kept here alongside EventBadge since they're always used together.
export function EventIcon({
  type,
  className = 'h-4 w-4',
}: {
  type: string
  className?: string
}) {
  switch (type) {
    case 'open':
    case 'opened':
      return <Eye className={cn(className, 'text-blue-500')} />
    case 'click':
    case 'clicked':
      return <MousePointerClick className={cn(className, 'text-green-500')} />
    case 'delivered':
      return <Send className={cn(className, 'text-emerald-500')} />
    case 'bounced':
      return <MailWarning className={cn(className, 'text-red-500')} />
    case 'complained':
      return <AlertTriangle className={cn(className, 'text-orange-500')} />
    default:
      return <Mail className={cn(className, 'text-muted-foreground')} />
  }
}

const EVENT_BADGE_CONFIG: Record<
  string,
  { variant: 'default' | 'secondary' | 'destructive' | 'outline'; label: string }
> = {
  open: { variant: 'secondary', label: 'Opened' },
  opened: { variant: 'secondary', label: 'Opened' },
  click: { variant: 'default', label: 'Clicked' },
  clicked: { variant: 'default', label: 'Clicked' },
  delivered: { variant: 'outline', label: 'Delivered' },
  bounced: { variant: 'destructive', label: 'Bounced' },
  complained: { variant: 'destructive', label: 'Complained' },
}

export function EventBadge({
  type,
  iconClassName,
}: {
  type: string
  iconClassName?: string
}) {
  const config = EVENT_BADGE_CONFIG[type] || { variant: 'outline' as const, label: type }

  return (
    <Badge variant={config.variant} className="gap-1 text-xs">
      <EventIcon type={type} className={iconClassName} />
      {config.label}
    </Badge>
  )
}
