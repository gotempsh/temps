import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useDiskStatus } from '@/hooks/useDiskStatus'
import { HardDrive, Settings, X } from 'lucide-react'
import { Link } from 'react-router-dom'
import { useState } from 'react'

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
        border: 'border-red-200',
        bg: 'bg-red-50/50 dark:bg-red-950/20',
        icon: 'text-red-600',
        title: 'text-red-900 dark:text-red-100',
        body: 'text-red-800 dark:text-red-200',
        button:
          'border-red-300 text-red-700 hover:bg-red-100 dark:border-red-700 dark:text-red-300 dark:hover:bg-red-900/20',
        dismiss:
          'text-red-600 hover:bg-red-100 dark:text-red-400 dark:hover:bg-red-900/20',
      }
    : {
        border: 'border-orange-200',
        bg: 'bg-orange-50/50 dark:bg-orange-950/20',
        icon: 'text-orange-600',
        title: 'text-orange-900 dark:text-orange-100',
        body: 'text-orange-800 dark:text-orange-200',
        button:
          'border-orange-300 text-orange-700 hover:bg-orange-100 dark:border-orange-700 dark:text-orange-300 dark:hover:bg-orange-900/20',
        dismiss:
          'text-orange-600 hover:bg-orange-100 dark:text-orange-400 dark:hover:bg-orange-900/20',
      }

  const headline = isCritical
    ? 'Disk Space Critically Low'
    : 'Disk Space Running Low'

  return (
    <Alert className={`${accent.border} ${accent.bg} mb-6`}>
      <div className="flex items-start justify-between w-full">
        <div className="flex items-start gap-3 flex-1">
          <HardDrive className={`h-5 w-5 ${accent.icon} mt-0.5`} />
          <div className="space-y-1 flex-1">
            <AlertTitle className={accent.title}>{headline}</AlertTitle>
            <AlertDescription className={accent.body}>
              <span className="font-mono">{worst.mount_point}</span> is{' '}
              <strong>{worst.usage_percent.toFixed(1)}% full</strong> (threshold{' '}
              {worst.threshold_percent}%), with {worst.available_human} free.
              {alerts.length > 1 && (
                <span className="block mt-1">
                  {alerts.length} disks are over the configured threshold.
                </span>
              )}
              <span className="block mt-1">
                Free up space or adjust the threshold to avoid failed
                deployments and backups.
              </span>
            </AlertDescription>
          </div>
        </div>
        <div className="flex items-center gap-2 ml-4">
          <Link to="/settings/disk-monitoring">
            <Button size="sm" variant="outline" className={accent.button}>
              <Settings className="h-4 w-4 mr-1" />
              <span className="hidden sm:inline">Disk Settings</span>
            </Button>
          </Link>
          {dismissible && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => setIsDismissed(true)}
              className={`h-8 w-8 p-0 ${accent.dismiss}`}
            >
              <X className="h-4 w-4" />
            </Button>
          )}
        </div>
      </div>
    </Alert>
  )
}
