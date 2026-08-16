import {
  type GlobalConversationResponse,
  type ProjectResponse,
  archiveConversation,
  getProjects,
  listAllConversations,
  renameConversation,
  updateProjectSettings,
} from '@/api/client'
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
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import {
  Bell,
  ChevronDown,
  ChevronLeft,
  PanelLeft,
  ExternalLink,
  FolderGit2,
  GitBranch,
  Loader2,
  Maximize2,
  MessageSquare,
  Pencil,
  Plus,
  RotateCcw,
  Search,
  Sparkles,
  Trash2,
  X,
  Zap,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { AiChatContext, useAiAssistant } from './AiAssistantContext'
import {
  conversationListNeedsRefresh,
  createProjectChat,
  resolvePageChat,
} from './chat-page-state'
import { DebugChatPanel } from './DebugChatPanel'

interface ActiveChat {
  projectId: number
  projectSlug?: string
  projectName?: string
  contextType: string
  contextId: string | number
  title?: string
  description?: string
  startPrompt?: string
  autoStart: boolean
}

const CONTEXT_META: Record<string, { label: string; Icon: typeof GitBranch }> =
  {
    deployment: { label: 'Deployment', Icon: GitBranch },
    alert: { label: 'Alert', Icon: Bell },
    project: { label: 'Project', Icon: FolderGit2 },
  }

function metaFor(contextType: string) {
  return (
    CONTEXT_META[contextType] ?? { label: contextType, Icon: MessageSquare }
  )
}

/**
 * Project favicon with a small context-type badge (deployment/alert/project)
 * overlaid in the corner. Shared by the conversation list and the open-chat
 * header so a chat looks the same in both places.
 */
function ContextAvatar({
  projectId,
  projectName,
  contextType,
  className,
  badgeClassName,
}: {
  projectId: number
  projectName?: string
  contextType: string
  className?: string
  badgeClassName?: string
}) {
  const { label, Icon } = metaFor(contextType)
  return (
    <div className="relative shrink-0">
      <Avatar className={cn('size-8 rounded-md', className)}>
        <AvatarImage
          src={`/api/projects/${projectId}/favicon`}
          alt={projectName ?? 'Project'}
        />
        <AvatarFallback className="rounded-md bg-primary/10 text-xs font-medium text-primary">
          {(projectName ?? label).slice(0, 1).toUpperCase()}
        </AvatarFallback>
      </Avatar>
      <span
        className={cn(
          'absolute -bottom-1 -right-1 flex h-4 w-4 items-center justify-center rounded-full border border-background bg-muted text-muted-foreground',
          badgeClassName
        )}
      >
        <Icon className="h-2.5 w-2.5" />
      </span>
    </div>
  )
}

/** Route to the entity a chat was started from, when we know its project. */
function sourceHref(a: {
  contextType: string
  contextId: string | number
  projectSlug?: string
}): string | null {
  if (!a.projectSlug) return null
  if (a.contextType === 'deployment') {
    return `/projects/${a.projectSlug}/deployments/${a.contextId}`
  }
  if (a.contextType === 'alert') {
    return `/projects/${a.projectSlug}/metrics/alerts/${a.contextId}/edit`
  }
  if (a.contextType === 'project') {
    // A project chat's "source" is the project itself.
    return `/projects/${a.projectSlug}`
  }
  return null
}

// Remember the last open chat across close/reopen and page reloads, so the dock
// returns to where the user was instead of the conversation list.
const ACTIVE_CHAT_KEY = 'temps.ai.activeChat'

function loadActiveChat(): ActiveChat | null {
  try {
    const raw = localStorage.getItem(ACTIVE_CHAT_KEY)
    if (!raw) return null
    const a = JSON.parse(raw) as ActiveChat
    if (
      a &&
      typeof a.projectId === 'number' &&
      a.contextType &&
      a.contextId != null
    ) {
      return { ...a, autoStart: false }
    }
  } catch {
    /* storage unavailable or malformed — fall through */
  }
  return null
}

function saveActiveChat(a: ActiveChat | null) {
  try {
    if (a) {
      // Never re-trigger the one-shot auto-diagnosis when restoring.
      localStorage.setItem(
        ACTIVE_CHAT_KEY,
        JSON.stringify({ ...a, autoStart: false })
      )
    } else {
      localStorage.removeItem(ACTIVE_CHAT_KEY)
    }
  } catch {
    /* non-fatal */
  }
}

/**
 * The persistent AI assistant dock (ADR-023). Rendered once in the app shell as
 * a flex sibling of the page content, so opening it *pushes* the layout rather
 * than covering it with a modal backdrop — the rest of the console stays fully
 * interactive and the chat keeps streaming while the user navigates. Width
 * animates to zero when closed.
 */
export function AiAssistantDock() {
  const { isOpen, initialContext, openSeq, close } = useAiAssistant()

  return (
    <aside
      aria-hidden={!isOpen}
      className={cn(
        'shrink-0 overflow-hidden bg-background transition-[width] duration-300 ease-in-out',
        isOpen
          ? 'w-full border-l sm:w-[24rem] lg:w-[28rem] xl:w-[32rem]'
          : 'w-0 border-l-0'
      )}
    >
      {/* Pinned full-height column; page content scrolls independently to the
          left, so the dock stays put ("sticky") while navigating. `dvh` tracks
          the visible viewport so the composer pins to the bottom of the screen
          on mobile instead of leaving an empty band below it. */}
      <div className="sticky top-0 h-dvh w-full">
        {isOpen && (
          <DockBody
            key={openSeq}
            initialContext={initialContext}
            onClose={close}
          />
        )}
      </div>
    </aside>
  )
}

/**
 * The assistant itself: conversation list, message thread and composer.
 *
 * Exported so it can be mounted outside the dock — `/chat` renders it
 * full-screen with `layout="page"`, which keeps the conversation list as a
 * collapsible rail beside the thread instead of stacking behind it.
 */
export function DockBody({
  initialContext,
  onClose,
  layout = 'dock',
}: {
  initialContext?: AiChatContext
  onClose: () => void
  layout?: 'dock' | 'page'
}) {
  const navigate = useNavigate()
  const {
    projectId: openedProjectId,
    currentProject,
    open: openAssistant,
  } = useAiAssistant()

  // Navigate to a chat's source. On narrow screens the dock covers the whole
  // viewport, so close it after navigating — otherwise it looks like nothing
  // happened. On wide screens the dock stays open beside the page.
  const goToSource = (href: string) => {
    navigate(href)
    if (typeof window !== 'undefined' && window.innerWidth < 1024) onClose()
  }

  // Full-screen only (`/chat`) — the dock is layered on arbitrary console
  // pages and shouldn't hijack that page's URL. `?chat=<publicId>` is set
  // whenever the open conversation resolves (see the sync effect below), so
  // reloading `/chat?chat=...` (F5) lands back on the same conversation
  // instead of the list — more explicit than the dock's own localStorage
  // fallback, and correct per-tab where a shared localStorage key isn't.
  const [searchParams, setSearchParams] = useSearchParams()
  const urlChatId = layout === 'page' ? searchParams.get('chat') : null

  const [active, setActive] = useState<ActiveChat | null>(() => {
    if (initialContext && openedProjectId != null) {
      return {
        projectId: openedProjectId,
        projectSlug: initialContext.projectSlug,
        projectName: initialContext.projectName,
        contextType: initialContext.contextType,
        contextId: initialContext.contextId,
        title: initialContext.title,
        description: initialContext.description,
        startPrompt: initialContext.startPrompt,
        // Opening the contextual dock is still a new conversation: pause at
        // the start state so the user can choose provider/model/reasoning and
        // permissions before those immutable conversation fields are pinned.
        autoStart: false,
      }
    }
    // Page layout (`/chat`) always resolves via URL: either an existing
    // `?chat=` param (resolution effect below) or, if bare, a redirect to a
    // deterministic chat (redirect effect below) — never the dock's
    // localStorage guess, which can silently diverge from what's on screen
    // across tabs/reloads.
    if (layout === 'page') return null
    // Dock: resume the last chat the user was in (across close/reopen and
    // reloads), falling back to the conversation list.
    return loadActiveChat()
  })

  // Persist the open chat so reopening the dock returns to it.
  useEffect(() => {
    saveActiveChat(active)
  }, [active])
  const [conversations, setConversations] = useState<
    GlobalConversationResponse[]
  >([])
  const [loadingList, setLoadingList] = useState(false)
  // Set once the first list fetch settles (success or failure) — distinct
  // from `loadingList` (which starts `false` before the fetch is even
  // scheduled) so the URL-resolution effect below can tell "haven't tried
  // yet" apart from "tried and finished" without a race on mount.
  const [listReady, setListReady] = useState(false)
  const [activePublicId, setActivePublicId] = useState<string | null>(null)
  const [resetKey, setResetKey] = useState(0)
  const [pendingDelete, setPendingDelete] = useState<{
    projectId: number
    publicId: string
    title: string
  } | null>(null)
  const [pendingRename, setPendingRename] = useState<{
    projectId: number
    publicId: string
  } | null>(null)
  const [renameValue, setRenameValue] = useState('')
  const [renaming, setRenaming] = useState(false)
  // When true, the body shows the project picker for starting a fresh
  // project-scoped chat (a new thread, not tied to a deployment/alert).
  const [picking, setPicking] = useState(false)
  const [railOpen, setRailOpen] = useState(true)

  // Unified list across every project.
  const loadList = useCallback(() => {
    setLoadingList(true)
    listAllConversations()
      .then(({ data }) => setConversations(data ?? []))
      .catch(() => setConversations([]))
      .finally(() => {
        setLoadingList(false)
        setListReady(true)
      })
  }, [])

  // A project chat is created lazily by DebugChatPanel when its first message
  // is sent. Until that point the list is necessarily a pre-creation snapshot.
  // Reconcile the parent as one transition: select the new conversation,
  // reveal the page rail, and fetch the row so it appears there immediately.
  // Existing conversations are already present and do not need another fetch.
  const handleConversationChange = useCallback(
    (publicId: string | null) => {
      setActivePublicId(publicId)
      if (!conversationListNeedsRefresh(publicId, conversations)) return
      setRailOpen(true)
      loadList()
    },
    [conversations, loadList]
  )

  useEffect(() => {
    if (initialContext) return
    // Defer to a microtask so the synchronous setLoadingList(true) inside
    // loadList() doesn't fire during the effect's render-commit phase (which
    // would trigger a cascading render warning).
    let cancelled = false
    queueMicrotask(() => {
      if (!cancelled) loadList()
    })
    return () => {
      cancelled = true
    }
  }, [initialContext, loadList])

  const openConversation = useCallback((c: GlobalConversationResponse) => {
    // The list already knows this id. Setting it immediately avoids an
    // intermediate render where URL synchronization can mistake a resolved
    // conversation for an unsaved draft.
    setActivePublicId(c.public_id)
    setPicking(false)
    setActive({
      projectId: c.project_id,
      projectSlug: c.project_slug ?? undefined,
      projectName: c.project_name ?? undefined,
      contextType: c.context_type,
      contextId: c.context_id,
      title: c.title ?? undefined,
      autoStart: false,
    })
  }, [])

  // Resolve the initial full-page route exactly once after the list arrives.
  // A valid `?chat=` is restored; bare or stale routes are replaced with the
  // API's first (most recently active) conversation. Keeping this separate
  // from ongoing URL synchronization prevents the mount-time race where a
  // still-null active id removed a valid URL before it could be resolved.
  const initialPageRouteResolvedRef = useRef(false)
  useEffect(() => {
    if (layout !== 'page' || initialContext) return
    if (initialPageRouteResolvedRef.current || !listReady) return

    const conversation = resolvePageChat(urlChatId, conversations)
    if (!conversation) return

    // Defer the state transition out of the effect body. The cancellation
    // guard keeps React Strict Mode's setup/cleanup replay from consuming the
    // one-shot ref without ever opening the resolved conversation.
    let cancelled = false
    queueMicrotask(() => {
      if (cancelled) return
      initialPageRouteResolvedRef.current = true
      openConversation(conversation)
      if (urlChatId !== conversation.public_id) {
        const next = new URLSearchParams(searchParams)
        next.set('chat', conversation.public_id)
        setSearchParams(next, { replace: true })
      }
    })
    return () => {
      cancelled = true
    }
  }, [
    layout,
    initialContext,
    urlChatId,
    listReady,
    conversations,
    openConversation,
    searchParams,
    setSearchParams,
  ])

  // Keep `?chat=` in sync with whatever conversation is actually open, so
  // reloading always lands back on it. `activePublicId` is the single source
  // of truth here — every path that opens/creates/leaves a conversation
  // already updates it (see `openConversation`, `onConversationChange`,
  // `backToList`).
  useEffect(() => {
    if (layout !== 'page') return
    const next = new URLSearchParams(searchParams)
    if (!activePublicId || next.get('chat') === activePublicId) return
    next.set('chat', activePublicId)
    setSearchParams(next, { replace: true })
  }, [layout, activePublicId, searchParams, setSearchParams])

  // Start a brand-new project-scoped chat (a fresh thread). The context_id is a
  // client-generated uuid so a project can have many independent chats; the
  // conversation row is created lazily on the first message (DebugChatPanel).
  const startProjectChat = (p: {
    id: number
    slug?: string | null
    name: string
  }) => {
    setPicking(false)
    setActivePublicId(null)
    const contextId =
      typeof crypto !== 'undefined' && 'randomUUID' in crypto
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random().toString(36).slice(2)}`
    setActive(createProjectChat(p, contextId))

    // The old conversation id must not remain in the address bar while a new
    // lazy-created chat is open. Once its first message creates a conversation,
    // `onConversationChange` supplies the new public id and the sync effect
    // below writes it back to the URL.
    if (layout === 'page' && searchParams.has('chat')) {
      const next = new URLSearchParams(searchParams)
      next.delete('chat')
      setSearchParams(next, { replace: true })
    }
  }

  const backToList = () => {
    setActive(null)
    setActivePublicId(null)
    setPicking(false)
    loadList()
  }

  // Reset = archive the current conversation and start a truly blank one for
  // the same source. Deliberately does NOT auto-resend the original
  // startPrompt (e.g. "Diagnose this and suggest concrete next steps") —
  // "reset" means start fresh, not replay the same message again.
  const resetConversation = async () => {
    if (active && activePublicId) {
      await archiveConversation({
        path: { project_id: active.projectId, public_id: activePublicId },
      }).catch(() => {})
    }
    setActivePublicId(null)
    setActive((a) => (a ? { ...a, autoStart: false } : a))
    setResetKey((k) => k + 1)
  }

  // Delete = archive (soft delete): the chat drops out of the list. Confirmed
  // via the dialog so it isn't triggered by accident.
  const confirmDelete = async () => {
    const d = pendingDelete
    if (!d) return
    setPendingDelete(null)
    await archiveConversation({
      path: { project_id: d.projectId, public_id: d.publicId },
    }).catch(() => {})
    toast.success('Chat deleted')
    if (active && activePublicId === d.publicId) {
      backToList()
    } else {
      setConversations((prev) => prev.filter((c) => c.public_id !== d.publicId))
    }
  }

  const startRename = (c: GlobalConversationResponse) => {
    setRenameValue(c.title ?? '')
    setPendingRename({ projectId: c.project_id, publicId: c.public_id })
  }

  const confirmRename = async () => {
    const r = pendingRename
    const title = renameValue.trim()
    if (!r || !title) return
    setRenaming(true)
    try {
      const { data } = await renameConversation({
        path: { project_id: r.projectId, public_id: r.publicId },
        body: { title },
      })
      const newTitle = data?.title ?? title
      setConversations((prev) =>
        prev.map((c) =>
          c.public_id === r.publicId ? { ...c, title: newTitle } : c
        )
      )
      toast.success('Chat renamed')
      setPendingRename(null)
    } catch {
      toast.error('Could not rename chat')
    } finally {
      setRenaming(false)
    }
  }

  const isPage = layout === 'page'

  // Dock -> `/chat`. The open conversation is handed over through the provider
  // so the full-screen view lands on the same thread rather than dumping the
  // user back on the chat list; the dock closes so only one copy is on screen.
  const goFullScreen = () => {
    if (active) {
      openAssistant({
        projectId: active.projectId,
        context: {
          contextType: active.contextType,
          contextId: active.contextId,
          title: active.title,
          projectSlug: active.projectSlug,
          projectName: active.projectName,
        },
      })
    }
    navigate(
      activePublicId
        ? `/chat?chat=${encodeURIComponent(activePublicId)}`
        : '/chat'
    )
    onClose()
  }
  const inConversation = active !== null
  const href = active ? sourceHref(active) : null
  const sourceLabel = active
    ? `${metaFor(active.contextType).label}${active.projectName ? ` · ${active.projectName}` : ''}`
    : ''

  // Full-height chat rail, only in page layout. It sits beside the whole
  // column (header included) so the list lines up with the top of the page
  // instead of starting under the conversation title.
  const rail = isPage && railOpen && (
    <aside className="flex h-full w-80 shrink-0 flex-col border-r">
      <div className="flex items-center justify-between gap-2 px-4 pb-2 pt-4">
        <h2 className="text-sm font-medium text-muted-foreground">Chats</h2>
        <span className="text-xs tabular-nums text-muted-foreground">
          {conversations.length}
        </span>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-4">
        <ConversationList
          loading={loadingList}
          conversations={conversations}
          activeId={activePublicId}
          onOpen={openConversation}
          onOpenSource={(c) => {
            const h = sourceHref({
              contextType: c.context_type,
              contextId: c.context_id,
              projectSlug: c.project_slug ?? undefined,
            })
            if (h) goToSource(h)
          }}
          onRename={startRename}
          onDelete={(c) =>
            setPendingDelete({
              projectId: c.project_id,
              publicId: c.public_id,
              title:
                c.title ?? `${metaFor(c.context_type).label} ${c.context_id}`,
            })
          }
        />
      </div>
    </aside>
  )

  return (
    <div className={cn('flex h-full', !isPage && 'flex-col gap-3 p-4')}>
      {rail}
      <div
        className={cn(
          isPage && 'flex h-full min-w-0 flex-1 flex-col gap-3 p-4',
          !isPage && 'contents'
        )}
      >
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2">
              {isPage ? (
                <button
                  type="button"
                  onClick={() => setRailOpen((v) => !v)}
                  className="-ml-1 rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  aria-label={railOpen ? 'Hide chat list' : 'Show chat list'}
                  title={railOpen ? 'Hide chat list' : 'Show chat list'}
                >
                  <PanelLeft className="h-4 w-4" />
                </button>
              ) : inConversation || picking ? (
                <button
                  type="button"
                  onClick={
                    inConversation ? backToList : () => setPicking(false)
                  }
                  className="-ml-1 rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  aria-label="Back to all conversations"
                  title="All chats"
                >
                  <ChevronLeft className="h-4 w-4" />
                </button>
              ) : (
                <Sparkles className="h-5 w-5 text-primary" />
              )}
              {inConversation && active && (
                <ContextAvatar
                  projectId={active.projectId}
                  projectName={active.projectName}
                  contextType={active.contextType}
                  className="size-7"
                />
              )}
              <h2 className="min-w-0 truncate text-lg font-semibold">
                {inConversation
                  ? (active?.title ?? 'AI chat')
                  : picking
                    ? 'New project chat'
                    : 'AI assistant'}
              </h2>
            </div>
            {inConversation ? (
              <p className="flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
                <span>{sourceLabel}</span>
                {href && (
                  <button
                    type="button"
                    onClick={() => goToSource(href)}
                    className="inline-flex items-center gap-0.5 text-primary hover:underline"
                  >
                    View source
                    <ExternalLink className="h-3 w-3" />
                  </button>
                )}
              </p>
            ) : picking ? (
              <p className="text-sm text-muted-foreground">
                Choose a project to start a general chat about it.
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">
                Resume any AI conversation across your projects, or start a new
                chat for a project.
              </p>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-0.5">
            {!inConversation &&
              !picking &&
              (currentProject ? (
                // On a project page: start a chat for it in one click, with a
                // caret to pick a different project instead.
                <div className="mr-1 flex items-center">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => startProjectChat(currentProject)}
                    className="h-8 gap-1 rounded-r-none border-r-0"
                    title={`New chat in ${currentProject.name}`}
                  >
                    <Plus className="h-4 w-4" />
                    New chat
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setPicking(true)}
                    className="h-8 rounded-l-none px-1.5"
                    title="New chat in another project"
                    aria-label="New chat in another project"
                  >
                    <ChevronDown className="h-4 w-4" />
                  </Button>
                </div>
              ) : (
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setPicking(true)}
                  className="mr-1 h-8 gap-1"
                >
                  <Plus className="h-4 w-4" />
                  New chat
                </Button>
              ))}
            {!isPage && (
              <button
                type="button"
                onClick={goFullScreen}
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                title="Open full screen"
                aria-label="Open chat full screen"
              >
                <Maximize2 className="h-4 w-4" />
              </button>
            )}
            {inConversation && active && (
              <button
                type="button"
                onClick={() =>
                  startProjectChat({
                    id: active.projectId,
                    slug: active.projectSlug,
                    name: active.projectName ?? 'Project',
                  })
                }
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                title="New chat"
                aria-label="New chat"
              >
                <Plus className="h-4 w-4" />
              </button>
            )}
            {inConversation && (
              <button
                type="button"
                onClick={resetConversation}
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                title="Reset — archive this chat and start a new blank one"
                aria-label="Reset conversation"
              >
                <RotateCcw className="h-4 w-4" />
              </button>
            )}
            {inConversation && activePublicId && (
              <button
                type="button"
                onClick={() =>
                  setPendingDelete({
                    projectId: active!.projectId,
                    publicId: activePublicId,
                    title: active!.title ?? 'this chat',
                  })
                }
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-destructive"
                title="Delete this chat"
                aria-label="Delete conversation"
              >
                <Trash2 className="h-4 w-4" />
              </button>
            )}
            <button
              type="button"
              onClick={onClose}
              className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              aria-label="Close AI assistant"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>

        <div className="min-h-0 flex-1">
          {inConversation ? (
            <DebugChatPanel
              key={`${active!.contextType}:${active!.contextId}:${resetKey}`}
              projectId={active!.projectId}
              contextType={active!.contextType}
              contextId={active!.contextId}
              startPrompt={
                active!.startPrompt ??
                'Diagnose this and suggest concrete next steps.'
              }
              autoStart={active!.autoStart}
              lazyCreate={active!.contextType === 'project'}
              emptyHint="Ask anything about this project — deployments, logs, traces, and errors."
              placeholder={
                active!.contextType === 'project'
                  ? 'Ask about this project…'
                  : 'Ask a follow-up…'
              }
              onConversationChange={handleConversationChange}
            />
          ) : picking ? (
            <ProjectPicker onSelect={startProjectChat} />
          ) : (
            <ConversationList
              loading={loadingList}
              conversations={conversations}
              onOpen={openConversation}
              onOpenSource={(c) => {
                const h = sourceHref({
                  contextType: c.context_type,
                  contextId: c.context_id,
                  projectSlug: c.project_slug ?? undefined,
                })
                if (h) goToSource(h)
              }}
              onRename={startRename}
              onDelete={(c) =>
                setPendingDelete({
                  projectId: c.project_id,
                  publicId: c.public_id,
                  title:
                    c.title ??
                    `${metaFor(c.context_type).label} ${c.context_id}`,
                })
              }
            />
          )}
        </div>

        <AlertDialog
          open={pendingDelete !== null}
          onOpenChange={(o) => !o && setPendingDelete(null)}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete this chat?</AlertDialogTitle>
              <AlertDialogDescription>
                “{pendingDelete?.title}” will be removed from your list. This
                can&apos;t be undone from here.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={confirmDelete}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Delete
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>

        <Dialog
          open={pendingRename !== null}
          onOpenChange={(o) => !o && setPendingRename(null)}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Rename chat</DialogTitle>
              <DialogDescription>
                Give this conversation a name so it&apos;s easy to find later.
              </DialogDescription>
            </DialogHeader>
            <form
              onSubmit={(e) => {
                e.preventDefault()
                void confirmRename()
              }}
            >
              <Input
                autoFocus
                value={renameValue}
                maxLength={200}
                placeholder="e.g. Prod memory tuning"
                onChange={(e) => setRenameValue(e.target.value)}
              />
              <DialogFooter className="mt-4">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setPendingRename(null)}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  disabled={!renameValue.trim() || renaming}
                >
                  {renaming && <Loader2 className="h-4 w-4 animate-spin" />}
                  Save
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      </div>
    </div>
  )
}

function ConversationList({
  loading,
  conversations,
  activeId,
  onOpen,
  onOpenSource,
  onRename,
  onDelete,
}: {
  loading: boolean
  conversations: GlobalConversationResponse[]
  /** Public id of the chat currently open, highlighted in list-detail layouts. */
  activeId?: string | null
  onOpen: (c: GlobalConversationResponse) => void
  onOpenSource: (c: GlobalConversationResponse) => void
  onRename: (c: GlobalConversationResponse) => void
  onDelete: (c: GlobalConversationResponse) => void
}) {
  if (loading) {
    return (
      <div className="space-y-2">
        {Array.from({ length: 4 }).map((_, i) => (
          <Skeleton key={i} className="h-14 w-full" />
        ))}
      </div>
    )
  }
  if (conversations.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 text-center">
        <MessageSquare className="h-7 w-7 text-muted-foreground" />
        <p className="text-sm font-medium">No AI conversations yet</p>
        <p className="max-w-xs text-sm text-muted-foreground">
          Click “New chat” to start one for a project, or open a failed
          deployment / firing alert and choose “Debug with AI”.
        </p>
      </div>
    )
  }
  return (
    <div className="h-full space-y-1 overflow-y-auto pr-1">
      {conversations.map((c) => {
        const { label } = metaFor(c.context_type)
        const hasSource = !!sourceHref({
          contextType: c.context_type,
          contextId: c.context_id,
          projectSlug: c.project_slug ?? undefined,
        })
        return (
          <div
            key={c.public_id}
            className={cn(
              'group relative flex items-center gap-2 rounded-md border border-transparent transition-colors hover:border-border hover:bg-accent',
              c.public_id === activeId && 'border-border bg-muted'
            )}
          >
            <button
              type="button"
              onClick={() => onOpen(c)}
              className="flex min-w-0 flex-1 items-center gap-3 px-2 py-2.5 text-left"
            >
              <ContextAvatar
                projectId={c.project_id}
                projectName={c.project_name ?? undefined}
                contextType={c.context_type}
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">
                  {c.title ?? `${label} ${c.context_id}`}
                </div>
                {/* One identifier + time. Showing the context label *and* the
                    project name *and* the timestamp left no room in a 320px
                    rail: the name collapsed to zero width, leaving a stray
                    "· ·" gap. The avatar badge already encodes the context
                    type, so the project name replaces the label when present. */}
                <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
                  <span className="min-w-0 flex-1 truncate">
                    {c.project_name ?? label}
                  </span>
                  <span className="shrink-0">
                    <TimeAgo date={c.last_activity_at} />
                  </span>
                </div>
              </div>
            </button>
            {/* Overlaid rather than laid out inline: as flex siblings these
                three buttons reserved ~90px of a 320px row permanently, so
                titles truncated to "Checkout latency …" even though they were
                invisible until hover. Absolute positioning gives the text the
                full row and fades the actions in over it. */}
            <div className="pointer-events-none absolute inset-y-0 right-0 flex items-center gap-0.5 rounded-r-md bg-gradient-to-l from-accent via-accent to-transparent pl-8 pr-1 opacity-0 transition-opacity focus-within:pointer-events-auto focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100">
              {hasSource && (
                <button
                  type="button"
                  onClick={() => onOpenSource(c)}
                  className="shrink-0 rounded-md p-1.5 text-muted-foreground hover:bg-background hover:text-foreground"
                  title="Go to source"
                  aria-label="Go to source"
                >
                  <ExternalLink className="h-3.5 w-3.5" />
                </button>
              )}
              <button
                type="button"
                onClick={() => onRename(c)}
                className="shrink-0 rounded-md p-1.5 text-muted-foreground hover:bg-background hover:text-foreground"
                title="Rename chat"
                aria-label="Rename chat"
              >
                <Pencil className="h-3.5 w-3.5" />
              </button>
              <button
                type="button"
                onClick={() => onDelete(c)}
                className="shrink-0 rounded-md p-1.5 text-muted-foreground hover:bg-background hover:text-destructive"
                title="Delete chat"
                aria-label="Delete chat"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
        )
      })}
    </div>
  )
}

/**
 * Project picker for starting a new project-scoped chat. Lists ALL projects,
 * searchable by name or slug. Selecting a project whose read-only AI chat is
 * already on opens a fresh thread; selecting one where it's off enables it
 * inline (one click, no trip to Settings — the read-only chat is safe) and then
 * opens the thread. Write actions remain a separate, explicit per-project opt-in.
 */
function ProjectPicker({
  onSelect,
}: {
  onSelect: (p: ProjectResponse) => void
}) {
  const [projects, setProjects] = useState<ProjectResponse[]>([])
  const [loading, setLoading] = useState(true)
  const [q, setQ] = useState('')
  // Project id currently being enabled+opened, so its row shows a spinner and
  // can't be double-clicked.
  const [enablingId, setEnablingId] = useState<number | null>(null)
  // True when more projects exist than the single page we fetched, so the
  // client-side search can't reach all of them — surfaced, never silent.
  const [truncated, setTruncated] = useState(false)

  useEffect(() => {
    let cancelled = false
    const PER_PAGE = 100
    getProjects({ query: { per_page: PER_PAGE } })
      .then(({ data }) => {
        if (cancelled) return
        const page = data?.projects ?? []
        setProjects(page)
        setTruncated((data?.total ?? page.length) > page.length)
      })
      .catch(() => setProjects([]))
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [])

  // Open an already-enabled project's chat, or enable the read-only chat inline
  // then open it. Enabling flips only `ai_debug_chat_enabled` (read-only, safe);
  // it never touches write actions. A 403 means the user lacks settings
  // permission — surfaced as a toast rather than a silent no-op.
  const pick = useCallback(
    async (p: ProjectResponse) => {
      if (p.ai_debug_chat_enabled === true) {
        onSelect(p)
        return
      }
      setEnablingId(p.id)
      try {
        const { error } = await updateProjectSettings({
          path: { project_id: p.id },
          body: { ai_debug_chat_enabled: true },
        })
        if (error) throw error
        setProjects((prev) =>
          prev.map((x) =>
            x.id === p.id ? { ...x, ai_debug_chat_enabled: true } : x
          )
        )
        toast.success(`AI chat enabled for ${p.name}`)
        onSelect({ ...p, ai_debug_chat_enabled: true })
      } catch {
        toast.error(
          "Couldn't enable AI chat — you may need project admin permission."
        )
      } finally {
        setEnablingId(null)
      }
    },
    [onSelect]
  )

  const needle = q.trim().toLowerCase()
  const filtered = needle
    ? projects.filter((p) =>
        `${p.name} ${p.slug ?? ''}`.toLowerCase().includes(needle)
      )
    : projects

  return (
    <div className="flex h-full flex-col gap-3">
      <div className="relative shrink-0">
        <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search projects…"
          className="pl-8"
        />
      </div>
      {loading ? (
        <div className="space-y-2">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-12 w-full" />
          ))}
        </div>
      ) : projects.length === 0 ? (
        <div className="flex h-full flex-col items-center justify-center gap-2 text-center">
          <FolderGit2 className="h-7 w-7 text-muted-foreground" />
          <p className="text-sm font-medium">No projects yet</p>
          <p className="max-w-xs text-sm text-muted-foreground">
            Create a project to start an AI chat for it.
          </p>
        </div>
      ) : filtered.length === 0 ? (
        <p className="px-1 text-sm text-muted-foreground">
          No projects match “{q}”.
        </p>
      ) : (
        <div className="h-full space-y-1 overflow-y-auto pr-1">
          {filtered.map((p) => (
            <button
              key={p.id}
              type="button"
              onClick={() => pick(p)}
              disabled={enablingId === p.id}
              title={
                p.ai_debug_chat_enabled === true
                  ? undefined
                  : 'AI chat is off for this project — click to enable it and start'
              }
              className="group flex w-full items-center gap-3 rounded-md border border-transparent px-2 py-2.5 text-left transition-colors hover:border-border hover:bg-accent disabled:opacity-60"
            >
              <Avatar className="size-8 rounded-md">
                <AvatarImage
                  src={`/api/projects/${p.id}/favicon`}
                  alt={p.name}
                />
                <AvatarFallback className="rounded-md bg-primary/10 text-xs font-medium text-primary">
                  {p.name.slice(0, 1).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">{p.name}</div>
                {p.slug && (
                  <div className="truncate text-xs text-muted-foreground">
                    {p.slug}
                  </div>
                )}
              </div>
              {enablingId === p.id ? (
                <Loader2 className="h-4 w-4 shrink-0 animate-spin text-muted-foreground" />
              ) : p.ai_debug_chat_enabled === true ? (
                <Plus className="h-4 w-4 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
              ) : (
                <span className="inline-flex shrink-0 items-center gap-1 rounded-full border border-amber-500/30 bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400">
                  <Zap className="h-3 w-3" />
                  Enable
                </span>
              )}
            </button>
          ))}
          {truncated && (
            <p className="px-2 pt-1 text-xs text-muted-foreground">
              Showing the 100 most recent projects. If you don&apos;t see yours,
              open it and start the chat from there.
            </p>
          )}
        </div>
      )}
    </div>
  )
}
