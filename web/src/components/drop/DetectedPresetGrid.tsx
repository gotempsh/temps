import type { DropPresetCandidate } from '@/api/client'
import { PresetIcon } from '@/components/presets/PresetIcon'
import { cn } from '@/lib/utils'
import { Check, Folder } from 'lucide-react'

export function DetectedPresetGrid({
  candidates,
  selectedIndex,
  onSelect,
  disabled = false,
}: {
  candidates: DropPresetCandidate[]
  selectedIndex: number
  onSelect: (index: number) => void
  disabled?: boolean
}) {
  return (
    <div
      className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3"
      role="radiogroup"
      aria-label="Detected projects"
    >
      {candidates.map((candidate, index) => {
        const selected = index === selectedIndex
        const directory = candidate.directory || '.'

        return (
          <button
            key={`${candidate.preset}:${directory}:${index}`}
            type="button"
            role="radio"
            aria-checked={selected}
            disabled={disabled}
            onClick={() => onSelect(index)}
            className={cn(
              'relative min-h-36 rounded-xl border bg-card p-4 text-left transition-all duration-200',
              'hover:-translate-y-0.5 hover:border-muted-foreground/50 hover:shadow-sm',
              'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
              selected && 'border-primary ring-2 ring-primary/35',
              disabled && 'cursor-not-allowed opacity-60 hover:translate-y-0'
            )}
          >
            {selected && (
              <span className="absolute right-3 top-3 flex size-5 items-center justify-center rounded-full bg-primary text-primary-foreground">
                <Check className="size-3" />
              </span>
            )}

            <div className="flex items-start gap-3">
              <PresetIcon
                preset={candidate.label}
                label={candidate.label}
                className="size-11 shrink-0 bg-white p-2 dark:bg-white"
              />
              <div className="min-w-0 flex-1 pr-5">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="truncate text-sm font-semibold">
                    {candidate.label}
                  </span>
                  <span
                    className={cn(
                      'rounded-full px-2 py-0.5 text-[10px] font-medium capitalize',
                      candidate.confidence.toLowerCase() === 'high'
                        ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                        : 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
                    )}
                  >
                    {candidate.confidence}
                  </span>
                </div>
                <p className="mt-1 line-clamp-2 text-xs leading-5 text-muted-foreground">
                  {candidate.reason}
                </p>
              </div>
            </div>

            <div className="mt-3 flex items-center gap-1.5 border-t pt-3 text-xs text-muted-foreground">
              <Folder className="size-3 shrink-0" />
              <span className="truncate font-mono" title={directory}>
                {directory}
              </span>
            </div>
          </button>
        )
      })}
    </div>
  )
}
