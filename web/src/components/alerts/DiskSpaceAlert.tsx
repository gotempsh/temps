import { Button } from '@/components/ui/button'
import { useDiskStatus } from '@/hooks/useDiskStatus'
import { HardDrive, X } from 'lucide-react'
import { Link } from 'react-router-dom'
import { useState } from 'react'
import { cn } from '@/lib/utils'

interface DiskSpaceAlertProps {
  dismissible?: boolean
}

/**
 * Dashboard banner that warns when the control-plane server is running low on
 * disk space. Renders only when disk monitoring is enabled and at least one
 * monitored disk meets or exceeds the configured threshold. Critical (>= 90%)
 * usage uses a red treatment; otherwise orange.
 */
export function DiskSpaceAlert({ dismissible = true }: DiskSpaceAlertProps) {
  const { data } = useDiskStatus()
  const [isDismissed, setIsDismissed] = useState(false)

  const alerts = data?.alerts ?? []
  // The worst (highest-usage) disk drives the banner copy and severity.
  const worst = alerts.reduce<(typeof alerts)[number] | undefined>(
    (max, a) => (max && max.usage_percent >= a.usage_percent ? max : a),
    undefined,
  )

  if (!data?.enabled || !worst || isDismissed) {
    return null
  }

  const isCritical = worst.usage_percent >= 90

  const accent = isCritical
    ? {
        border: 'border-red-200 dark:border-red-900/50',
        bg: 'bg-red-50/60 dark:bg-red-950/20',
        icon: 'text-red-600 dark:text-red-400',
        text: 'text-red-800 dark:text-red-200',
        link: 'text-red-700 hover:text-red-900 dark:text-red-300 dark:hover:text-red-100',
        dismiss:
          'text-red-600/70 hover:text-red-800 dark:text-red-400/70 dark:hover:text-red-200',
      }
    : {
        border: 'border-orange-200 dark:border-orange-900/50',
        bg: 'bg-orange-50/60 dark:bg-orange-950/20',
        icon: 'text-orange-600 dark:text-orange-400',
        text: 'text-orange-800 dark:text-orange-200',
        link: 'text-orange-700 hover:text-orange-900 dark:text-orange-300 dark:hover:text-orange-100',
        dismiss:
          'text-orange-600/70 hover:text-orange-800 dark:text-orange-400/70 dark:hover:text-orange-200',
      }

  const headline = isCritical ? 'Disk space critically low' : 'Disk space running low'

  // Thin, single-line banner. A passive heads-up shouldn't claim a 3-line
  // padded card — it just states the worst disk's usage and links to the
  // settings page for the full detail / threshold adjustment.
  return (
    <div
      className={cn(
        // Full-width top strip above the sidebar + header — no rounding or
        // margins, just a bottom border so it reads as a banner, not a card.
        'flex items-center gap-2 border-b px-4 py-1.5 text-sm',
        accent.border,
        accent.bg,
        accent.text
      )}
    >
      <HardDrive className={cn('h-4 w-4 shrink-0', accent.icon)} />
      <p className="min-w-0 flex-1 truncate">
        <span className="font-medium">{headline}</span>
        {/* Mobile: keep it actionable with the % used, since the full
            mount/free detail below is hidden on small screens. */}
        <span className="sm:hidden">
          {' '}— <strong>{worst.usage_percent.toFixed(0)}% full</strong>
        </span>
        <span className="hidden sm:inline">
          {' '}— <span className="font-mono">{worst.mount_point}</span> is{' '}
          <strong>{worst.usage_percent.toFixed(1)}% full</strong>,{' '}
          {worst.available_human} free
          {alerts.length > 1 && ` (${alerts.length} disks over threshold)`}
        </span>
      </p>
      <Link
        to="/settings/disk-monitoring"
        className={cn('shrink-0 font-medium underline-offset-2 hover:underline', accent.link)}
      >
        Disk settings
      </Link>
      {dismissible && (
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setIsDismissed(true)}
          aria-label="Dismiss disk space warning"
          className={cn('-mr-1 h-6 w-6 shrink-0 p-0', accent.dismiss)}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      )}
    </div>
  )
}
