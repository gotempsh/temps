// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  suggestNextSteps,
  type ChatSuggestionSettings,
  type SuggestionMode,
} from '@/api/client'
import {
  getChatSuggestionModeOptions,
  getSuggestionDefaultOptions,
  updateChatSuggestionModeMutation,
  updateSuggestionDefaultMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowRight,
  Check,
  Lightbulb,
  Pencil,
  RefreshCw,
  X,
} from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  nextStepsRow,
  SUGGESTION_MODE_LABELS,
  suggestionModeSummary,
} from './next-steps'

const MODES: SuggestionMode[] = ['ask', 'always', 'off']

/** The chat's suggestion setting, shared by the row and the composer control. */
function useChatSuggestionSettings(conversationPublicId: string) {
  return useQuery(
    getChatSuggestionModeOptions({ path: { public_id: conversationPublicId } })
  )
}

/** Saving either setting changes what every chat follows, so refresh them all. */
function useSuggestionSettingsMutations() {
  const queryClient = useQueryClient()
  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({
        queryKey: [{ _id: 'getChatSuggestionMode' }],
      }),
      queryClient.invalidateQueries({
        queryKey: [{ _id: 'getSuggestionDefault' }],
      }),
    ])
  const chat = useMutation({
    ...updateChatSuggestionModeMutation(),
    onSuccess: () => void refresh(),
    onError: (error) =>
      toast.error(
        getErrorMessage(error, 'Could not save suggestions for this chat')
      ),
  })
  const all = useMutation({
    ...updateSuggestionDefaultMutation(),
    onSuccess: () => void refresh(),
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not save your default')),
  })
  return { chat, all }
}

/**
 * "Suggest next steps?" with the three answers. Asking costs nothing. "Yes"
 * turns them on for this chat, "Always" for every chat, "No" skips this answer.
 */
export function NextStepsOffer({
  onYes,
  onAlways,
  onNo,
  disabled,
}: {
  onYes: () => void
  onAlways: () => void
  onNo: () => void
  disabled?: boolean
}) {
  return (
    <div className="flex flex-wrap items-center gap-1.5 pl-1 text-xs text-muted-foreground">
      <Lightbulb className="size-3.5 shrink-0" aria-hidden="true" />
      <span>Suggest next steps?</span>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="h-6 px-2 text-xs"
        disabled={disabled}
        onClick={onYes}
        title="Suggest after every answer in this chat"
      >
        Yes
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="h-6 px-2 text-xs"
        disabled={disabled}
        onClick={onAlways}
        title="Suggest after every answer in all your chats, this one included"
      >
        Always
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="h-6 px-2 text-xs"
        disabled={disabled}
        onClick={onNo}
        title="Not for this answer"
      >
        No
      </Button>
    </div>
  )
}

/**
 * The suggestions themselves. Picking one sends it as the next message; the
 * pencil puts it in the composer instead, for a step worth adjusting before it
 * runs (a redeploy, a deletion).
 */
export function NextStepsChips({
  suggestions,
  onPick,
  onEdit,
  onDismiss,
}: {
  suggestions: string[]
  onPick: (text: string) => void
  onEdit?: (text: string) => void
  onDismiss?: () => void
}) {
  return (
    <section className="max-w-2xl pl-1" aria-label="Suggested next steps">
      <div className="mb-1.5 flex items-center gap-1.5 text-xs text-muted-foreground">
        <Lightbulb
          className="size-3.5 shrink-0 text-amber-500"
          aria-hidden="true"
        />
        <span className="font-medium">Suggested next steps</span>
        {onDismiss && (
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="ml-auto size-6"
            aria-label="Hide suggestions"
            title="Hide suggestions"
            onClick={onDismiss}
          >
            <X className="size-3.5" />
          </Button>
        )}
      </div>
      <ul className="divide-y overflow-hidden rounded-lg border bg-background">
        {suggestions.map((suggestion) => (
          <li key={suggestion} className="group flex items-stretch">
            <button
              type="button"
              className="flex min-w-0 flex-1 items-start gap-2 px-3 py-2 text-left text-sm transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none"
              onClick={() => onPick(suggestion)}
              title="Send this as your next message"
            >
              <ArrowRight
                className="mt-0.5 size-3.5 shrink-0 text-muted-foreground transition-colors group-hover:text-foreground"
                aria-hidden="true"
              />
              <span className="min-w-0">{suggestion}</span>
            </button>
            {onEdit && (
              <button
                type="button"
                className="flex shrink-0 items-center px-2.5 text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:bg-muted/60 focus-visible:outline-none sm:opacity-0 sm:group-hover:opacity-100 sm:focus-visible:opacity-100"
                onClick={() => onEdit(suggestion)}
                aria-label={`Edit before sending: ${suggestion}`}
                title="Edit before sending"
              >
                <Pencil className="size-3.5" aria-hidden="true" />
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}

function NextStepsSkeleton() {
  return (
    <div className="max-w-2xl space-y-1.5 pl-1" role="status">
      <span className="sr-only">Suggesting next steps…</span>
      <Skeleton className="h-4 w-40" />
      <div className="divide-y rounded-lg border">
        {['w-3/4', 'w-2/3', 'w-1/2'].map((width) => (
          <div key={width} className="px-3 py-2.5">
            <Skeleton className={cn('h-4', width)} />
          </div>
        ))}
      </div>
    </div>
  )
}

/**
 * Next steps under the chat's latest answer. Render it keyed by the answer, so
 * a "No" or "Yes" applies to that answer only.
 */
export function ChatNextSteps({
  conversationPublicId,
  answerKey,
  answerCursor,
  saved,
  answeredWhileOpen,
  onPick,
  onEdit,
}: {
  conversationPublicId: string
  answerKey: string
  /** The stored answer's cursor, so the server can check it is still the latest. */
  answerCursor?: string
  /** Suggestions the server already saved on this answer. */
  saved?: string[]
  answeredWhileOpen: boolean
  onPick: (text: string) => void
  /** Put a suggestion in the composer instead of sending it. */
  onEdit?: (text: string) => void
}) {
  const [accepted, setAccepted] = useState(false)
  const [dismissed, setDismissed] = useState(false)
  const settings = useChatSuggestionSettings(conversationPublicId)
  const { chat, all } = useSuggestionSettingsMutations()
  const row = settings.data?.available
    ? nextStepsRow({
        mode: settings.data.mode,
        accepted,
        dismissed,
        answeredWhileOpen,
        hasSaved: (saved?.length ?? 0) > 0,
      })
    : 'hidden'
  const steps = useQuery({
    queryKey: ['next-steps', conversationPublicId, answerKey],
    queryFn: async () => {
      const { data, error } = await suggestNextSteps({
        path: { public_id: conversationPublicId },
        body: { answer: answerCursor },
      })
      if (error) throw error
      return data
    },
    enabled: row === 'load',
    // The row explains its own failure with a retry, and suggestions are an
    // extra: a failed set is not the page failing to load.
    meta: { routeFailure: ROUTE_FAILURE_IGNORE },
    // The server saves the first set on the answer and returns it for any
    // later request, so this cache only spares a round trip.
    staleTime: Infinity,
    retry: false,
  })

  if (row === 'hidden') return null
  if (row === 'saved' && saved) {
    return (
      <NextStepsChips
        suggestions={saved}
        onPick={onPick}
        onEdit={onEdit}
        onDismiss={() => setDismissed(true)}
      />
    )
  }
  if (row === 'offer') {
    return (
      <NextStepsOffer
        disabled={chat.isPending || all.isPending}
        onYes={() => {
          setAccepted(true)
          chat.mutate(
            {
              path: { public_id: conversationPublicId },
              body: { mode: 'always' },
            },
            {
              onSuccess: () =>
                toast.success('Next steps will be suggested in this chat'),
            }
          )
        }}
        onNo={() => setDismissed(true)}
        onAlways={() => {
          setAccepted(true)
          // A chat with its own setting would not follow the new default, so
          // this chat is moved back onto the default it was just set to.
          const chatOverride = settings.data?.chat_mode ?? null
          all.mutate(
            { body: { mode: 'always' } },
            {
              onSuccess: () => {
                if (chatOverride !== null) {
                  chat.mutate({
                    path: { public_id: conversationPublicId },
                    body: { mode: null },
                  })
                }
                toast.success('Next steps will be suggested in all your chats')
              },
            }
          )
        }}
      />
    )
  }
  if (steps.isPending) return <NextStepsSkeleton />
  if (steps.isError) {
    return (
      <div
        className="flex flex-wrap items-center gap-2 pl-1 text-xs text-muted-foreground"
        role="alert"
      >
        <span>
          {getErrorMessage(steps.error, 'Could not suggest next steps')}
        </span>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-6 gap-1 px-2 text-xs"
          onClick={() => void steps.refetch()}
        >
          <RefreshCw className="size-3" />
          Try again
        </Button>
      </div>
    )
  }
  return (
    <NextStepsChips
      suggestions={steps.data.suggestions}
      onPick={onPick}
      onEdit={onEdit}
      onDismiss={() => setDismissed(true)}
    />
  )
}

function ModeOption({
  label,
  selected,
  disabled,
  onSelect,
}: {
  label: string
  selected: boolean
  disabled: boolean
  onSelect: () => void
}) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onSelect}
      className={cn(
        'flex w-full items-center justify-between rounded-md px-2 py-1.5 text-left text-sm hover:bg-muted disabled:opacity-50',
        selected && 'font-medium'
      )}
    >
      {label}
      {selected && <Check className="size-3.5" aria-hidden="true" />}
    </button>
  )
}

/** The two settings: this chat (or follow the default) and the default. */
export function SuggestionSettingsMenu({
  settings,
  defaultMode,
  busy,
  onChatMode,
  onDefaultMode,
}: {
  settings: ChatSuggestionSettings
  defaultMode: SuggestionMode
  busy: boolean
  onChatMode: (mode: SuggestionMode | null) => void
  onDefaultMode: (mode: SuggestionMode) => void
}) {
  const chatMode = settings.chat_mode ?? null
  return (
    <div className="space-y-3" role="menu">
      <div>
        <p className="font-medium">Next-step suggestions</p>
        <p className="text-xs text-muted-foreground">
          A few follow-ups after an answer, such as “Show the deployment logs”.
          Each set is one small extra request to this chat&rsquo;s model.
        </p>
      </div>
      <div className="space-y-0.5">
        <p className="px-2 text-xs font-medium text-muted-foreground">
          This chat
        </p>
        <ModeOption
          label={`Use my default (${SUGGESTION_MODE_LABELS[defaultMode].toLowerCase()})`}
          selected={chatMode === null}
          disabled={busy}
          onSelect={() => onChatMode(null)}
        />
        {MODES.map((mode) => (
          <ModeOption
            key={mode}
            label={SUGGESTION_MODE_LABELS[mode]}
            selected={chatMode === mode}
            disabled={busy}
            onSelect={() => onChatMode(mode)}
          />
        ))}
      </div>
      <div className="space-y-0.5 border-t pt-2">
        <p className="px-2 text-xs font-medium text-muted-foreground">
          My default for all chats
        </p>
        {MODES.map((mode) => (
          <ModeOption
            key={mode}
            label={SUGGESTION_MODE_LABELS[mode]}
            selected={defaultMode === mode}
            disabled={busy}
            onSelect={() => onDefaultMode(mode)}
          />
        ))}
      </div>
    </div>
  )
}

/** Composer control for the suggestion settings. */
export function ChatSuggestionControl({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const settings = useChatSuggestionSettings(conversationPublicId)
  const defaults = useQuery(getSuggestionDefaultOptions())
  const { chat, all } = useSuggestionSettingsMutations()
  if (!settings.data?.available) return null
  const defaultMode = defaults.data?.mode ?? settings.data.default_mode
  const busy = chat.isPending || all.isPending
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className={cn(
            'h-8 w-8 rounded-full',
            settings.data.mode === 'off'
              ? 'text-muted-foreground'
              : 'text-amber-700 dark:text-amber-300'
          )}
          aria-label={`Next-step suggestions: ${suggestionModeSummary(settings.data.mode)}`}
          title={suggestionModeSummary(settings.data.mode)}
        >
          <Lightbulb className="size-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" side="top" className="w-80">
        <SuggestionSettingsMenu
          settings={settings.data}
          defaultMode={defaultMode}
          busy={busy}
          onChatMode={(mode) =>
            chat.mutate({
              path: { public_id: conversationPublicId },
              body: { mode },
            })
          }
          onDefaultMode={(mode) => all.mutate({ body: { mode } })}
        />
      </PopoverContent>
    </Popover>
  )
}
