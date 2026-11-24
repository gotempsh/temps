// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ConversationUsage } from '@/api/client'
import {
  compactConversationMutation,
  getUserConversationUsageOptions,
  getUserConversationUsageQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { ContextPie } from './ContextWindow-components'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { cn } from '@/lib/utils'
import { CircleHelp, Shrink } from 'lucide-react'
import { toast } from 'sonner'
import {
  compactResultMessage,
  contextLabel,
  contextLevel,
  contextShare,
  formatSpend,
  formatTokens,
  spendLabel,
  summarizedLabel,
} from './chat-spend'

/** A chat is quiet between turns; this keeps the figure fresh while one runs. */
const REFRESH_MS = 15_000

/** What the model reads on every request: the whole chat so far. */
export function ContextMeter({
  context,
}: {
  context: NonNullable<ConversationUsage['context']>
}) {
  const share = contextShare(context)
  const level = contextLevel(context)
  return (
    <div className="space-y-2" role="group" aria-label="Context size">
      <div className="flex items-baseline justify-between gap-3">
        <p className="font-medium">Context window</p>
        {share !== null && (
          <p className="text-xs tabular-nums text-muted-foreground">
            {percentUsed(share)}% used
          </p>
        )}
      </div>
      {share !== null && (
        <div
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(share * 100)}
          className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
        >
          <div
            className={cn(
              'h-full rounded-full',
              level === 'ok' && 'bg-emerald-500',
              level === 'high' && 'bg-amber-500',
              level === 'critical' && 'bg-red-500'
            )}
            style={{ width: `${Math.max(2, Math.round(share * 100))}%` }}
          />
        </div>
      )}
      <p className="text-xs tabular-nums text-muted-foreground">
        {contextLabel(context)}
      </p>
      <p className="text-xs text-muted-foreground">
        What the model reads on every request: the conversation so far, the
        instructions and the tools.
      </p>
      {level !== 'ok' && (
        <p className="text-xs text-amber-700 dark:text-amber-300">
          A long chat costs more with every message, because all of it is sent
          again. Start a new chat for an unrelated task.
        </p>
      )}
    </div>
  )
}

/** Whole percent for display; "<1" rather than a misleading 0 for a small share. */
function percentUsed(share: number): string {
  return share > 0 && share < 0.01 ? '<1' : String(Math.round(share * 100))
}

/**
 * Summarizing the chat's older messages is automatic past a size, but the
 * button is always here so the user can do it earlier, and the line above it
 * says when it already happened.
 */
export function CompactSection({
  summarizedMessages,
  pending,
  onCompact,
}: {
  summarizedMessages: number
  pending: boolean
  onCompact: () => void
}) {
  const summarized = summarizedLabel(summarizedMessages)
  return (
    <div className="space-y-1.5">
      <p className="text-xs text-muted-foreground">
        {summarized ??
          'Long chats are summarized automatically. Compact now to shrink what the model reads, and what each reply costs, before that.'}
      </p>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="h-8 gap-1.5"
        disabled={pending}
        onClick={onCompact}
      >
        <Shrink className="size-4" />
        {pending ? 'Compacting…' : 'Compact now'}
      </Button>
    </div>
  )
}

function CompactControl({
  conversationPublicId,
  summarizedMessages,
}: {
  conversationPublicId: string
  summarizedMessages: number
}) {
  const queryClient = useQueryClient()
  const compact = useMutation({
    ...compactConversationMutation(),
    onSuccess: (result) => {
      toast.success(compactResultMessage(result))
      void queryClient.invalidateQueries({
        queryKey: getUserConversationUsageQueryKey({
          path: { public_id: conversationPublicId },
        }),
      })
    },
    onError: (error) => {
      toast.error('Could not compact this chat', {
        description: getErrorMessage(error),
      })
    },
  })
  return (
    <CompactSection
      summarizedMessages={summarizedMessages}
      pending={compact.isPending}
      onCompact={() =>
        compact.mutate({ path: { public_id: conversationPublicId } })
      }
    />
  )
}

function SpendBreakdown({
  usage,
  conversationPublicId,
}: {
  usage: ConversationUsage
  conversationPublicId: string
}) {
  return (
    <div className="space-y-3 text-sm">
      {usage.context && <ContextMeter context={usage.context} />}
      <CompactControl
        conversationPublicId={conversationPublicId}
        summarizedMessages={usage.summarized_messages}
      />
      <div>
        <div className="flex items-baseline justify-between gap-3">
          <p className="font-medium">What this chat has cost</p>
          <p className="text-sm tabular-nums">{spendLabel(usage)}</p>
        </div>
        <p className="text-xs text-muted-foreground">
          An estimate from each model&apos;s published price and the tokens it
          used.
        </p>
      </div>
      {usage.models.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          Nothing yet. Ask something and the spend appears here.
        </p>
      ) : (
        <ul className="space-y-1.5">
          {usage.models.map((model) => (
            <li
              key={model.model}
              className="flex items-start justify-between gap-3"
            >
              <span className="min-w-0">
                <span className="block truncate font-mono text-xs">
                  {model.model}
                </span>
                <span className="text-xs text-muted-foreground">
                  {model.requests}{' '}
                  {model.requests === 1 ? 'request' : 'requests'}
                  {' · '}
                  {formatTokens(model.input_tokens)} in,{' '}
                  {formatTokens(model.output_tokens)} out
                </span>
              </span>
              <span className="shrink-0 tabular-nums">
                {formatSpend(model.cost_microcents)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {usage.unpriced_requests > 0 && (
        <p className="text-xs text-amber-700 dark:text-amber-300">
          {usage.unpriced_requests}{' '}
          {usage.unpriced_requests === 1 ? 'request has' : 'requests have'} no
          known price, so the total is a minimum.
        </p>
      )}
      {usage.byok_requests > 0 && (
        <p className="text-xs text-muted-foreground">
          {usage.byok_requests}{' '}
          {usage.byok_requests === 1 ? 'request used' : 'requests used'} your
          own provider key, which your provider bills you for directly.
        </p>
      )}
    </div>
  )
}

/**
 * How full the chat's context is, shown beside the send button as a ring and a
 * percentage. The popover holds the figures behind it, the compact action and
 * what the chat has cost, so none of it is something the user has to know to
 * look for.
 */
export function ChatContextMeter({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const usage = useQuery({
    ...getUserConversationUsageOptions({
      path: { public_id: conversationPublicId },
    }),
    refetchInterval: REFRESH_MS,
    refetchIntervalInBackground: false,
  })

  if (usage.isPending) {
    return <Skeleton className="h-8 w-14 rounded-full" />
  }

  const context = usage.data?.context
  const share = context ? contextShare(context) : null
  const label = share !== null ? `${percentUsed(share)}%` : 'Context'

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label="Context window and spend"
          title="How much of the model's context window this chat uses, and what it has cost"
          className="flex h-8 items-center gap-1.5 rounded-full px-2 text-xs tabular-nums text-muted-foreground hover:bg-muted hover:text-foreground"
        >
          {share === null ? (
            <CircleHelp className="size-4" />
          ) : (
            <ContextPie percentage={Math.round(share * 100)} />
          )}
          <span className={share === null ? 'sr-only' : undefined}>
            {label}
          </span>
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" side="top" className="w-80">
        {usage.data ? (
          <SpendBreakdown
            usage={usage.data}
            conversationPublicId={conversationPublicId}
          />
        ) : (
          <p className="text-sm text-muted-foreground">
            The usage for this chat could not be loaded. It will retry on its
            own.
          </p>
        )}
      </PopoverContent>
    </Popover>
  )
}
