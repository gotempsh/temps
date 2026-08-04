import type { DropPresetCandidate } from '@/api/client'
import { PresetIcon } from '@/components/presets/PresetIcon'
import { cn } from '@/lib/utils'
import { AnimatePresence, motion, useReducedMotion } from 'framer-motion'
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
  const reduceMotion = useReducedMotion()
  const [elapsedSeconds, setElapsedSeconds] = useState(0)
  const transition = reduceMotion
    ? { duration: 0 }
    : { duration: 0.32, ease: [0.22, 1, 0.36, 1] as const }

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
      <AnimatePresence mode="wait" initial={false}>
        {isDetecting ? (
          <motion.div
            key="detecting"
            initial={{ opacity: 0, y: 6 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -6 }}
            transition={transition}
            className="relative flex min-h-28 items-center gap-4 p-4"
          >
            <div className="relative flex size-14 shrink-0 items-center justify-center rounded-xl border bg-background text-primary">
              <ScanSearch className="size-6" />
              {!reduceMotion && (
                <motion.span
                  className="absolute inset-0 rounded-xl border border-primary/50"
                  animate={{
                    opacity: [0.2, 0.8, 0.2],
                    scale: [0.88, 1.08, 0.88],
                  }}
                  transition={{
                    duration: 1.4,
                    repeat: Infinity,
                    ease: 'easeInOut',
                  }}
                />
              )}
            </div>
            <div className="min-w-0">
              <p className="text-sm font-medium">{detectionTitle}</p>
              <p className="mt-1 text-xs leading-5 text-muted-foreground">
                {detectionDescription}
              </p>
            </div>
            {!reduceMotion && (
              <motion.div
                className="pointer-events-none absolute inset-y-0 w-24 bg-gradient-to-r from-transparent via-primary/10 to-transparent"
                initial={{ x: '-140%' }}
                animate={{ x: '520%' }}
                transition={{ duration: 1.6, repeat: Infinity, ease: 'linear' }}
              />
            )}
          </motion.div>
        ) : candidate ? (
          <motion.div
            key={`${candidate.directory}:${candidate.preset}`}
            initial={{ opacity: 0, scale: reduceMotion ? 1 : 0.98, y: 8 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, y: -6 }}
            transition={transition}
            className="flex min-h-28 items-center gap-4 p-4"
          >
            <motion.div
              initial={{
                rotate: reduceMotion ? 0 : -8,
                scale: reduceMotion ? 1 : 0.84,
              }}
              animate={{ rotate: 0, scale: 1 }}
              transition={{ ...transition, delay: reduceMotion ? 0 : 0.08 }}
            >
              <PresetIcon
                preset={candidate.label}
                label={candidate.label}
                className="size-14 bg-white p-2.5 shadow-sm dark:bg-white"
              />
            </motion.div>
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
          </motion.div>
        ) : (
          <motion.div
            key="waiting"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={transition}
            className="flex min-h-28 items-center gap-4 p-4 text-muted-foreground"
          >
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
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
