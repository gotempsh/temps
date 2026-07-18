import { Button } from '@/components/ui/button'
import { Lightbulb } from 'lucide-react'
import { useCallback, useState } from 'react'

/**
 * Shared show/hide preference for the analytics insights panels. One key
 * for all analytics pages: opening insights on one page opens them
 * everywhere, since it expresses a single "I want insights" preference.
 */
const STORAGE_KEY = 'temps.analytics.insights.open'

export function useInsightsOpen(): [boolean, (open: boolean) => void] {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === 'true'
    } catch {
      return false
    }
  })
  const update = useCallback((next: boolean) => {
    setOpen(next)
    try {
      localStorage.setItem(STORAGE_KEY, String(next))
    } catch {
      // Preference just won't persist — toggling still works this session.
    }
  }, [])
  return [open, update]
}

interface InsightsToggleButtonProps {
  open: boolean
  onToggle: (open: boolean) => void
}

/** Compact icon button that shows or hides a page's insights panel. */
export function InsightsToggleButton({
  open,
  onToggle,
}: InsightsToggleButtonProps) {
  const label = open ? 'Hide insights' : 'Show insights'
  return (
    <Button
      variant={open ? 'secondary' : 'outline'}
      size="sm"
      aria-pressed={open}
      title={label}
      onClick={() => onToggle(!open)}
    >
      <Lightbulb className="size-4 shrink-0" />
      <span className="sr-only">{label}</span>
    </Button>
  )
}
