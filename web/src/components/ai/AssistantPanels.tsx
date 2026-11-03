// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createAssistantConversation,
  createProviderKey,
  setChatWorkspace,
  uploadUserConversationAttachment,
  type ConversationResponse,
} from '@/api/client'
import {
  getAiProviderStatusOptions,
  getChatSandboxOptions,
  getAiProviderStatusQueryKey,
  listProviderKeysQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowRight,
  Boxes,
  Cloud,
  KeyRound,
  ArrowUp,
  Loader2,
  Paperclip,
  Rocket,
  Sparkles,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useDefaultLayout, usePanelRef } from 'react-resizable-panels'
import { useForm } from 'react-hook-form'
import { Link, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { FOCUS_COMPOSER_HINT } from './assistant-view'
import { WorkspaceChip } from './WorkspaceChip'
import { InAssistantHeader } from './InAssistantHeader'
import { ChatProjectsLink } from './ChatProjectsLink'
import { useChatBlueprint } from './chat-blueprint'
import { projectRequest, type ProjectChoice } from './chat-project'
import { NewChatProjectChip } from './NewChatProjectChip'
import { useFocusComposerShortcut } from './useFocusComposerShortcut'
import {
  ASSISTANT_CONTEXT_TYPE,
  ASSISTANT_EXAMPLE_PROMPTS,
  assistantReadiness,
  setupPathFromProblem,
} from './assistant-state'
import { AssistantModelChip, AssistantModelSetup } from './AssistantModelPicker'
import { gatewayKeyProviderId } from './assistant-keys'
import { enterSendsMessage } from './chat-page-state'
import {
  useAssistantImageSupport,
  useNewChatModel,
} from './assistant-model-hooks'
import {
  ASSISTANT_ATTACHMENT_LIMITS,
  assistantAttachTitle,
  assistantImageError,
  attachHint,
  attachmentSelectionError,
  type OpeningAttachment,
} from './attachment-previews'
import { DebugChatPanel } from './DebugChatPanel'
import {
  ARCHITECTURE_TAB,
  DeploymentSidebar,
  PREVIEW_TAB,
} from './DeploymentSidebar'
import {
  liveSearchParams,
  PANEL_PARAM,
  TAB_PARAM,
  withParam,
} from './assistant-url'
import { resourceKeys, useChatResources } from './chat-resources'
import { ProjectReferencesProvider } from './ProjectReference'
import type { ProjectRef } from './project-refs'
import { ChatSandbox } from './ChatSandbox'
import { ChatOperationsIndicator, useChatGoals } from './GoalPanel'
import { presentGoal, watchedDeployments } from './goal-presentation'
import { useIsDesktop, useIsMobile } from '@/components/hooks/use-mobile'
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from '@/components/ui/resizable'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'

const keySchema = z.object({
  api_key: z.string().trim().min(1, 'Paste your OpenRouter API key'),
})
type KeyForm = z.infer<typeof keySchema>

/**
 * What the assistant does and exactly what is missing, with the fix on the
 * same screen. Shown in place of the chat whenever no AI provider exists, so
 * the feature is discoverable instead of absent.
 *
 * Pasting an OpenRouter key here is the whole setup: it is verified and stored
 * encrypted by the same endpoint the AI Gateway page uses, and never shown
 * again.
 */
export function AssistantOnboarding({
  reason,
  setupPath,
  onConfigured,
  onNavigate,
  cloudPath,
  roomy,
}: {
  reason: string
  setupPath: string
  /** Full-screen sizing: larger type and controls, content centred. */
  roomy?: boolean
  /** Called once the key is saved and the assistant can start. */
  onConfigured?: () => void
  /** Called when a link is followed, e.g. to close the dock on small screens. */
  onNavigate?: () => void
  /** Secondary route for operators who use managed AI instead. */
  cloudPath?: string
}) {
  const queryClient = useQueryClient()
  // Set once the key is stored: the second step picks the model. The provider
  // status is only refreshed when that step ends, because a ready status swaps
  // this onboarding out for the chat before the model has been chosen.
  const [savedKeyId, setSavedKeyId] = useState<number | null>(null)
  const form = useForm<KeyForm>({
    resolver: zodResolver(keySchema),
    defaultValues: { api_key: '' },
  })
  const save = useMutation({
    mutationFn: async (values: KeyForm) =>
      createProviderKey({
        body: {
          provider: 'openrouter',
          display_name: 'OpenRouter',
          api_key: values.api_key,
        },
        throwOnError: true,
      }),
    onSuccess: async (result) => {
      form.reset()
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: listProviderKeysQueryKey() }),
        queryClient.invalidateQueries({ queryKey: ['providerKeys'] }),
      ])
      toast.success('OpenRouter key saved')
      setSavedKeyId(result.data?.id ?? null)
      if (result.data?.id == null) await finishSetup()
    },
  })

  const finishSetup = async () => {
    await queryClient.invalidateQueries({
      queryKey: getAiProviderStatusQueryKey(),
    })
    setSavedKeyId(null)
    onConfigured?.()
  }

  return (
    <div
      className={cn(
        'flex h-full min-h-0 flex-col gap-5 overflow-y-auto p-5',
        roomy && 'justify-center gap-8 p-6'
      )}
    >
      <div className={cn('space-y-2', roomy && 'space-y-4')}>
        <Badge
          variant="outline"
          className={cn(
            'gap-1.5 whitespace-normal font-normal',
            roomy && 'px-3 py-1 text-sm'
          )}
        >
          <KeyRound
            className={cn('size-3 shrink-0', roomy && 'size-4')}
            aria-hidden="true"
          />
          {reason}
        </Badge>
        <h2
          className={cn(
            'text-xl font-semibold tracking-tight',
            roomy && 'text-4xl'
          )}
        >
          Get things running on Temps by chat
        </h2>
        <p
          className={cn(
            'text-sm leading-6 text-muted-foreground',
            roomy && 'text-lg leading-8'
          )}
        >
          Say what you want: deploy a repository from a Git URL, back up your
          databases to S3, add a database to a project. The assistant works out
          the steps and uses the same API as this console, with your own
          permissions.
        </p>
      </div>

      {savedKeyId !== null ? (
        <AssistantModelSetup
          providerKeyId={savedKeyId}
          onDone={() => void finishSetup()}
          roomy={roomy}
        />
      ) : (
        <>
          <AssistantExample roomy={roomy} />

          <form
            noValidate
            onSubmit={form.handleSubmit((values) => save.mutate(values))}
            className={cn('space-y-2', roomy && 'space-y-3')}
          >
            <label
              htmlFor="assistant-openrouter-key"
              className={cn('text-sm font-medium', roomy && 'text-base')}
            >
              OpenRouter API key
            </label>
            <div className="flex gap-2">
              <Input
                className={cn(roomy && 'h-12 text-base md:text-base')}
                id="assistant-openrouter-key"
                type="password"
                autoComplete="off"
                spellCheck={false}
                placeholder="sk-or-…"
                disabled={save.isPending}
                aria-invalid={form.formState.errors.api_key ? true : undefined}
                {...form.register('api_key')}
              />
              <Button
                type="submit"
                disabled={save.isPending}
                className={cn(roomy && 'h-12 px-6 text-base')}
              >
                {save.isPending ? (
                  <Loader2 className="size-4 animate-spin" aria-hidden="true" />
                ) : null}
                Save key
              </Button>
            </div>
            {form.formState.errors.api_key ? (
              <p className="text-sm text-destructive" role="alert">
                {form.formState.errors.api_key.message}
              </p>
            ) : null}
            {save.isError ? (
              <p className="text-sm text-destructive" role="alert">
                {getErrorMessage(
                  save.error,
                  'OpenRouter rejected the key. Check it and try again.'
                )}
              </p>
            ) : null}
            <p
              className={cn(
                'text-xs leading-5 text-muted-foreground',
                roomy && 'text-sm leading-6'
              )}
            >
              The key is checked with OpenRouter, then stored encrypted. It is
              never shown again.{' '}
              <a
                className="underline underline-offset-2"
                href="https://openrouter.ai/keys"
                target="_blank"
                rel="noreferrer"
              >
                Get a key
              </a>
            </p>
          </form>

          <div className={cn('grid gap-2', roomy && 'gap-3')}>
            <Button
              asChild
              variant="outline"
              className={cn(
                'w-full justify-between',
                roomy && 'h-12 text-base'
              )}
            >
              <Link to={setupPath} onClick={onNavigate}>
                Open AI provider settings <ArrowRight className="size-4" />
              </Link>
            </Button>
            {cloudPath ? (
              <Button
                asChild
                variant="ghost"
                className={cn(
                  'w-full justify-between text-muted-foreground',
                  roomy && 'h-12 text-base'
                )}
              >
                <Link to={cloudPath} onClick={onNavigate}>
                  <span className="flex items-center gap-2">
                    <Cloud className="size-4" aria-hidden="true" />
                    Use Temps Cloud managed AI instead
                  </span>
                  <ArrowRight className="size-4" />
                </Link>
              </Button>
            ) : null}
          </div>
        </>
      )}
    </div>
  )
}

/** A concrete exchange, so "ask the assistant" is not an abstract promise. */
function AssistantExample({ roomy }: { roomy?: boolean }) {
  return (
    <div
      className={cn(
        'space-y-2 rounded-lg border border-border bg-muted/30 p-3 text-sm',
        roomy && 'space-y-3 p-5 text-base'
      )}
    >
      <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Example
      </p>
      <p
        className={cn(
          'rounded-md bg-background px-3 py-2',
          roomy && 'px-4 py-3'
        )}
      >
        {ASSISTANT_EXAMPLE_PROMPTS[0]}
      </p>
      <p
        className={cn(
          'px-1 text-xs leading-5 text-muted-foreground',
          roomy && 'text-sm leading-6'
        )}
      >
        Reads and changes run straight away unless you turn on “Ask before
        changes”. Deletions always wait for your approval.
      </p>
    </div>
  )
}

class AssistantStartError extends Error {
  constructor(
    readonly status: number | undefined,
    readonly problem: unknown,
    message: string
  ) {
    super(message)
  }
}

/**
 * Entry point for a new assistant chat: a prompt box and example questions.
 * The conversation is created when the first question is sent, so opening the
 * assistant and walking away leaves nothing behind.
 */
export function AssistantStart({
  onStarted,
  onNavigate,
  cloudPath,
  roomy,
  workspace,
  onLeaveWorkspace,
}: {
  onStarted: (
    conversation: ConversationResponse,
    firstMessage: string,
    attachments: OpeningAttachment[]
  ) => void
  onNavigate?: () => void
  cloudPath?: string
  /** Full-screen sizing: larger type and controls, content centred. */
  roomy?: boolean
  /** A workspace the new chat joins before its first message, sharing its
   *  blueprint and sandbox with the workspace's other chats. */
  workspace?: NewChatWorkspaceChoice | null
  /** Starts the chat on its own instead of in `workspace`. */
  onLeaveWorkspace?: () => void
}) {
  const isMobile = useIsMobile()
  const status = useQuery({
    ...getAiProviderStatusOptions(),
    staleTime: 15_000,
    retry: false,
  })
  const [blocked, setBlocked] = useState<{
    reason: string
    setupPath: string
  } | null>(null)
  const [draft, setDraft] = useState('')
  const [files, setFiles] = useState<File[]>([])
  // Set once the chat exists, so a failed upload retries into the same chat
  // instead of leaving an empty one behind each time.
  const [created, setCreated] = useState<ConversationResponse | null>(null)
  const uploaded = useRef(new Map<File, OpeningAttachment>())
  const inputRef = useRef<HTMLTextAreaElement>(null)
  useFocusComposerShortcut(inputRef)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const newChatModel = useNewChatModel()
  const imageSupport = useAssistantImageSupport(
    true,
    newChatModel.model,
    newChatModel.keyId
  )
  const [project, setProject] = useState<ProjectChoice | null>(null)
  // The workspace the created chat already joined, so a retry after a failed
  // upload does not join it twice.
  const joined = useRef<string | null>(null)

  const start = useMutation({
    mutationFn: async (firstMessage: string) => {
      let conversation = created
      if (!conversation) {
        const { data, error, response } = await createAssistantConversation({
          body: {
            ...(newChatModel.keyId !== null
              ? { ai_provider: gatewayKeyProviderId(newChatModel.keyId) }
              : {}),
            ...(newChatModel.model ? { ai_model: newChatModel.model } : {}),
            ...projectRequest(project),
          },
        })
        if (!data) {
          throw new AssistantStartError(
            response?.status,
            error,
            getErrorMessage(error, 'Could not start the assistant.')
          )
        }
        conversation = data
        setCreated(data)
      }
      const target = conversation
      if (workspace && joined.current !== workspace.public_id) {
        const { data, error, response } = await setChatWorkspace({
          path: { public_id: target.public_id },
          body: {
            application_id: workspace.public_id,
            keep_sandbox: 'workspace',
          },
        })
        if (!data) {
          throw new AssistantStartError(
            response?.status,
            error,
            getErrorMessage(
              error,
              `The chat was created but could not join ${workspace.name}.`
            )
          )
        }
        joined.current = workspace.public_id
      }
      await Promise.all(
        files
          .filter((file) => !uploaded.current.has(file))
          .map(async (file) => {
            const { data, error, response } =
              await uploadUserConversationAttachment({
                path: { public_id: target.public_id },
                body: { file },
              })
            if (!data) {
              throw new AssistantStartError(
                response?.status,
                error,
                getErrorMessage(error, `Could not upload ${file.name}.`)
              )
            }
            uploaded.current.set(file, { payload: data, file })
          })
      )
      return {
        conversation: target,
        firstMessage,
        attachments: files.flatMap((file) => {
          const done = uploaded.current.get(file)
          return done ? [done] : []
        }),
      }
    },
    onSuccess: ({ conversation, firstMessage, attachments }) =>
      onStarted(conversation, firstMessage, attachments),
    onError: (error) => {
      // 409 is "no provider configured", and carries where to fix it. Show the
      // onboarding in place rather than a toast the user cannot act on.
      if (error instanceof AssistantStartError && error.status === 409) {
        const readiness = assistantReadiness(status.data)
        setBlocked({
          reason:
            readiness.state === 'unconfigured'
              ? readiness.reason
              : 'No AI provider is configured.',
          setupPath:
            setupPathFromProblem(error.problem) ??
            (readiness.state === 'unconfigured'
              ? readiness.setupPath
              : '/ai-gateway?add=openrouter'),
        })
        return
      }
      toast.error(
        created || files.length > 0
          ? 'Could not send your files'
          : 'Could not start the assistant',
        { description: error.message }
      )
    },
  })

  const addFiles = (incoming: FileList | null) => {
    if (!incoming || incoming.length === 0) return
    const selected = Array.from(incoming)
    const problem =
      attachmentSelectionError(
        selected,
        files.length,
        ASSISTANT_ATTACHMENT_LIMITS
      ) ?? assistantImageError(selected, imageSupport)
    if (fileInputRef.current) fileInputRef.current.value = ''
    if (problem) {
      toast.error('Cannot attach these files', { description: problem })
      return
    }
    setFiles((current) => [...current, ...selected])
  }

  if (status.isPending) return <AssistantStartSkeleton roomy={roomy} />

  const readiness = assistantReadiness(status.data)
  if (blocked || readiness.state === 'unconfigured') {
    const view =
      blocked ?? (readiness.state === 'unconfigured' ? readiness : null)
    return (
      <AssistantOnboarding
        reason={view?.reason ?? ''}
        setupPath={view?.setupPath ?? '/ai-gateway?add=openrouter'}
        cloudPath={cloudPath}
        roomy={roomy}
        onNavigate={onNavigate}
        onConfigured={() => setBlocked(null)}
      />
    )
  }

  const submit = (text: string) => {
    const message = text.trim()
    if (!message || start.isPending) return
    start.mutate(message)
  }

  return (
    <div
      className={cn(
        // `safe` alignment: when the content is taller than the viewport (a
        // phone) it starts at the top and scrolls instead of being clipped.
        'flex h-full min-h-0 flex-col gap-4 overflow-y-auto p-5 [justify-content:safe_flex-end]',
        roomy && 'gap-8 p-4 [justify-content:safe_center] sm:p-6'
      )}
    >
      <div className={cn('space-y-2', roomy && 'space-y-4')}>
        <h2
          className={cn(
            'flex items-center gap-2 text-xl font-semibold tracking-tight',
            roomy && 'gap-3 text-2xl sm:text-4xl'
          )}
        >
          <Sparkles
            className={cn('size-5 text-primary', roomy && 'size-6 sm:size-8')}
            aria-hidden="true"
          />
          What should we get running?
        </h2>
        <p
          className={cn(
            'text-sm leading-6 text-muted-foreground',
            roomy && 'text-base leading-7 sm:text-lg sm:leading-8'
          )}
        >
          Describe the outcome and Temps works out the steps, using the same API
          as this console with your own permissions. Deletions always wait for
          your approval.
        </p>
      </div>
      <form
        className={cn(
          '@container rounded-2xl border border-border bg-background shadow-sm transition-shadow focus-within:shadow-md',
          roomy && 'rounded-3xl'
        )}
        onSubmit={(event) => {
          event.preventDefault()
          submit(draft)
        }}
      >
        <Textarea
          ref={inputRef}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            // Enter sends on a computer and breaks the line on a phone.
            if (
              enterSendsMessage(
                { ...event, isComposing: event.nativeEvent.isComposing },
                isMobile
              )
            ) {
              event.preventDefault()
              submit(draft)
            }
          }}
          placeholder={`Describe what you want to set up or change…${isMobile ? '' : FOCUS_COMPOSER_HINT}`}
          aria-label="Message the Temps assistant"
          disabled={start.isPending}
          maxLength={8000}
          rows={3}
          className={cn(
            'min-h-[84px] resize-none border-0 bg-transparent px-4 pb-1 pt-4 text-sm shadow-none focus-visible:border-transparent focus-visible:outline-none focus-visible:ring-0',
            roomy && 'min-h-[120px] px-5 pt-5 text-base md:text-base'
          )}
        />
        {files.length > 0 ? (
          <ul
            className="flex flex-wrap gap-2 px-3 pb-2"
            aria-label="Attached files"
          >
            {files.map((file, index) => (
              <li
                key={`${file.name}:${file.size}:${index}`}
                className="flex max-w-full items-center gap-1 rounded-full border border-border bg-muted px-2.5 py-1 text-xs"
              >
                <Paperclip className="size-3 shrink-0" aria-hidden="true" />
                <span className="truncate">{file.name}</span>
                <button
                  type="button"
                  className="rounded-full p-0.5 hover:bg-background disabled:opacity-50"
                  aria-label={`Remove ${file.name}`}
                  disabled={start.isPending}
                  onClick={() =>
                    setFiles((current) =>
                      current.filter((_, at) => at !== index)
                    )
                  }
                >
                  <X className="size-3" aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        ) : null}
        <div className="flex items-center justify-between gap-2 px-3 pb-3">
          <div className="flex min-w-0 items-center gap-1">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="sr-only"
              aria-label="Attach files"
              tabIndex={-1}
              disabled={start.isPending}
              onChange={(event) => addFiles(event.target.files)}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className={cn('size-8 shrink-0', roomy && 'size-10')}
              disabled={start.isPending}
              aria-label="Attach files"
              title={assistantAttachTitle(
                imageSupport,
                ASSISTANT_ATTACHMENT_LIMITS,
                true
              )}
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip className="size-4" aria-hidden="true" />
            </Button>
            <AssistantModelChip
              value={newChatModel.model}
              onChange={newChatModel.choose}
              providerKeyId={newChatModel.keyId}
              chooseKey
              disabled={start.isPending || created !== null}
              roomy={roomy}
            />
            <NewChatProjectChip
              value={project}
              onChange={setProject}
              disabled={start.isPending || created !== null}
              roomy={roomy}
            />
            {workspace && (
              <NewChatWorkspaceChip
                workspace={workspace}
                onLeave={onLeaveWorkspace}
                disabled={start.isPending || created !== null}
                roomy={roomy}
              />
            )}
          </div>
          <Button
            type="submit"
            size="icon"
            className={cn('size-8 shrink-0 rounded-full', roomy && 'size-10')}
            disabled={!draft.trim() || start.isPending}
            aria-label="Send message"
          >
            {start.isPending ? (
              <Loader2 className="size-4 animate-spin" aria-hidden="true" />
            ) : (
              <ArrowUp className="size-4" aria-hidden="true" />
            )}
          </Button>
        </div>
      </form>
      <p className="-mt-2 text-xs text-muted-foreground">
        {attachHint(true, imageSupport, ASSISTANT_ATTACHMENT_LIMITS)}
      </p>
      {/* Suggestions fill the box rather than send: most goals need the user to
          add a detail (the repository URL, the bucket) before they make sense. */}
      <div
        className={cn('flex flex-wrap gap-2', roomy && 'gap-3')}
        role="group"
        aria-label="Suggestions"
      >
        {ASSISTANT_EXAMPLE_PROMPTS.map((prompt) => (
          <button
            key={prompt}
            type="button"
            disabled={start.isPending}
            onClick={() => {
              setDraft(prompt)
              inputRef.current?.focus()
            }}
            className={cn(
              'rounded-full border border-border px-3 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50',
              roomy && 'px-4 py-2'
            )}
          >
            {prompt}
          </button>
        ))}
      </div>
    </div>
  )
}

function AssistantStartSkeleton({ roomy }: { roomy?: boolean }) {
  return (
    <div
      className={cn('mt-auto space-y-3 p-5', roomy && 'my-auto space-y-5 p-6')}
      role="status"
      aria-label="Loading"
    >
      <Skeleton className="h-6 w-48" />
      <Skeleton className="h-4 w-full" />
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
    </div>
  )
}

export type NewChatWorkspaceChoice = { public_id: string; name: string }

/** Says which workspace a chat being started will join, with a way out. */
function NewChatWorkspaceChip({
  workspace,
  onLeave,
  disabled,
  roomy,
}: {
  workspace: NewChatWorkspaceChoice
  onLeave?: () => void
  disabled: boolean
  roomy?: boolean
}) {
  return (
    <span
      className={cn(
        'inline-flex h-8 min-w-0 max-w-40 items-center gap-1.5 rounded-md border px-2 text-muted-foreground md:max-w-56',
        roomy ? 'text-sm' : 'text-xs'
      )}
      title={`This chat joins the application ${workspace.name} and shares its blueprint and sandbox`}
    >
      <Boxes className="size-3.5 shrink-0" aria-hidden />
      <span className="truncate">In {workspace.name}</span>
      {onLeave && (
        <button
          type="button"
          onClick={onLeave}
          disabled={disabled}
          className="shrink-0 rounded-sm hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
          aria-label={`Start this chat outside ${workspace.name}`}
          title="Start it on its own instead"
        >
          <X className="size-3.5" />
        </button>
      )}
    </span>
  )
}

/**
 * An assistant conversation: the shared chat panel on the chat's model, named
 * above it. Whether changes ask first is a switch inside the composer.
 */
export function AssistantThread({
  conversation,
  firstMessage,
  firstAttachments,
  readOnly,
  syncUrl = false,
}: {
  conversation: Pick<ConversationResponse, 'public_id' | 'context_id'> & {
    /** What kind of chat this is; a new chat is an assistant chat. */
    context_type?: string
  }
  firstMessage?: string
  /** Files already uploaded to this chat, sent with `firstMessage`. */
  firstAttachments?: OpeningAttachment[]
  readOnly?: boolean
  /**
   * Keep the working tabs (open or not, and which one) in the address, so a
   * link or reload shows the same screen. Only the full-screen page does:
   * the dock sits on arbitrary console pages and must not rewrite their URL.
   */
  syncUrl?: boolean
}) {
  const goals = useChatGoals(conversation.public_id)
  const deployments = useMemo(
    () => watchedDeployments(goals.data ?? []),
    [goals.data]
  )
  const isDesktop = useIsDesktop()
  const [searchParams, setSearchParams] = useSearchParams()
  // Callbacks below run long after the render that made them; each write
  // starts from the live address (see `liveSearchParams`), and the setter is
  // kept in a ref so the callbacks stay stable.
  const setSearchParamsRef = useRef(setSearchParams)
  useEffect(() => {
    setSearchParamsRef.current = setSearchParams
  })
  const [localPanelOpen, setLocalPanelOpen] = useState(false)
  const panelOpen = syncUrl
    ? searchParams.get(PANEL_PARAM) === 'open'
    : localPanelOpen
  const urlTab = syncUrl ? searchParams.get(TAB_PARAM) : null
  const setPanelOpen = useCallback(
    (open: boolean) => {
      if (!syncUrl) {
        setLocalPanelOpen(open)
        return
      }
      const current = liveSearchParams()
      const write = setSearchParamsRef.current
      // Closing the panel forgets which tab it was on.
      const next = open
        ? withParam(current, PANEL_PARAM, 'open')
        : withParam(withParam(current, PANEL_PARAM, null), TAB_PARAM, null)
      write(next, { replace: true })
    },
    [syncUrl]
  )
  const setUrlTab = useCallback(
    (tab: string) => {
      if (!syncUrl) return
      const current = liveSearchParams()
      const write = setSearchParamsRef.current
      if (current.get(TAB_PARAM) === tab) return
      write(withParam(current, TAB_PARAM, tab), { replace: true })
    },
    [syncUrl]
  )
  // Projects the user opened from a reply, newest first.
  const [openedProjects, setOpenedProjects] = useState<ProjectRef[]>([])
  const [focus, setFocus] = useState({ key: '', nonce: 0 })
  // The panel always has its "Made here" tab, so it can always be opened: a
  // chat that has made nothing yet says what will appear there.
  const resources = useChatResources(conversation.public_id)
  const openProject = useCallback(
    (project: ProjectRef) => {
      setOpenedProjects((current) => [
        project,
        ...current.filter((p) => p.id !== project.id),
      ])
      setFocus((current) => ({
        key: `project:${project.id}`,
        nonce: current.nonce + 1,
      }))
      setPanelOpen(true)
    },
    [setPanelOpen]
  )
  // A piece the chat makes while the user is watching opens the panel, once;
  // what an old chat already made waits for the button instead.
  const seenPieces = useRef<Set<string> | null>(null)
  useEffect(() => {
    if (!resources.data) return
    const keys = resourceKeys(resources.data.resources)
    const seen = seenPieces.current
    seenPieces.current = new Set(keys)
    if (seen && keys.some((piece) => !seen.has(piece))) setPanelOpen(true)
  }, [resources.data, setPanelOpen])
  // The assistant confirming a page is being served shows it in the Preview
  // tab; the port a chat already had when it was opened stays in the
  // background.
  const sandbox = useQuery(
    getChatSandboxOptions({ path: { public_id: conversation.public_id } })
  )
  const previewPort = sandbox.data?.preview?.port ?? null
  const seenPort = useRef<number | null | undefined>(undefined)
  useEffect(() => {
    if (!sandbox.data) return
    const before = seenPort.current
    seenPort.current = previewPort
    if (before === undefined || previewPort === null || before === previewPort)
      return
    setFocus((current) => ({ key: PREVIEW_TAB, nonce: current.nonce + 1 }))
    setPanelOpen(true)
  }, [sandbox.data, previewPort, setPanelOpen])
  // An architecture proposal waiting for a decision opens the Architecture
  // tab, once per version, so what the assistant waits for is in view.
  const { blueprint } = useChatBlueprint(conversation.public_id)
  const proposalVersion = blueprint.data
    ? (blueprint.data.proposal?.version ?? null)
    : undefined
  const seenProposal = useRef<number | null | undefined>(undefined)
  useEffect(() => {
    if (proposalVersion === undefined) return
    const before = seenProposal.current
    seenProposal.current = proposalVersion
    if (proposalVersion === null || before === proposalVersion) return
    setFocus((current) => ({
      key: ARCHITECTURE_TAB,
      nonce: current.nonce + 1,
    }))
    setPanelOpen(true)
  }, [proposalVersion, setPanelOpen])
  const panelRef = usePanelRef()
  const { defaultLayout, onLayoutChanged } = useDefaultLayout({
    id: 'assistant-deployment-split',
    onlySaveAfterUserInteractions: true,
  })

  // A deployment that is still being built opens the panel by itself, once per
  // deployment; a finished one in an old chat waits for the button instead.
  const newest = deployments[0]
  const newestKey = newest
    ? `${newest.projectId}:${newest.deploymentId ?? 'latest'}`
    : ''
  const announced = useRef('')
  const newestOpen = newest ? presentGoal(newest.goal).open : false
  useEffect(() => {
    if (!newestKey || announced.current === newestKey) return
    // Opening the panel can write the address, so it runs after the effect
    // body rather than inside it. The ref is only consumed once it does, so
    // Strict Mode's setup/cleanup replay cannot swallow the announcement.
    let cancelled = false
    queueMicrotask(() => {
      if (cancelled) return
      announced.current = newestKey
      if (newestOpen) setPanelOpen(true)
    })
    return () => {
      cancelled = true
    }
  }, [newestKey, newestOpen, setPanelOpen])

  // Wide screens dock the panel beside the chat (resizable); narrower ones
  // show the same content in a drawer, so the chat is never squeezed.
  const docked = isDesktop && panelOpen
  const wasShown = useRef(false)
  useEffect(() => {
    const panel = panelRef.current
    if (!panel) return
    if (!docked) wasShown.current = false
    if (docked && panel.isCollapsed()) panel.expand()
    if (!docked && !panel.isCollapsed()) panel.collapse()
  }, [docked, panelRef])

  const sidebar = (
    <DeploymentSidebar
      deployments={deployments}
      projects={openedProjects}
      focus={focus}
      conversationPublicId={conversation.public_id}
      initialSelected={urlTab ?? undefined}
      onSelectedChange={setUrlTab}
      onOpenProject={openProject}
      onClose={() => setPanelOpen(false)}
    />
  )

  return (
    <ProjectReferencesProvider onOpen={openProject}>
      <ResizablePanelGroup
        orientation="horizontal"
        className="h-full"
        defaultLayout={defaultLayout}
        onLayoutChanged={onLayoutChanged}
      >
        <ResizablePanel id="thread" minSize={isDesktop ? 360 : 0}>
          <div
            className={cn(
              'flex h-full min-h-0 min-w-0 flex-col',
              // Alone, the chat reads best as a centred column; beside its tabs it fills its half.
              !docked && 'mx-auto w-full max-w-3xl'
            )}
          >
            <InAssistantHeader>
              <ChatOperationsIndicator
                conversationPublicId={conversation.public_id}
              />
              <ChatProjectsLink conversationPublicId={conversation.public_id} />
              <WorkspaceChip conversationPublicId={conversation.public_id} />
              <ChatSandbox
                conversationPublicId={conversation.public_id}
                iconOnly
              />
              <Button
                type="button"
                variant={panelOpen ? 'secondary' : 'ghost'}
                size="icon"
                className="size-8"
                aria-pressed={panelOpen}
                aria-label="Working tabs"
                onClick={() => setPanelOpen(!panelOpen)}
                title={
                  panelOpen
                    ? 'Hide the working tabs'
                    : 'Show what this chat made and the deployments it is working on'
                }
              >
                <Rocket className="size-4" />
              </Button>
            </InAssistantHeader>
            <div className="min-h-0 flex-1">
              <DebugChatPanel
                key={conversation.public_id}
                conversationPublicId={conversation.public_id}
                userScoped
                contextType={
                  conversation.context_type ?? ASSISTANT_CONTEXT_TYPE
                }
                contextId={conversation.context_id}
                detailLevel="simple"
                initialMessage={firstMessage}
                initialAttachments={firstAttachments}
                emptyHint="Ask about your projects, deployments or settings."
                placeholder="Ask Temps to inspect or change something…"
                readOnly={readOnly}
              />
            </div>
          </div>
        </ResizablePanel>
        <ResizableHandle
          withHandle
          aria-label="Resize deployment panel"
          className={docked ? 'mx-2' : 'hidden'}
        />
        <ResizablePanel
          id="deployment"
          panelRef={panelRef}
          defaultSize={0}
          minSize={300}
          maxSize="50"
          collapsible
          collapsedSize={0}
          disabled={!docked}
          onResize={(size) => {
            // A page opened on `panel=open` starts the panel at zero for a
            // moment before the effect above expands it; that is not the user
            // dragging it shut.
            if (size.asPercentage > 0) wasShown.current = true
            if (size.asPercentage !== 0 || !docked || !wasShown.current) return
            // Dragging the tabs shut closes them, like the button does.
            setPanelOpen(false)
          }}
          className="min-w-0"
        >
          {docked && sidebar}
        </ResizablePanel>
      </ResizablePanelGroup>
      <Sheet open={!isDesktop && panelOpen} onOpenChange={setPanelOpen}>
        <SheetContent side="right" className="w-full p-0 sm:max-w-md">
          <SheetHeader className="sr-only">
            <SheetTitle>Working tabs</SheetTitle>
            <SheetDescription>
              What this chat made, and the deployments and projects it is
              working on.
            </SheetDescription>
          </SheetHeader>
          <DeploymentSidebar
            deployments={deployments}
            projects={openedProjects}
            focus={focus}
            conversationPublicId={conversation.public_id}
            initialSelected={urlTab ?? undefined}
            onSelectedChange={setUrlTab}
            onOpenProject={openProject}
            inDrawer
          />
        </SheetContent>
      </Sheet>
    </ProjectReferencesProvider>
  )
}
