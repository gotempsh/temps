import { useState } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2, Settings2, ShieldCheck, Sparkles } from 'lucide-react'
import { toast } from 'sonner'

// REGEN: bun run openapi-ts — generated from the new
// GET /projects/{project_id}/ai/readiness endpoint (operationId
// get_chat_readiness). Imported from the generated SDK rather than hand-rolled
// (project rule: always use the generated SDK in web/).
import {
  getChatReadinessOptions,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'

import { useAiAssistant } from '@/components/ai/AiAssistantContext'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ENABLE_AI_WRITES_BODY,
  focusedStartPrompt,
  suggestAlertsState,
} from './suggest-alerts-state'

const START_PROMPT =
  'Look at what this project already alerts on and what metrics it actually reports, then propose the alert rules worth adding. Ground every threshold in real values you queried, and backtest anomaly detectors before proposing them.'

/**
 * Opened from the explorer with a metric on screen, the question is narrower —
 * "should I alert on *this*" — so lead with that metric instead of restarting
 * the whole survey, while still letting the assistant mention anything else
 * obviously worth covering.
 */
interface SuggestAlertsButtonProps {
  projectId: number
  projectSlug?: string
  projectName?: string
  /** Rendered inside an EmptyState — slightly more prominent there. */
  variant?: 'outline' | 'default'
  /**
   * The metric the user is currently viewing, when there is one. Narrows the
   * opening question to that metric; omit for the general "what should I alert
   * on?" survey.
   */
  focusMetric?: string
}

/**
 * "Suggest alerts with AI" — opens a chat that proposes metric alert rules for
 * this project, each staged as a confirm-gated write action.
 *
 * The button is always visible, because the interesting case is the one where
 * the feature is *not* set up yet: hiding it would leave a self-hosted user with
 * no way to discover that the capability exists, and no way to find the two
 * settings that unlock it. Instead it renders in whatever state the project is
 * actually in and explains the next step:
 *
 * - no AI provider on the instance → point at Settings → AI Providers
 * - write actions off for this project → offer to turn them on, in one click,
 *   behind a confirmation that says what that grants
 * - everything ready → open the chat
 *
 * The two gates are independent and fixed in different places (one instance-wide,
 * one per project), which is why the readiness endpoint reports them separately
 * instead of as a single "unavailable".
 */
export function SuggestAlertsButton({
  projectId,
  projectSlug,
  projectName,
  variant = 'outline',
  focusMetric,
}: SuggestAlertsButtonProps) {
  const { open } = useAiAssistant()
  const queryClient = useQueryClient()
  const [setupOpen, setSetupOpen] = useState(false)

  const readinessQuery = useQuery({
    ...getChatReadinessOptions({ path: { project_id: projectId } }),
    enabled: !!projectId,
  })

  const enableMutation = useMutation({
    ...updateProjectSettingsMutation(),
    meta: { errorTitle: 'Failed to enable AI write actions' },
    onSuccess: () => {
      toast.success('AI write actions enabled for this project')
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id ===
          'getChatReadiness',
      })
      setSetupOpen(false)
      openChat()
    },
  })

  const openChat = () =>
    open({
      projectId,
      // The conversation is keyed on the project, so returning here resumes the
      // same chat rather than starting a parallel one per metric. `focusMetric`
      // only steers the opening question of a NEW chat.
      context: {
        contextType: 'alert_suggest',
        contextId: projectId,
        title: 'Suggest alerts',
        description:
          'Ask AI which metrics are worth alerting on. Each suggested rule is proposed for you to review — nothing is created until you confirm it.',
        startPrompt: focusMetric
          ? focusedStartPrompt(focusMetric)
          : START_PROMPT,
        projectSlug,
        projectName,
      },
    })

  const readiness = readinessQuery.data
  const aiConfigured = readiness?.ai_configured === true
  const writeEnabled = readiness?.write_actions_enabled === true
  const state = suggestAlertsState({
    isPending: readinessQuery.isPending,
    isError: readinessQuery.isError,
    aiConfigured,
    writeEnabled,
  })

  // Don't offer an action whose outcome we can't predict yet.
  if (state === 'loading') {
    return <Skeleton className="h-8 w-40" />
  }

  // A failed readiness check is not a reason to hide the feature — but it IS a
  // reason not to promise it works. Let the click through to the chat, which
  // surfaces its own error if the prerequisites really are missing.
  const needsSetup = state !== 'ready'

  return (
    <>
      <Button
        size="sm"
        variant={variant}
        className="gap-1.5"
        onClick={() => (needsSetup ? setSetupOpen(true) : openChat())}
      >
        <Sparkles className="size-4 text-primary" />
        Suggest alerts with AI
      </Button>

      <AlertDialog open={setupOpen} onOpenChange={setSetupOpen}>
        <AlertDialogContent>
          {state === 'configure-provider' ? (
            <>
              <AlertDialogHeader>
                <AlertDialogTitle className="flex items-center gap-2">
                  <Settings2 className="size-4" />
                  Configure an AI provider first
                </AlertDialogTitle>
                <AlertDialogDescription>
                  Suggesting alerts needs a model to reason over your metrics,
                  and this instance has no AI provider configured yet. Add one
                  in Settings → AI Providers (you bring your own key — Temps
                  stores it encrypted and calls the provider directly). Come
                  back here afterwards and this button will open the chat.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Close</AlertDialogCancel>
                <AlertDialogAction asChild>
                  <Link to="/settings/ai-providers">Go to AI Providers</Link>
                </AlertDialogAction>
              </AlertDialogFooter>
            </>
          ) : (
            <>
              <AlertDialogHeader>
                <AlertDialogTitle className="flex items-center gap-2">
                  <ShieldCheck className="size-4" />
                  Enable AI write actions?
                </AlertDialogTitle>
                <AlertDialogDescription>
                  To suggest alerts, the assistant needs to be able to{' '}
                  <strong>propose</strong> changes to this project. Nothing runs
                  automatically: each suggested rule appears in the chat as a
                  separate action that waits for you to review and{' '}
                  <strong>Confirm</strong> it, and it runs with your own
                  permissions. You can turn this off anytime in Settings →
                  Security.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel disabled={enableMutation.isPending}>
                  Cancel
                </AlertDialogCancel>
                <AlertDialogAction
                  disabled={enableMutation.isPending}
                  onClick={(e) => {
                    e.preventDefault()
                    enableMutation.mutate({
                      path: { project_id: projectId },
                      // Enable the read-only chat too, so the project can never
                      // end up with write actions on but the chat they're
                      // proposed in switched off.
                      body: ENABLE_AI_WRITES_BODY,
                    })
                  }}
                >
                  {enableMutation.isPending && (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  )}
                  Enable and continue
                </AlertDialogAction>
              </AlertDialogFooter>
            </>
          )}
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
