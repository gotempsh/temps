import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { ArrowRight, Check, Loader2, Wand2 } from 'lucide-react'
import { useNavigate } from 'react-router'
import { useAutofixOnboarding } from './AutofixOnboardingContext'
import {
  useAutofixReadiness,
  type AutofixReadinessStep,
} from '@/hooks/useAutofixReadiness'

/**
 * App-wide setup dialog for AI autofix. Rendered once in the app shell and
 * opened from anywhere via `useAutofixOnboarding().openOnboarding()`.
 *
 * Shows one row per prerequisite so the user can see how far along they are
 * and jump straight to whatever is missing — a provider credential, the
 * sandbox image, or the project's git repository. Steps already satisfied
 * stay visible with a check so progress is legible rather than disappearing.
 */
export function AutofixOnboardingDialog() {
  const { isOpen, target, closeOnboarding } = useAutofixOnboarding()
  const navigate = useNavigate()

  const readiness = useAutofixReadiness({
    projectId: target?.projectId,
    projectSlug: target?.projectSlug,
    projectGit: target?.projectGit,
    enabled: isOpen,
  })

  const goToStep = (step: AutofixReadinessStep) => {
    closeOnboarding()
    navigate(step.to)
  }

  const doneCount = readiness.steps.filter((s) => s.done).length
  const total = readiness.steps.length

  return (
    <Dialog open={isOpen} onOpenChange={(open) => !open && closeOnboarding()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Wand2 className="size-4 text-muted-foreground" />
            Set up AI autofix
          </DialogTitle>
          <DialogDescription>
            Autofix reads your codebase, finds the root cause of an error, and
            opens a pull request with a fix and a test. Finish these steps to
            turn it on.
          </DialogDescription>
        </DialogHeader>

        {readiness.isLoading ? (
          <div className="space-y-3">
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-16 w-full" />
          </div>
        ) : (
          <>
            <p className="text-xs text-muted-foreground">
              {doneCount} of {total} steps complete
            </p>
            <ol className="divide-y divide-gray-950/5 dark:divide-white/10">
              {readiness.steps.map((step, i) => (
                <li
                  key={step.key}
                  className={cn(
                    'flex items-start gap-3',
                    i === 0 ? 'pb-4' : i === total - 1 ? 'pt-4' : 'py-4'
                  )}
                >
                  <div
                    className={cn(
                      'mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full text-xs font-medium',
                      step.done
                        ? 'bg-green-500/10 text-green-600 dark:text-green-400'
                        : 'bg-muted text-muted-foreground'
                    )}
                    aria-hidden="true"
                  >
                    {step.done ? <Check className="size-3.5" /> : i + 1}
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-sm font-medium">{step.title}</p>
                    {!step.done && (
                      <p className="mt-0.5 text-xs text-muted-foreground">
                        {step.description}
                      </p>
                    )}
                  </div>
                  {step.done ? (
                    <span className="mt-0.5 shrink-0 text-xs text-muted-foreground">
                      Ready
                    </span>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      className="shrink-0"
                      onClick={() => goToStep(step)}
                    >
                      {step.ctaLabel}
                      <ArrowRight className="size-3.5" />
                    </Button>
                  )}
                </li>
              ))}
            </ol>
          </>
        )}

        {readiness.isReady && (
          <div className="rounded-md border bg-green-500/5 p-3">
            <p className="text-sm font-medium text-green-700 dark:text-green-400">
              Autofix is ready.
            </p>
            <p className="mt-0.5 text-xs text-muted-foreground">
              Open any unresolved error and choose Fix with AI to start a run.
            </p>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}

/**
 * Small inline spinner used by callers that trigger onboarding while their own
 * readiness check is still resolving.
 */
export function AutofixReadinessSpinner() {
  return <Loader2 className="size-4 animate-spin text-muted-foreground" />
}
