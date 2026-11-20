// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  cancelGoal,
  retryGoal,
  type ChatOperation,
  type ChatOperationState,
  type GoalResponse,
} from '@/api/client'
import {
  listGoalsOptions,
  listGoalsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  AlertTriangle,
  Ban,
  CheckCircle2,
  ChevronRight,
  Loader2,
  RotateCcw,
  Target,
  XCircle,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  indicatorSummary,
  operationStateLabel,
  useChatOperations,
  type IndicatorState,
} from './chat-operations'
import {
  goalPollingInterval,
  presentGoal,
  visibleGoals,
  type GoalTone,
} from './goal-presentation'
import { IconWithStatus, type IconStatusTone } from './IconWithStatus'

const TONE_CLASSES: Record<GoalTone, string> = {
  working: 'text-sky-700 dark:text-sky-300',
  good: 'text-emerald-700 dark:text-emerald-300',
  attention: 'text-amber-700 dark:text-amber-300',
  bad: 'text-destructive',
  muted: 'text-muted-foreground',
}

function GoalIcon({
  status,
  className,
}: {
  status: string
  className: string
}) {
  switch (status) {
    case 'achieved':
      return <CheckCircle2 className={className} />
    case 'failed':
      return <XCircle className={className} />
    case 'needs_you':
      return <AlertTriangle className={className} />
    case 'cancelled':
      return <Ban className={className} />
    default:
      return <Loader2 className={cn(className, 'animate-spin')} />
  }
}

function GoalRow({
  goal,
  onStop,
  onRetry,
  busy,
}: {
  goal: GoalResponse
  onStop: () => void
  onRetry: () => void
  busy: boolean
}) {
  const view = presentGoal(goal)
  const tone = TONE_CLASSES[view.tone]
  return (
    <li className="flex items-start gap-2 px-3 py-2">
      <GoalIcon
        status={goal.status}
        className={cn('mt-0.5 size-4 shrink-0', tone)}
      />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{goal.title}</p>
        <p className="text-xs text-muted-foreground">
          <span className={cn('font-medium', tone)}>{view.label}</span>
          {goal.last_observation ? ` · ${goal.last_observation}` : ''}
          {goal.last_checked_at ? (
            <>
              {' · checked '}
              <TimeAgo date={goal.last_checked_at} />
            </>
          ) : null}
        </p>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {view.canRetry && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 gap-1 px-2 text-xs"
            disabled={busy}
            onClick={onRetry}
          >
            <RotateCcw className="size-3" />
            Try again
          </Button>
        )}
        {view.canStop && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs text-muted-foreground"
            disabled={busy}
            onClick={onStop}
          >
            Stop
          </Button>
        )}
      </div>
    </li>
  )
}

/** The chat's goals, polled quickly while one is open. */
export function useChatGoals(conversationPublicId: string) {
  return useQuery({
    ...listGoalsOptions({ path: { public_id: conversationPublicId } }),
    refetchInterval: (query) => goalPollingInterval(query.state.data ?? []),
  })
}

/** Stop and retry for a chat's goals, refreshing the list after each. */
function useGoalActions(conversationPublicId: string) {
  const queryClient = useQueryClient()
  const options = { path: { public_id: conversationPublicId } }
  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: listGoalsQueryKey(options) })

  const stop = useMutation({
    mutationFn: async (goalId: string) => {
      const { error } = await cancelGoal({
        path: { public_id: conversationPublicId, goal_id: goalId },
      })
      if (error) throw error
    },
    onSuccess: () => {
      toast.success('Stopped watching')
      void refresh()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not stop the goal')),
  })
  const retry = useMutation({
    mutationFn: async (goalId: string) => {
      const { error } = await retryGoal({
        path: { public_id: conversationPublicId, goal_id: goalId },
      })
      if (error) throw error
    },
    onSuccess: () => {
      toast.success('Checking again')
      void refresh()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not retry the goal')),
  })
  return { stop, retry, busy: stop.isPending || retry.isPending }
}

const INDICATOR_TONES: Record<IndicatorState, IconStatusTone | null> = {
  idle: null,
  working: 'working',
  attention: 'attention',
  failed: 'bad',
}

function OperationsIcon({ state }: { state: IndicatorState }) {
  return (
    <IconWithStatus
      icon={<Activity className="size-4" />}
      tone={INDICATOR_TONES[state]}
    />
  )
}

const OPERATION_TONES: Record<ChatOperationState, GoalTone> = {
  running: 'working',
  succeeded: 'good',
  failed: 'bad',
  cancelled: 'muted',
  not_started: 'attention',
}

function OperationIcon({
  state,
  className,
}: {
  state: ChatOperationState
  className: string
}) {
  switch (state) {
    case 'succeeded':
      return <CheckCircle2 className={className} />
    case 'failed':
      return <XCircle className={className} />
    case 'cancelled':
      return <Ban className={className} />
    case 'not_started':
      return <AlertTriangle className={className} />
    default:
      return <Loader2 className={cn(className, 'animate-spin')} />
  }
}

function OperationRow({
  operation,
  onOpen,
}: {
  operation: ChatOperation
  onOpen: () => void
}) {
  const tone = TONE_CLASSES[OPERATION_TONES[operation.state]]
  const body = (
    <>
      <OperationIcon
        state={operation.state}
        className={cn('mt-0.5 size-4 shrink-0', tone)}
      />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{operation.title}</p>
        {operation.detail && (
          <p className="truncate text-xs text-muted-foreground">
            {operation.detail}
          </p>
        )}
        <p className="text-xs text-muted-foreground">
          <span className={cn('font-medium', tone)}>
            {operationStateLabel(operation)}
          </span>
          {' · started '}
          <TimeAgo date={operation.started_at} />
        </p>
        {operation.error && operation.state !== 'running' && (
          <p className="mt-0.5 line-clamp-2 text-xs text-destructive">
            {operation.error}
          </p>
        )}
      </div>
      {operation.href && (
        <ChevronRight className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      )}
    </>
  )
  return (
    <li>
      {operation.href ? (
        <Link
          to={operation.href}
          onClick={onOpen}
          className="flex items-start gap-2 px-3 py-2 hover:bg-muted/50"
        >
          {body}
        </Link>
      ) : (
        <div className="flex items-start gap-2 px-3 py-2">{body}</div>
      )}
    </li>
  )
}

function SectionHeading({
  icon: Icon,
  children,
}: {
  icon: typeof Target
  children: string
}) {
  return (
    <h3 className="flex items-center gap-1.5 border-b px-3 py-2 text-xs font-medium text-muted-foreground">
      <Icon className="size-3.5" />
      {children}
    </h3>
  )
}

/**
 * What this chat started and how it is going: the deployments, backups and
 * restores it ran, read live from each one, and the goals Temps is watching
 * for it. Always shown, so the place to look exists before there is anything
 * in it; the button turns busy, amber or red when there is something to see.
 */
export function ChatOperationsIndicator({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const [open, setOpen] = useState(false)
  const operations = useChatOperations(conversationPublicId)
  const goals = useChatGoals(conversationPublicId)
  const { stop, retry, busy } = useGoalActions(conversationPublicId)
  const started: ChatOperation[] = operations.data?.operations ?? []
  const allGoals: GoalResponse[] = goals.data ?? []
  const summary = indicatorSummary(started, allGoals)
  const shownGoals = visibleGoals(allGoals)
  const empty = started.length === 0 && shownGoals.length === 0
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-8"
          title={`${summary.label}: deployments, backups and restores this chat started`}
          aria-label={`Activity: ${summary.label}`}
        >
          <OperationsIcon state={summary.state} />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-96 p-0">
        {operations.isError && (
          <p className="border-b px-3 py-2 text-xs text-destructive">
            {getErrorMessage(
              operations.error,
              'Could not load what this chat started'
            )}
          </p>
        )}
        {empty ? (
          <div className="space-y-1 px-3 py-3 text-sm">
            <p className="font-medium">Nothing running</p>
            <p className="text-xs text-muted-foreground">
              Deployments, backups and restores this chat starts show up here
              with their progress, e.g. after &ldquo;deploy my app&rdquo; or
              &ldquo;back up the orders database&rdquo;.
            </p>
          </div>
        ) : (
          <div className="max-h-[70vh] overflow-y-auto">
            {started.length > 0 && (
              <>
                <SectionHeading icon={Activity}>
                  Started in this chat
                </SectionHeading>
                <ul className="divide-y">
                  {started.map((operation) => (
                    <OperationRow
                      key={operation.id}
                      operation={operation}
                      onOpen={() => setOpen(false)}
                    />
                  ))}
                </ul>
                {operations.data?.truncated && (
                  <p className="border-t px-3 py-2 text-xs text-muted-foreground">
                    Showing the newest {started.length}.
                  </p>
                )}
              </>
            )}
            {shownGoals.length > 0 && (
              <>
                <SectionHeading icon={Target}>Watching for you</SectionHeading>
                <ul className="divide-y">
                  {shownGoals.map((goal) => (
                    <GoalRow
                      key={goal.id}
                      goal={goal}
                      busy={busy}
                      onStop={() => stop.mutate(goal.id)}
                      onRetry={() => retry.mutate(goal.id)}
                    />
                  ))}
                </ul>
              </>
            )}
          </div>
        )}
      </PopoverContent>
    </Popover>
  )
}
