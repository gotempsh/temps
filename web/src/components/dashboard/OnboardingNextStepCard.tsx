import { Button } from '@/components/ui/button'
import { useGettingStarted } from '@/hooks/useGettingStarted'
import {
  ArrowRight,
  Bell,
  Bot,
  Check,
  ChevronLeft,
  ChevronRight,
  Database,
  DatabaseBackup,
  GitBranch,
  Globe,
  ShieldCheck,
  Sparkles,
  Users,
  type LucideIcon,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { firstIncompleteGettingStartedIndex } from './onboarding-next-step'

const STEP_ICONS: Record<string, LucideIcon> = {
  ai: Bot,
  git: GitBranch,
  domain: Globe,
  notifications: Bell,
  dns: ShieldCheck,
  database: Database,
  backups: DatabaseBackup,
  team: Users,
}

export function OnboardingNextStepCard() {
  const { items, totalCount, visible } = useGettingStarted()
  const [selectedStepKey, setSelectedStepKey] = useState<string | null>(null)
  const firstIncompleteIndex = firstIncompleteGettingStartedIndex(items)
  const selectedIndex = selectedStepKey
    ? items.findIndex((item) => item.key === selectedStepKey)
    : firstIncompleteIndex
  const safeSelectedIndex = selectedIndex >= 0 ? selectedIndex : 0
  const selectedStep = items[safeSelectedIndex]

  if (!visible || !selectedStep) return null

  const Icon = STEP_ICONS[selectedStep.key] ?? Sparkles
  const isRecommendedStep = safeSelectedIndex === firstIncompleteIndex

  return (
    <section
      aria-labelledby="dashboard-onboarding-title"
      className="rounded-xl border border-primary/20 bg-card px-4 py-3"
    >
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="flex min-w-0 flex-1 items-start gap-3 sm:items-center">
          <div className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50 text-primary">
            <Icon className="size-4" />
          </div>

          <div className="min-w-0">
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
              <Link
                to="/setup"
                className="text-[11px] font-medium uppercase tracking-wider text-primary hover:underline"
              >
                {isRecommendedStep ? 'Up next' : 'Onboarding checklist'}
              </Link>
              <h2
                id="dashboard-onboarding-title"
                className="text-sm font-semibold"
              >
                {selectedStep.label}
              </h2>
              {selectedStep.done && (
                <span className="inline-flex items-center gap-1 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
                  <Check className="size-3" />
                  Complete
                </span>
              )}
            </div>
            <p className="mt-0.5 text-xs leading-5 text-muted-foreground sm:truncate">
              {selectedStep.description}
            </p>
          </div>
        </div>

        <div className="flex w-full shrink-0 items-center justify-end gap-2 pl-12 sm:w-auto sm:pl-0">
          <Button asChild size="sm">
            <Link to={selectedStep.href}>
              {selectedStep.cta}
              <ArrowRight className="size-3.5" />
            </Link>
          </Button>
          <div className="flex w-[7.5rem] shrink-0 items-center justify-end gap-2">
            <Button
              variant="ghost"
              size="icon"
              className="size-8"
              aria-label="Previous onboarding step"
              disabled={safeSelectedIndex === 0}
              onClick={() =>
                setSelectedStepKey(items[safeSelectedIndex - 1]?.key ?? null)
              }
            >
              <ChevronLeft className="size-4" />
            </Button>
            <span className="min-w-10 text-center text-xs tabular-nums text-muted-foreground">
              {safeSelectedIndex + 1} / {totalCount}
            </span>
            <Button
              variant="ghost"
              size="icon"
              className="size-8"
              aria-label="Next onboarding step"
              disabled={safeSelectedIndex === items.length - 1}
              onClick={() =>
                setSelectedStepKey(items[safeSelectedIndex + 1]?.key ?? null)
              }
            >
              <ChevronRight className="size-4" />
            </Button>
          </div>
        </div>
      </div>
    </section>
  )
}
