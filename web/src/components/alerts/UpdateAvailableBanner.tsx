import { Button } from '@/components/ui/button'
import { useUpdateStatus } from '@/hooks/useUpdateStatus'
import { ArrowUpCircle, X } from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'

/** localStorage key holding the latest version the user dismissed. */
const DISMISSED_KEY = 'temps-update-banner-dismissed'

function readDismissedVersion(): string | null {
  try {
    return localStorage.getItem(DISMISSED_KEY)
  } catch {
    // Storage can be unavailable (private mode, disabled) — treat as never
    // dismissed rather than crashing the banner.
    return null
  }
}

/**
 * Dashboard banner shown when the server found a newer temps release on its
 * channel (the backend checks at startup and daily). Dismissal is persisted
 * per version: dismissing v0.2.0 keeps the banner hidden until v0.2.1 (or
 * newer) is published, so it never nags about the same release twice.
 */
export function UpdateAvailableBanner() {
  const { data } = useUpdateStatus()
  // Seed from storage once; the setter below keeps render state and storage
  // in sync when the user dismisses.
  const [dismissedVersion, setDismissedVersion] = useState(readDismissedVersion)

  if (!data?.update_available || !data.latest_version) {
    return null
  }

  if (dismissedVersion === data.latest_version) {
    return null
  }

  const dismiss = () => {
    setDismissedVersion(data.latest_version ?? null)
    try {
      if (data.latest_version) {
        localStorage.setItem(DISMISSED_KEY, data.latest_version)
      }
    } catch {
      // Best-effort persistence; the in-memory state still hides the banner
      // for this session.
    }
  }

  // Informational (not a warning): same thin single-line strip as the
  // disk-space banner, but with a calm blue treatment.
  return (
    <div
      className={cn(
        'flex items-center gap-2 border-b px-4 py-1.5 text-sm',
        'border-blue-200 dark:border-blue-900/50',
        'bg-blue-50/60 dark:bg-blue-950/20',
        'text-blue-800 dark:text-blue-200'
      )}
    >
      <ArrowUpCircle className="h-4 w-4 shrink-0 text-blue-600 dark:text-blue-400" />
      <p className="min-w-0 flex-1 truncate">
        <span className="font-medium">Update available</span>
        <span className="sm:hidden">
          {' '}— <strong>{data.latest_version}</strong>
        </span>
        <span className="hidden sm:inline">
          {' '}— temps <span className="font-mono">{data.current_version}</span> →{' '}
          <strong className="font-mono">{data.latest_version}</strong>
          {data.channel && ` on the ${data.channel} channel`}
        </span>
      </p>
      {data.release_url && (
        <a
          href={data.release_url}
          target="_blank"
          rel="noreferrer"
          className="hidden shrink-0 font-medium underline-offset-2 hover:underline sm:inline text-blue-700 hover:text-blue-900 dark:text-blue-300 dark:hover:text-blue-100"
        >
          Release notes
        </a>
      )}
      <a
        href={data.docs_url}
        target="_blank"
        rel="noreferrer"
        className="shrink-0 font-medium underline-offset-2 hover:underline text-blue-700 hover:text-blue-900 dark:text-blue-300 dark:hover:text-blue-100"
      >
        How to upgrade
      </a>
      <Button
        size="sm"
        variant="ghost"
        onClick={dismiss}
        aria-label="Dismiss update notification"
        className="-mr-1 h-6 w-6 shrink-0 p-0 text-blue-600/70 hover:text-blue-800 dark:text-blue-400/70 dark:hover:text-blue-200"
      >
        <X className="h-3.5 w-3.5" />
      </Button>
    </div>
  )
}
