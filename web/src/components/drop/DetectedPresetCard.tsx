// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DropPresetCandidate } from '@/api/client'
import { PresetIcon } from '@/components/presets/PresetIcon'
import { cn } from '@/lib/utils'
import { CheckCircle2, ScanSearch } from 'lucide-react'
import { useEffect, useState } from 'react'

export function DetectedPresetCard({
  candidate,
  isDetecting,
  phase = 'detecting',
}: {
  candidate?: DropPresetCandidate
  isDetecting: boolean
  phase?: 'packing' | 'detecting'
}) {
  const [elapsedSeconds, setElapsedSeconds] = useState(0)

  useEffect(() => {
    if (!isDetecting) return
    const startedAt = Date.now()
    const timer = window.setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - startedAt) / 1000))
    }, 1000)
    return () => {
      window.clearInterval(timer)
      setElapsedSeconds(0)
    }
  }, [isDetecting])

  const detectionTitle =
    phase === 'packing' ? 'Packaging project files' : 'Inspecting project files'
  const detectionDescription =
    elapsedSeconds >= 5
      ? `Still working — larger projects can take a little longer. ${elapsedSeconds}s elapsed. You can cancel with the X.`
      : phase === 'packing'
        ? 'Creating a secure archive in your browser…'
        : 'Uploading the archive and reading framework manifests…'

  return (
    <div className="relative min-h-28 overflow-hidden rounded-2xl border bg-muted/25">
      <>
        {isDetecting ? (
          <div className="relative flex min-h-28 animate-in items-center gap-4 p-4 fade-in-0 motion-reduce:animate-none">
            <div className="relative flex size-14 shrink-0 items-center justify-center rounded-xl border bg-background text-primary">
              <ScanSearch className="size-6" />
              <span className="absolute inset-0 animate-ping rounded-xl border border-primary/50 motion-reduce:hidden" />
            </div>
            <div className="min-w-0">
              <p className="text-sm font-medium">{detectionTitle}</p>
              <p className="mt-1 text-xs leading-5 text-muted-foreground">
                {detectionDescription}
              </p>
            </div>
            <div className="pointer-events-none absolute inset-y-0 left-0 w-24 animate-pulse bg-gradient-to-r from-transparent via-primary/10 to-transparent motion-reduce:hidden" />
          </div>
        ) : candidate ? (
          <div className="flex min-h-28 animate-in items-center gap-4 p-4 fade-in-0 zoom-in-95 motion-reduce:animate-none">
            <div>
              <PresetIcon
                preset={candidate.label}
                label={candidate.label}
                className="size-14 bg-white p-2.5 shadow-sm dark:bg-white"
              />
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <p className="text-sm font-semibold">{candidate.label}</p>
                <span
                  className={cn(
                    'rounded-full px-2 py-0.5 text-[10px] font-medium capitalize',
                    candidate.confidence.toLowerCase() === 'high'
                      ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                      : 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
                  )}
                >
                  {candidate.confidence} confidence
                </span>
              </div>
              <p className="mt-1 line-clamp-2 text-xs leading-5 text-muted-foreground">
                {candidate.reason}
              </p>
            </div>
            <CheckCircle2 className="size-5 shrink-0 text-emerald-500" />
          </div>
        ) : (
          <div className="flex min-h-28 animate-in items-center gap-4 p-4 text-muted-foreground fade-in-0 motion-reduce:animate-none">
            <div className="flex size-14 shrink-0 items-center justify-center rounded-xl border border-dashed bg-background/60">
              <ScanSearch className="size-6" strokeWidth={1.5} />
            </div>
            <div>
              <p className="text-sm font-medium text-foreground">
                Preset detection
              </p>
              <p className="mt-1 text-xs leading-5">
                Select a folder or archive and Temps will identify the framework
                automatically.
              </p>
            </div>
          </div>
        )}
      </>
    </div>
  )
}
