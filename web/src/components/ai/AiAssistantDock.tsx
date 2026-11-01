// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  type ConversationDetailResponse,
  type ConversationResponse,
  type GlobalConversationResponse,
  type ProjectResponse,
  archiveConversation,
  archiveUserConversation,
  getProjects,
  getUserConversation,
  listAllConversations,
  renameConversation,
  renameUserConversation,
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
import { listAllConversationsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useDebounce } from '@/hooks/useDebounce'
import { ProjectCardMedia } from '@/components/dashboard/ProjectCardMedia'
import { ProjectAvatar } from '@/components/project/ProjectAvatar'
import { useQuery } from '@tanstack/react-query'
import { AiSettingsMenu } from './AiSettingsMenu'
import { ChatProjectMediaProvider } from './ChatProjectMedia'
import { chatProjectQueryOptions } from './chat-project-media'
import { useChatProjectMedia } from './chat-project-media-context'
import { ConversationSandboxChip } from './ConversationSandboxChip'
import { ChatRunBadge, ChatRunMark } from './ChatRunBadge'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
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
import { KbdBadge } from '@/components/ui/kbd-badge'
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from '@/components/ui/resizable'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useIsDesktop } from '@/components/hooks/use-mobile'
import { cn } from '@/lib/utils'
import {
  Bell,
  Boxes,
  ChevronDown,
  ChevronLeft,
  ChevronsUpDown,
  ExternalLink,
  LayoutDashboard,
  FolderGit2,
  GitBranch,
  Loader2,
  Maximize2,
  MessageSquare,
  Pencil,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  RotateCcw,
  Search,
  Sparkles,
  Trash2,
  X,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useEffectEvent,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { useDefaultLayout, usePanelRef } from 'react-resizable-panels'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { AssistantHeaderSlot } from './assistant-header-slot'

/** Where "Back to console" lands. */
const CONSOLE_HOME_PATH = '/projects'
import { toast } from 'sonner'
import { AiChatContext, useAiAssistant } from './AiAssistantContext'
import { AiAssistantGate } from './AiAssistantGate'
import {
  appendChatPage,
  CHAT_LIST_PAGE_SIZE,
  chatListRefreshSize,
  conversationListNeedsRefresh,
  createProjectChat,
  hasProjectScope,
  nextChatListPage,
  refreshChatList,
  resolvePageChat,
  withLinkedChat,
  chatsNotListed,
} from './chat-page-state'
import { DebugChatPanel } from './DebugChatPanel'
import {
  CHAT_LIST_CHANGED_EVENT,
  NEW_CHAT_IN_EVENT,
  OPEN_CHAT_EVENT,
  type NewChatInDetail,
} from './chat-blueprint'
import {
  CHAT_TURN_EVENT,
  chatListPollInterval,
  withTurnStatus,
  type ChatTurnDetail,
} from './chat-run-state'
import { AgentSwitcher, type SwitcherChat } from './AgentSwitcher'
import {
  AssistantStart,
  AssistantThread,
  type NewChatWorkspaceChoice,
} from './AssistantPanels'
import { groupChatsByWorkspace } from './conversation-groups'
import {
  isChatListShortcut,
  isSwitcherShortcut,
  readChatListHidden,
  writeChatListHidden,
} from './assistant-view'
import {
  liveSearchParams,
  syncedPageParams,
  withoutChat,
} from './assistant-url'
import type { OpeningAttachment } from './attachment-previews'
import { chatTint, isAssistantConversation } from './assistant-state'

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

/**
 * A row in the conversation list: a project-attached chat, or a projectless
 * assistant chat (`project_id` is null), which uses the user-rooted routes.
 */
type ProjectConversation = GlobalConversationResponse

/** The kinds of chat this view lists: project chats and the assistant's. */
function isListedKind(c: ProjectConversation): boolean {
  return hasProjectScope(c) || isAssistantConversation(c)
}

/**
 * A chat loaded by id, in the shape the list holds, or null when this view
 * does not show it (archived, or a kind it does not list).
 */
function linkedChat(
  detail: ConversationDetailResponse
): ProjectConversation | null {
  const {
    messages: _messages,
    page: _page,
    pending_permission: _pending,
    ...chat
  } = detail
  return chat.status === 'active' && isListedKind(chat) ? chat : null
}

/** The open assistant conversation, and the first message to send into it. */
interface AssistantChat {
  conversation: Pick<ConversationResponse, 'public_id' | 'context_id'> & {
    title?: string | null
  }
  firstMessage?: string
  firstAttachments?: OpeningAttachment[]
}

const CONTEXT_META: Record<string, { label: string; Icon: typeof GitBranch }> =
  {
    deployment: { label: 'Deployment', Icon: GitBranch },
    alert: { label: 'Alert', Icon: Bell },
    project: { label: 'Project', Icon: FolderGit2 },
    assistant: { label: 'Assistant', Icon: Sparkles },
  }

function metaFor(contextType: string) {
  return (
    CONTEXT_META[contextType] ?? { label: contextType, Icon: MessageSquare }
  )
}

/** A chat as the ⌘K switcher shows it. */
function toSwitcherChat(c: ProjectConversation): SwitcherChat {
  return {
    id: c.public_id,
    title: c.title ?? `${metaFor(c.context_type).label} ${c.context_id}`,
    subtitle: isAssistantConversation(c)
      ? 'Assistant'
      : (c.project_name ?? metaFor(c.context_type).label),
    lastActivityAt: c.last_activity_at,
  }
}

/**
 * Project identity with a small context-type badge (deployment/alert/project)
 * overlaid in the corner. Shared by the conversation list and the open-chat
 * header so a chat looks the same in both places.
 */
/**
 * The project's logo as the project list shows it: the latest deployment's
 * screenshot or favicon, then the service template's image, then the initial.
 */
function ProjectLogo({
  projectId,
  name,
  className,
}: {
  projectId: number
  name: string
  className?: string
}) {
  const project = useQuery(chatProjectQueryOptions(projectId))
  // The media is loaded once for the whole list (ChatProjectMediaProvider).
  const media = useChatProjectMedia(projectId)
  const hosted = project.data != null && project.data.source_type !== 'external'
  const deployed = hosted ? media : undefined
  return (
    <ProjectCardMedia
      name={project.data?.name ?? name}
      className={className}
      deploymentUrl={deployed?.url}
      screenshotLocation={deployed?.screenshot_location}
      templateImageUrl={project.data?.service_template_image_url}
    />
  )
}

function ContextAvatar({
  projectId,
  projectName,
  contextType,
  seed,
  className,
  badgeClassName,
}: {
  projectId?: number | null
  projectName?: string
  contextType: string
  /** Chat id. A projectless chat has no project to identify it, so it gets a
   * stable colour from this instead. */
  seed?: string
  className?: string
  badgeClassName?: string
}) {
  const { label, Icon } = metaFor(contextType)
  if (!projectName && seed) {
    return (
      <div
        className={cn(
          'flex size-8 shrink-0 items-center justify-center rounded-md',
          chatTint(seed).tile,
          className
        )}
        title={label}
      >
        <Icon className="h-4 w-4" />
      </div>
    )
  }
  return (
    <div className="relative shrink-0">
      {projectId != null && projectName ? (
        <ProjectLogo
          projectId={projectId}
          name={projectName}
          className={cn('size-8', className)}
        />
      ) : (
        <ProjectAvatar
          name={projectName ?? label}
          className={cn('size-8 rounded-md', className)}
          fallbackClassName="rounded-md bg-primary/10 text-xs text-primary"
        />
      )}
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
          <AiAssistantGate onClose={close}>
            <DockBody
              key={openSeq}
              initialContext={initialContext}
              onClose={close}
            />
          </AiAssistantGate>
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
  const isPage = layout === 'page'

  // The chat list is a resizable left panel on the full-screen page. The size
  // is remembered across visits, but only once the user has dragged it — a
  // collapse caused by a narrow window must not be saved as their preference.
  const isDesktop = useIsDesktop()
  const sidebarRef = usePanelRef()
  const { defaultLayout, onLayoutChanged } = useDefaultLayout({
    id: 'assistant-chat-list-split',
    onlySaveAfterUserInteractions: true,
  })

  // One layout: the chat list beside the conversation, which the user can
  // hide to give the conversation the whole width. Hidden, the list is still
  // behind the title and ⌘K. Remembered across visits.
  const [listHidden, setListHidden] = useState(readChatListHidden)
  const toggleList = useCallback(() => {
    setListHidden((hidden) => {
      writeChatListHidden(!hidden)
      return !hidden
    })
  }, [])
  const listShown = isDesktop && !listHidden
  // The switcher stands in for the list while it is out of sight.
  const switcherOn = isPage && isDesktop && listHidden
  const [switcherOpen, setSwitcherOpen] = useState(false)
  // The switcher filters loaded chats as you type; this finds the rest, which
  // the list has not paged in yet.
  const [switcherSearch, setSwitcherSearch] = useState('')
  const switcherQuery = useDebounce(switcherSearch.trim(), 250)
  const switcherMatches = useQuery({
    ...listAllConversationsOptions({
      query: { search: switcherQuery, page_size: 20 },
    }),
    enabled: switcherOpen && switcherQuery.length > 0,
    staleTime: 30_000,
  })
  useEffect(() => {
    if (!isPage) return
    const panel = sidebarRef.current
    if (!panel) return
    if (listShown) {
      if (panel.isCollapsed()) panel.expand()
    } else if (!panel.isCollapsed()) {
      panel.collapse()
    }
  }, [isPage, listShown, sidebarRef])

  // ⌘B shows or hides the list on wide screens, where it can be shown. It is
  // the console sidebar's key, and that sidebar is not on this page.
  useEffect(() => {
    if (!isPage || !isDesktop) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isChatListShortcut(event)) return
      event.preventDefault()
      event.stopImmediatePropagation()
      toggleList()
    }
    window.addEventListener('keydown', onKeyDown, true)
    return () => window.removeEventListener('keydown', onKeyDown, true)
  }, [isPage, isDesktop, toggleList])

  // ⌘K opens the switcher while the list is hidden. It takes the shortcut
  // before the console-wide palette sees it; with the list shown the palette
  // keeps it.
  useEffect(() => {
    if (!switcherOn) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isSwitcherShortcut(event)) return
      event.preventDefault()
      event.stopImmediatePropagation()
      setSwitcherOpen((open) => !open)
    }
    window.addEventListener('keydown', onKeyDown, true)
    return () => window.removeEventListener('keydown', onKeyDown, true)
  }, [switcherOn])

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
  // Every chat the list endpoint returned, including kinds this view does not
  // show: pages are counted in the endpoint's terms, not in what is shown.
  const [listedChats, setListedChats] = useState<ProjectConversation[]>([])
  const conversations = useMemo(
    () => listedChats.filter(isListedKind),
    [listedChats]
  )
  const olderMatches = useMemo(
    () =>
      chatsNotListed(
        conversations,
        (switcherMatches.data ?? []).filter(isListedKind)
      ),
    [conversations, switcherMatches.data]
  )
  // Every project a logo in this view can show: the listed chats, the older
  // matches of the switcher and the open chat.
  const mediaProjectIds = useMemo(
    () => [
      ...conversations.map((c) => c.project_id),
      ...olderMatches.map((c) => c.project_id),
      active?.projectId,
    ],
    [conversations, olderMatches, active?.projectId]
  )
  // Whether the endpoint has older chats than the pages loaded so far.
  const [hasMoreChats, setHasMoreChats] = useState(false)
  const [loadingMoreChats, setLoadingMoreChats] = useState(false)
  // The fetches merge into the latest list, not the one they started from.
  const listedChatsRef = useRef<ProjectConversation[]>([])
  const hasMoreChatsRef = useRef(false)
  useEffect(() => {
    listedChatsRef.current = listedChats
    hasMoreChatsRef.current = hasMoreChats
  }, [listedChats, hasMoreChats])
  // Chats opened from a link that the loaded pages had not reached. They are
  // shown but not counted, or the next page would start past them.
  const linkedChatIdsRef = useRef(new Set<string>())
  const [loadingList, setLoadingList] = useState(false)
  // Set once the first list fetch settles (success or failure) — distinct
  // from `loadingList` (which starts `false` before the fetch is even
  // scheduled) so the URL-resolution effect below can tell "haven't tried
  // yet" apart from "tried and finished" without a race on mount.
  const [listReady, setListReady] = useState(false)
  const [activePublicId, setActivePublicId] = useState<string | null>(null)
  const [resetKey, setResetKey] = useState(0)
  const [pendingDelete, setPendingDelete] = useState<{
    projectId: number | null
    publicId: string
    title: string
  } | null>(null)
  const [pendingRename, setPendingRename] = useState<{
    projectId: number | null
    publicId: string
  } | null>(null)
  // Assistant chat: `assistantStarting` is the prompt screen before a
  // conversation exists; `assistant` is the open conversation.
  const [assistant, setAssistant] = useState<AssistantChat | null>(null)
  // The full-screen page opens on the assistant (onboarding, or the starter
  // prompts) unless the URL names a conversation to resume.
  const [assistantStarting, setAssistantStarting] = useState(
    () => layout === 'page' && !urlChatId && !initialContext
  )
  // The workspace a chat being started joins, when it was started from that
  // workspace's group in the list.
  const [newChatWorkspace, setNewChatWorkspace] =
    useState<NewChatWorkspaceChoice | null>(null)
  const [renameValue, setRenameValue] = useState('')
  const [renaming, setRenaming] = useState(false)
  // When true, the body shows the project picker for starting a fresh
  // project-scoped chat (a new thread, not tied to a deployment/alert).
  const [picking, setPicking] = useState(false)

  const listedCount = useCallback(
    () =>
      listedChatsRef.current.filter(
        (c) => !linkedChatIdsRef.current.has(c.public_id)
      ).length,
    []
  )
  const applyListedChats = useCallback(
    (chats: ProjectConversation[], hasMore: boolean) => {
      listedChatsRef.current = chats
      hasMoreChatsRef.current = hasMore
      setListedChats(chats)
      setHasMoreChats(hasMore)
    },
    []
  )

  // Unified list across every project. Project chats use the legacy project
  // routes and assistant chats the user-rooted ones; other user-owned
  // application/global chats live in the AI workspace and are filtered out
  // (see `isListedKind`) rather than coerced into a fabricated project scope.
  // A refresh re-reads every page already loaded, so it does not collapse a
  // list the user scrolled back through.
  const loadList = useCallback(() => {
    setLoadingList(true)
    const requested = chatListRefreshSize(listedCount())
    listAllConversations({ query: { page: 1, page_size: requested } })
      .then(({ data }) => {
        if (!data) return
        // A page that reaches a linked chat lists it like any other.
        for (const c of data) linkedChatIdsRef.current.delete(c.public_id)
        const { chats, hasMore } = refreshChatList(
          listedChatsRef.current,
          data,
          requested,
          hasMoreChatsRef.current
        )
        applyListedChats(chats, hasMore)
      })
      .catch(() => undefined)
      .finally(() => {
        setLoadingList(false)
        setListReady(true)
      })
  }, [applyListedChats, listedCount])

  // The next page of older chats. Chats created elsewhere since the list was
  // loaded shift every page down, so a page can hold only chats already shown;
  // a few more pages are read then, rather than leaving a button that does
  // nothing.
  const loadOlderChats = useCallback(async () => {
    setLoadingMoreChats(true)
    try {
      let page = nextChatListPage(listedCount())
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const { data } = await listAllConversations({
          query: { page, page_size: CHAT_LIST_PAGE_SIZE },
        })
        if (!data) throw new Error(`page ${page} of chats did not load`)
        const before = listedCount()
        for (const c of data) linkedChatIdsRef.current.delete(c.public_id)
        const hasMore = data.length === CHAT_LIST_PAGE_SIZE
        applyListedChats(appendChatPage(listedChatsRef.current, data), hasMore)
        if (!hasMore || listedCount() > before) return
        page += 1
      }
    } catch {
      toast.error('Could not load older chats', {
        description: 'Check your connection and try again.',
      })
    } finally {
      setLoadingMoreChats(false)
    }
  }, [applyListedChats, listedCount])

  // A project chat is created lazily by DebugChatPanel when its first message
  // is sent. Until that point the list is necessarily a pre-creation snapshot.
  // Reconcile the parent as one transition: select the new conversation,
  // and fetch the row so it appears in the chat list immediately.
  // Existing conversations are already present and do not need another fetch.
  const handleConversationChange = useCallback(
    (publicId: string | null) => {
      setActivePublicId(publicId)
      if (!conversationListNeedsRefresh(publicId, conversations)) return
      loadList()
    },
    [conversations, loadList]
  )

  useEffect(() => {
    // A contextual dock chat opens on that chat alone; the full-screen page
    // shows its chat list beside it, so it still needs the list.
    if (initialContext && !isPage) return
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
  }, [initialContext, isPage, loadList])

  // The open chat says when its turn starts and ends, so its row changes at
  // once; the final state (finished or error) is read back from the server.
  useEffect(() => {
    const onTurn = (event: Event) => {
      const detail = (event as CustomEvent<ChatTurnDetail>).detail
      if (!detail?.publicId) return
      const listed = listedChatsRef.current.find(
        (c) => c.public_id === detail.publicId
      )
      if (!listed) return
      if (detail.active) {
        applyListedChats(
          withTurnStatus(listedChatsRef.current, detail.publicId, 'running'),
          hasMoreChatsRef.current
        )
      } else if (listed.turn_status === 'running') {
        loadList()
      }
    }
    window.addEventListener(CHAT_TURN_EVENT, onTurn)
    return () => window.removeEventListener(CHAT_TURN_EVENT, onTurn)
  }, [applyListedChats, loadList])

  // The workspace chip moved a chat or renamed an application: the groups
  // the list shows changed, so it reads them again now.
  useEffect(() => {
    const refresh = () => loadList()
    window.addEventListener(CHAT_LIST_CHANGED_EVENT, refresh)
    return () => window.removeEventListener(CHAT_LIST_CHANGED_EVENT, refresh)
  }, [loadList])

  // Chats keep working in the background (a goal brings one back, another
  // tab sent a message), so the list checks again: often while one is
  // working, rarely otherwise, and as soon as the tab is looked at again.
  const pollInterval = chatListPollInterval(listedChats)
  useEffect(() => {
    if (!listReady) return
    const refresh = () => {
      if (document.visibilityState === 'visible') loadList()
    }
    const timer = window.setInterval(refresh, pollInterval)
    document.addEventListener('visibilitychange', refresh)
    return () => {
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', refresh)
    }
  }, [listReady, pollInterval, loadList])

  const openConversation = useCallback((c: ProjectConversation) => {
    // The list already knows this id. Setting it immediately avoids an
    // intermediate render where URL synchronization can mistake a resolved
    // conversation for an unsaved draft.
    setActivePublicId(c.public_id)
    setPicking(false)
    setAssistantStarting(false)
    if (isAssistantConversation(c)) {
      setActive(null)
      setAssistant({ conversation: c })
      return
    }
    setAssistant(null)
    if (!hasProjectScope(c)) return
    setActive({
      projectId: c.project_id ?? null,
      projectSlug: c.project_slug ?? undefined,
      projectName: c.project_name ?? undefined,
      contextType: c.context_type,
      contextId: c.context_id,
      title: c.title ?? undefined,
      autoStart: false,
    })
  }, [])

  // A `?chat=` route to a chat the loaded pages do not include. It may simply
  // be older than them, so it is loaded by id and shown at its place in the
  // list; only a chat that is gone (or archived) falls back to the start
  // screen.
  const openLinkedChat = useCallback(
    async (publicId: string) => {
      const detail = await getUserConversation({
        path: { public_id: publicId },
        query: { limit: 1 },
      })
        .then(({ data }) => data)
        .catch(() => undefined)
      // The user may have moved on while it loaded.
      if (liveSearchParams().get('chat') !== publicId) return
      const chat = detail ? linkedChat(detail) : null
      if (!chat) {
        setSearchParams(withoutChat(liveSearchParams()), { replace: true })
        setAssistantStarting(true)
        return
      }
      if (!listedChatsRef.current.some((c) => c.public_id === chat.public_id)) {
        linkedChatIdsRef.current.add(chat.public_id)
        setListedChats((prev) => withLinkedChat(prev, chat))
      }
      openConversation(chat)
    },
    [openConversation, setSearchParams]
  )

  // Another part of the page (the workspace chip) asks to open a chat of the
  // same application. It may not be in the loaded pages, so it is loaded by
  // id like a linked chat.
  useEffect(() => {
    const onOpenChat = (event: Event) => {
      const publicId = (event as CustomEvent<{ publicId?: string }>).detail
        ?.publicId
      if (!publicId) return
      const listed = listedChatsRef.current.find(
        (c) => c.public_id === publicId
      )
      if (listed) {
        openConversation(listed)
        return
      }
      void getUserConversation({
        path: { public_id: publicId },
        query: { limit: 1 },
      })
        .then(({ data }) => data)
        .catch(() => undefined)
        .then((detail) => {
          const chat = detail ? linkedChat(detail) : null
          if (!chat) {
            toast.error('That chat is no longer available')
            return
          }
          linkedChatIdsRef.current.add(chat.public_id)
          setListedChats((prev) => withLinkedChat(prev, chat))
          openConversation(chat)
        })
    }
    window.addEventListener(OPEN_CHAT_EVENT, onOpenChat)
    return () => window.removeEventListener(OPEN_CHAT_EVENT, onOpenChat)
  }, [openConversation])

  // Resolve a `?chat=` route exactly once after the list arrives. A valid id
  // is restored; a stale one falls back to the assistant start screen. A bare
  // route needs no resolving: the page already opened on that screen, so the
  // ref starts out resolved. Otherwise the `?chat=` this component writes for
  // the first chat it creates would be mistaken for a route to restore, and
  // because the list was fetched before that chat existed it would not be
  // found, which sent the user back to the start screen right after sending.
  // Keeping this separate from ongoing URL synchronization prevents the
  // mount-time race where a still-null active id removed a valid URL before it
  // could be resolved.
  const initialPageRouteResolvedRef = useRef(!urlChatId)
  useEffect(() => {
    if (layout !== 'page' || initialContext || !urlChatId) return
    if (initialPageRouteResolvedRef.current || !listReady) return

    const conversation = resolvePageChat(urlChatId, conversations)

    // Defer the state transition out of the effect body. The cancellation
    // guard keeps React Strict Mode's setup/cleanup replay from consuming the
    // one-shot ref without ever opening the resolved conversation.
    let cancelled = false
    queueMicrotask(() => {
      if (cancelled) return
      initialPageRouteResolvedRef.current = true
      if (conversation) {
        openConversation(conversation)
        return
      }
      // Older than the pages loaded so far, or gone: ask for it by id.
      void openLinkedChat(urlChatId)
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
    openLinkedChat,
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
    // `searchParams` (in the deps) re-runs this; the write starts from the live address.
    const next = syncedPageParams(liveSearchParams(), {
      chat: activePublicId,
    })
    if (next) setSearchParams(next, { replace: true })
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
    setAssistant(null)
    setAssistantStarting(false)
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
      setSearchParams(withoutChat(liveSearchParams()), { replace: true })
    }
  }

  const backToList = () => {
    setActive(null)
    setAssistant(null)
    setAssistantStarting(false)
    setActivePublicId(null)
    setPicking(false)
    loadList()
  }

  // Open the project picker. It only renders when no chat is open, so any open
  // conversation (or the assistant's start screen) has to be closed first —
  // otherwise the button looks dead while a chat is showing.
  const startPicking = () => {
    setActive(null)
    setAssistant(null)
    setAssistantStarting(false)
    setActivePublicId(null)
    setPicking(true)
    if (layout === 'page' && searchParams.has('chat')) {
      setSearchParams(withoutChat(liveSearchParams()), { replace: true })
    }
  }

  const startAssistant = () => startAssistantIn(null)

  const startAssistantIn = (workspace: NewChatWorkspaceChoice | null) => {
    setNewChatWorkspace(workspace)
    setPicking(false)
    setActive(null)
    setAssistant(null)
    setActivePublicId(null)
    setAssistantStarting(true)
    // A new chat must not leave the previous conversation's id in the address
    // bar; the sync effect writes the new one once its first message creates it.
    if (layout === 'page' && searchParams.has('chat')) {
      setSearchParams(withoutChat(liveSearchParams()), { replace: true })
    }
  }

  // The workspace chip's "New chat in …" does what the list's "+ Chat" does.
  const onNewChatIn = useEffectEvent((application: NewChatInDetail) =>
    startAssistantIn(application)
  )
  useEffect(() => {
    const handler = (event: Event) => {
      const application = (event as CustomEvent<NewChatInDetail>).detail
      if (application?.public_id) onNewChatIn(application)
    }
    window.addEventListener(NEW_CHAT_IN_EVENT, handler)
    return () => window.removeEventListener(NEW_CHAT_IN_EVENT, handler)
  }, [])

  const handleAssistantStarted = (
    conversation: ConversationResponse,
    firstMessage: string,
    firstAttachments: OpeningAttachment[]
  ) => {
    setAssistantStarting(false)
    setActivePublicId(conversation.public_id)
    setAssistant({ conversation, firstMessage, firstAttachments })
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
    await (
      d.projectId == null
        ? archiveUserConversation({ path: { public_id: d.publicId } })
        : archiveConversation({
            path: { project_id: d.projectId, public_id: d.publicId },
          })
    ).catch(() => {})
    toast.success('Chat deleted')
    if (activePublicId === d.publicId) {
      // Full screen has the sidebar for browsing; land on a fresh chat.
      if (isPage) {
        startAssistant()
        loadList()
      } else {
        backToList()
      }
    } else {
      setListedChats((prev) => prev.filter((c) => c.public_id !== d.publicId))
    }
  }

  const startRename = (c: ProjectConversation) => {
    setRenameValue(c.title ?? '')
    setPendingRename({ projectId: c.project_id ?? null, publicId: c.public_id })
  }

  const confirmRename = async () => {
    const r = pendingRename
    const title = renameValue.trim()
    if (!r || !title) return
    setRenaming(true)
    try {
      const { data } =
        r.projectId == null
          ? await renameUserConversation({
              path: { public_id: r.publicId },
              body: { title },
            })
          : await renameConversation({
              path: { project_id: r.projectId, public_id: r.publicId },
              body: { title },
            })
      const newTitle = data?.title ?? title
      setListedChats((prev) =>
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

  const assistantStart = (
    <AssistantStart
      onStarted={handleAssistantStarted}
      onNavigate={() => {
        if (typeof window !== 'undefined' && window.innerWidth < 1024) onClose()
      }}
      cloudPath="/settings/cloud"
      roomy={isPage}
      workspace={newChatWorkspace}
      onLeaveWorkspace={() => setNewChatWorkspace(null)}
    />
  )

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
  const inProjectChat = active !== null
  const inAssistant = assistant !== null || assistantStarting
  const inConversation = inProjectChat || inAssistant
  const href = active ? sourceHref(active) : null
  const sourceLabel = active
    ? `${metaFor(active.contextType).label}${active.projectName ? ` · ${active.projectName}` : ''}`
    : ''

  // Full screen: past chats live in a left sidebar, so switching or starting
  // one never leaves the conversation. Below `lg` the sidebar panel is collapsed
  // and the back arrow in the header opens the same list instead.
  const sidebar = (
    <aside
      aria-label="Chats"
      // Collapsed to zero width, the list must not take focus either.
      inert={!listShown}
      className="hidden h-full min-w-0 flex-col lg:flex"
    >
      <div className="flex flex-wrap items-center justify-between gap-2 px-3 py-3">
        <div className="flex items-center gap-1">
          <Button
            type="button"
            size="icon"
            variant="ghost"
            className="-ml-1 size-8 text-muted-foreground"
            onClick={toggleList}
            aria-label="Hide chat list"
            title="Hide chat list (⌘B)"
          >
            <PanelLeftClose className="size-4" />
          </Button>
          <h2 className="text-sm font-semibold">Chats</h2>
        </div>
        <div className="flex items-center gap-1">
          <Button
            size="sm"
            onClick={startAssistant}
            className="h-8 gap-1"
            title="Ask Temps to inspect or change something"
          >
            <Sparkles className="h-4 w-4" />
            New
          </Button>
        </div>
      </div>
      <div className="min-h-0 flex-1 px-2 pb-3">
        <ConversationList
          loading={loadingList && conversations.length === 0}
          conversations={conversations}
          hasMore={hasMoreChats}
          loadingMore={loadingMoreChats}
          onLoadMore={loadOlderChats}
          activeId={activePublicId}
          onOpen={openConversation}
          onNewChatIn={startAssistantIn}
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
              projectId: c.project_id ?? null,
              publicId: c.public_id,
              title:
                c.title ?? `${metaFor(c.context_type).label} ${c.context_id}`,
            })
          }
        />
      </div>
    </aside>
  )

  const headerTitle = inAssistant
    ? (assistant?.conversation.title ?? 'Temps assistant')
    : inConversation
      ? (active?.title ?? 'AI chat')
      : picking
        ? 'New project chat'
        : 'AI assistant'

  // Where the open chat puts its tools: the header, not a row above it.
  const [headerSlot, setHeaderSlot] = useState<HTMLDivElement | null>(null)

  // On a phone the header is a single line: the subtitle gives way to the
  // icon buttons below.
  const subtitleClass = isPage ? 'max-md:hidden' : undefined

  const main = (
    <AssistantHeaderSlot.Provider value={headerSlot}>
      <div
        className={cn(
          isPage && 'flex h-full min-h-0 min-w-0 flex-1 flex-col',
          !isPage && 'contents'
        )}
      >
        <div
          className={cn(
            'flex items-start justify-between gap-2',
            isPage && 'items-center border-b px-3 py-2 sm:px-6 sm:py-3'
          )}
        >
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2">
              {inConversation || picking ? (
                <button
                  type="button"
                  onClick={
                    inConversation ? backToList : () => setPicking(false)
                  }
                  className={cn(
                    '-ml-1 rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground',
                    isPage && 'lg:hidden'
                  )}
                  aria-label="Back to all conversations"
                  title="All chats"
                >
                  <ChevronLeft className="h-4 w-4" />
                </button>
              ) : (
                <Sparkles className="h-5 w-5 text-primary" />
              )}
              {inProjectChat && active && (
                <ContextAvatar
                  projectId={active.projectId}
                  projectName={active.projectName}
                  contextType={active.contextType}
                  className="size-7 max-md:hidden"
                />
              )}
              <h2 className="min-w-0 truncate text-base font-semibold sm:text-lg">
                {switcherOn ? (
                  <button
                    type="button"
                    onClick={() => setSwitcherOpen(true)}
                    className="-ml-1.5 inline-flex max-w-full items-center gap-2 rounded-md px-1.5 py-0.5 text-left transition-colors hover:bg-accent"
                    title="Switch chat (⌘K)"
                    aria-label={`${headerTitle}. Switch chat`}
                    aria-haspopup="dialog"
                  >
                    <span className="truncate">{headerTitle}</span>
                    <ChevronsUpDown
                      className="size-4 shrink-0 text-muted-foreground"
                      aria-hidden="true"
                    />
                    <KbdBadge keys={['⌘', 'K']} />
                  </button>
                ) : (
                  headerTitle
                )}
              </h2>
            </div>
            {inAssistant ? (
              <p className={cn(subtitleClass, 'text-sm text-muted-foreground')}>
                Acts with your own permissions
              </p>
            ) : inConversation ? (
              <p
                className={cn(
                  subtitleClass,
                  'flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground'
                )}
              >
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
              <p className={cn(subtitleClass, 'text-sm text-muted-foreground')}>
                Choose a project to start a general chat about it.
              </p>
            ) : (
              <p className={cn(subtitleClass, 'text-sm text-muted-foreground')}>
                Resume any AI conversation across your projects, or start a new
                chat for a project.
              </p>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-0.5">
            {/* The open chat's own tools render here (see `InAssistantHeader`). */}
            <div
              ref={setHeaderSlot}
              className="mr-1.5 flex items-center gap-0.5 border-r pr-1.5 empty:hidden"
            />
            {isPage && inConversation && href && (
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="size-8 md:hidden"
                onClick={() => goToSource(href)}
                title={`View source: ${sourceLabel}`}
                aria-label={`View source: ${sourceLabel}`}
              >
                <ExternalLink className="size-4" />
              </Button>
            )}
            {isPage && (
              <Button asChild size="icon" variant="ghost" className="size-8">
                <Link
                  to={CONSOLE_HOME_PATH}
                  title="Back to console"
                  aria-label="Back to console"
                >
                  <LayoutDashboard className="size-4" />
                </Link>
              </Button>
            )}
            {!inConversation && !picking && (
              <Button
                size="sm"
                onClick={startAssistant}
                className={cn('mr-1 h-8 gap-1', isPage && 'lg:hidden')}
                title="Ask Temps to inspect or change something"
              >
                <Sparkles className="h-4 w-4" />
                <span className="hidden sm:inline">Ask Temps</span>
              </Button>
            )}
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
                    onClick={startPicking}
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
                  onClick={startPicking}
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
            {inAssistant && (
              <button
                type="button"
                onClick={startAssistant}
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                title="New assistant chat"
                aria-label="New assistant chat"
              >
                <Plus className="h-4 w-4" />
              </button>
            )}
            {inProjectChat && active && (
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
            {inProjectChat && (
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
                    projectId: active?.projectId ?? null,
                    publicId: activePublicId,
                    title:
                      active?.title ??
                      assistant?.conversation.title ??
                      'this chat',
                  })
                }
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-destructive"
                title="Delete this chat"
                aria-label="Delete conversation"
              >
                <Trash2 className="h-4 w-4" />
              </button>
            )}
            <AiSettingsMenu />
            {!isPage && (
              <button
                type="button"
                onClick={onClose}
                className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                aria-label="Close AI assistant"
              >
                <X className="h-4 w-4" />
              </button>
            )}
          </div>
        </div>

        <div
          className={cn(
            'min-h-0 flex-1',
            // Full screen: a centred reading column, like a focused chat app.
            // A conversation uses the whole width (its working tabs sit beside
            // it and centre the chat column themselves); the rest read best in
            // a centred column.
            isPage && 'w-full px-4 pb-4 pt-3',
            isPage && !(assistant && !assistantStarting) && 'mx-auto max-w-3xl'
          )}
        >
          {assistantStarting ? (
            assistantStart
          ) : assistant ? (
            <AssistantThread
              key={assistant.conversation.public_id}
              conversation={assistant.conversation}
              firstMessage={assistant.firstMessage}
              firstAttachments={assistant.firstAttachments}
              syncUrl={isPage}
            />
          ) : inProjectChat ? (
            <DebugChatPanel
              key={`${active!.contextType}:${active!.contextId}:${activePublicId ?? 'new'}:${resetKey}`}
              projectId={active!.projectId}
              conversationPublicId={activePublicId ?? undefined}
              contextType={active!.contextType}
              contextId={active!.contextId}
              startPrompt={
                active!.startPrompt ??
                'Diagnose this and suggest concrete next steps.'
              }
              autoStart={active!.autoStart}
              lazyCreate={active!.contextType === 'project'}
              // A free-form project chat never auto-starts, so a prompt the
              // opener gave it goes in the composer for the user to send:
              // once. After the first send the chat exists, and the remount
              // under its id must not put the prompt back.
              initialDraft={
                active!.contextType === 'project' && !activePublicId
                  ? active!.startPrompt
                  : undefined
              }
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
          ) : isPage && conversations.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-3 text-center">
              <MessageSquare className="size-8 text-muted-foreground" />
              <div>
                <p className="font-medium">No AI conversations yet</p>
                <p className="mt-1 max-w-sm text-sm text-muted-foreground">
                  Ask Temps to inspect or change your setup, or start a project
                  chat to investigate deployments, logs, traces, and errors.
                </p>
              </div>
              <div className="flex gap-2">
                <Button size="sm" onClick={startAssistant}>
                  <Sparkles className="mr-1.5 size-4" />
                  Ask Temps
                </Button>
                <Button size="sm" variant="outline" onClick={startPicking}>
                  <Plus className="mr-1.5 size-4" />
                  Project chat
                </Button>
              </div>
            </div>
          ) : (
            <ConversationList
              loading={loadingList && conversations.length === 0}
              conversations={conversations}
              hasMore={hasMoreChats}
              loadingMore={loadingMoreChats}
              onLoadMore={loadOlderChats}
              onOpen={openConversation}
              onNewChatIn={startAssistantIn}
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
                  projectId: c.project_id ?? null,
                  publicId: c.public_id,
                  title:
                    c.title ??
                    `${metaFor(c.context_type).label} ${c.context_id}`,
                })
              }
            />
          )}
        </div>

        <AgentSwitcher
          open={switcherOpen}
          onOpenChange={(open) => {
            setSwitcherOpen(open)
            if (!open) setSwitcherSearch('')
          }}
          chats={conversations.map(toSwitcherChat)}
          search={switcherSearch}
          onSearchChange={setSwitcherSearch}
          moreChats={olderMatches.map(toSwitcherChat)}
          searching={
            switcherSearch.trim() !== switcherQuery ||
            switcherMatches.isFetching
          }
          activeId={activePublicId}
          onOpen={(id) => {
            const chat = conversations.find((c) => c.public_id === id)
            if (chat) {
              openConversation(chat)
              return
            }
            // An older chat found by the search: list it, then open it.
            const match = olderMatches.find((c) => c.public_id === id)
            if (!match) return
            linkedChatIdsRef.current.add(match.public_id)
            setListedChats((prev) => withLinkedChat(prev, match))
            openConversation(match)
          }}
          onNewAssistant={startAssistant}
          onNewProjectChat={startPicking}
        />

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
    </AssistantHeaderSlot.Provider>
  )

  if (!isPage) {
    return (
      <ChatProjectMediaProvider projectIds={mediaProjectIds}>
        <div className="flex h-full flex-col gap-3 p-4">{main}</div>
      </ChatProjectMediaProvider>
    )
  }

  return (
    <ChatProjectMediaProvider projectIds={mediaProjectIds}>
      <div className="flex h-full min-w-0">
        {/* Hidden, the list folds into a rail rather than disappearing (and the
          rail holds the way to show it again). */}
        {isDesktop && listHidden && (
          <ChatRail
            conversations={conversations}
            activeId={activePublicId}
            onOpen={openConversation}
            onShowList={toggleList}
            onNew={startAssistant}
            onFind={() => setSwitcherOpen(true)}
          />
        )}
        <ResizablePanelGroup
          orientation="horizontal"
          className="h-full min-w-0 flex-1"
          defaultLayout={defaultLayout}
          onLayoutChanged={(layout, meta) => {
            onLayoutChanged(layout, meta)
            // Once a drag ends: a shown list never rests at zero width; hiding it
            // is the header button's job.
            const panel = sidebarRef.current
            if (listShown && panel?.isCollapsed()) panel.expand()
          }}
        >
          {/* Numbers are pixels in v4, strings are percentages: the list can grow
          to 40% of the screen but never crowd out the conversation. Below `lg`
          it is collapsed to nothing (see the effect above); on desktop
          it stops at the minimum so it can't be dragged away. */}
          <ResizablePanel
            id="chats"
            panelRef={sidebarRef}
            defaultSize={288}
            minSize={260}
            maxSize="40"
            collapsible
            collapsedSize={0}
            disabled={!listShown}
            onResize={(size) => {
              // Collapsing is only for hiding the list (the user hid it, or the
              // screen is narrow); a drag that empties a shown list springs back.
              if (size.asPercentage === 0 && listShown)
                sidebarRef.current?.expand()
            }}
            className="min-w-0"
          >
            {sidebar}
          </ResizablePanel>
          <ResizableHandle
            withHandle
            aria-label="Resize chat list"
            className={listShown ? undefined : 'hidden'}
          />
          <ResizablePanel id="thread" minSize={isDesktop ? 360 : 0}>
            {main}
          </ResizablePanel>
        </ResizablePanelGroup>
      </div>
    </ChatProjectMediaProvider>
  )
}

function ConversationList({
  loading,
  conversations,
  hasMore,
  loadingMore,
  onLoadMore,
  activeId,
  onOpen,
  onOpenSource,
  onRename,
  onDelete,
  onNewChatIn,
}: {
  loading: boolean
  conversations: ProjectConversation[]
  /** Whether older chats than the ones shown exist. */
  hasMore: boolean
  loadingMore: boolean
  onLoadMore: () => void
  /** Public id of the chat currently open, highlighted in list-detail layouts. */
  activeId?: string | null
  onOpen: (c: ProjectConversation) => void
  onOpenSource: (c: ProjectConversation) => void
  onRename: (c: ProjectConversation) => void
  onDelete: (c: ProjectConversation) => void
  /** Starts a new chat that joins the given workspace. */
  onNewChatIn: (workspace: NewChatWorkspaceChoice) => void
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
  if (conversations.length === 0 && !hasMore) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 text-center">
        <MessageSquare className="h-7 w-7 text-muted-foreground" />
        <p className="text-sm font-medium">No AI conversations yet</p>
        <p className="max-w-xs text-sm text-muted-foreground">
          Click “Ask Temps” to manage your setup by chat, “New chat” to start
          one for a project, or open a failed deployment / firing alert and
          choose “Debug with AI”.
        </p>
      </div>
    )
  }
  const rowProps = { activeId, onOpen, onOpenSource, onRename, onDelete }
  return (
    <div className="h-full space-y-1 overflow-y-auto pr-1">
      {groupChatsByWorkspace(conversations).map((entry) =>
        entry.kind === 'chat' ? (
          <ConversationRow
            key={entry.chat.public_id}
            c={entry.chat}
            {...rowProps}
          />
        ) : (
          <WorkspaceChatGroup
            key={entry.workspace.public_id}
            workspace={entry.workspace}
            chats={entry.chats}
            onNewChat={() => onNewChatIn(entry.workspace)}
            {...rowProps}
          />
        )
      )}
      {hasMore && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="w-full text-muted-foreground"
          onClick={onLoadMore}
          disabled={loadingMore}
        >
          {loadingMore && <Loader2 className="size-4 animate-spin" />}
          Load older chats
        </Button>
      )}
    </div>
  )
}

type ConversationRowActions = {
  activeId?: string | null
  onOpen: (c: ProjectConversation) => void
  onOpenSource: (c: ProjectConversation) => void
  onRename: (c: ProjectConversation) => void
  onDelete: (c: ProjectConversation) => void
}

/**
 * The chats of one workspace together under its name, so the chats that share
 * an application's blueprint and sandbox read as one piece of work, with a
 * visible way to add another chat to it.
 */
function WorkspaceChatGroup({
  workspace,
  chats,
  onNewChat,
  ...rowProps
}: ConversationRowActions & {
  workspace: NewChatWorkspaceChoice
  chats: ProjectConversation[]
  onNewChat: () => void
}) {
  return (
    <section
      aria-label={`Chats in ${workspace.name}`}
      className="rounded-md border bg-muted/30 py-1"
    >
      <div className="flex items-center gap-1.5 py-1 pl-3 pr-1 text-xs text-muted-foreground">
        <Boxes className="size-3.5 shrink-0" aria-hidden />
        <span
          className="min-w-0 truncate font-medium text-foreground"
          title={`Chats in the application ${workspace.name} share its blueprint and sandbox`}
        >
          {workspace.name}
        </span>
        <span className="shrink-0">
          · {chats.length} {chats.length === 1 ? 'chat' : 'chats'}
        </span>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={onNewChat}
          className="ml-auto h-7 shrink-0 gap-1 px-2 text-xs"
          title={`Start another chat in ${workspace.name}`}
          aria-label={`New chat in ${workspace.name}`}
        >
          <Plus className="size-3.5" />
          <span>Chat</span>
        </Button>
      </div>
      <div className="space-y-0.5 px-1">
        {chats.map((c) => (
          <ConversationRow key={c.public_id} c={c} inWorkspace {...rowProps} />
        ))}
      </div>
    </section>
  )
}

/**
 * The chat list folded into a rail: each chat's avatar, in the list's order,
 * marked while it works or when it needs a look, so you can watch and switch
 * chats without the full list taking the width. Shown on wide screens while
 * the list is hidden.
 */
function ChatRail({
  conversations,
  activeId,
  onOpen,
  onShowList,
  onNew,
  onFind,
}: {
  conversations: ProjectConversation[]
  activeId?: string | null
  onOpen: (c: ProjectConversation) => void
  onShowList: () => void
  onNew: () => void
  /** Opens the switcher, which also finds chats the rail has not loaded. */
  onFind: () => void
}) {
  return (
    <TooltipProvider delayDuration={150}>
      <nav
        aria-label="Chats"
        className="flex h-full w-14 shrink-0 flex-col items-center gap-1 border-r py-3"
      >
        <RailAction label="Show chat list (⌘B)" onClick={onShowList}>
          <PanelLeftOpen className="size-4" />
        </RailAction>
        <RailAction label="New chat" onClick={onNew}>
          <Plus className="size-4" />
        </RailAction>
        <div className="my-1 h-px w-8 shrink-0 bg-border" aria-hidden />
        <ul className="flex min-h-0 w-full flex-1 flex-col items-center gap-1 overflow-y-auto px-2 py-1 [scrollbar-width:none]">
          {conversations.map((c) => {
            const { label } = metaFor(c.context_type)
            const title = c.title ?? `${label} ${c.context_id}`
            const active = c.public_id === activeId
            return (
              <li key={c.public_id}>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      onClick={() => onOpen(c)}
                      aria-label={title}
                      aria-current={active ? 'page' : undefined}
                      className={cn(
                        'relative rounded-lg p-1 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                        active
                          ? 'bg-muted ring-1 ring-border'
                          : 'hover:bg-accent'
                      )}
                    >
                      <ContextAvatar
                        projectId={c.project_id}
                        projectName={c.project_name ?? undefined}
                        contextType={c.context_type}
                        seed={c.public_id}
                      />
                      <ChatRunMark
                        turnStatus={c.turn_status}
                        className="absolute -right-0.5 -top-0.5"
                      />
                    </button>
                  </TooltipTrigger>
                  <TooltipContent side="right" className="max-w-64 space-y-1">
                    <p className="font-medium">{title}</p>
                    <div className="flex items-center gap-2 text-xs text-muted-foreground">
                      {c.project_name && (
                        <span className="truncate">{c.project_name}</span>
                      )}
                      <ChatRunBadge
                        turnStatus={c.turn_status}
                        failureTitle={c.failure?.title}
                      />
                      <TimeAgo date={c.last_activity_at} />
                    </div>
                  </TooltipContent>
                </Tooltip>
              </li>
            )
          })}
        </ul>
        <RailAction label="Find a chat (⌘K)" onClick={onFind}>
          <Search className="size-4" />
        </RailAction>
      </nav>
    </TooltipProvider>
  )
}

function RailAction({
  label,
  onClick,
  children,
}: {
  label: string
  onClick: () => void
  children: ReactNode
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-9 shrink-0 text-muted-foreground"
          onClick={onClick}
          aria-label={label}
        >
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  )
}

function ConversationRow({
  c,
  inWorkspace = false,
  activeId,
  onOpen,
  onOpenSource,
  onRename,
  onDelete,
}: ConversationRowActions & {
  c: ProjectConversation
  /** Shown under its workspace's name, so the row does not repeat it. */
  inWorkspace?: boolean
}) {
  const { label } = metaFor(c.context_type)
  const hasSource = !!sourceHref({
    contextType: c.context_type,
    contextId: c.context_id,
    projectSlug: c.project_slug ?? undefined,
  })
  return (
    <div
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
          seed={c.public_id}
        />
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-medium">
            {c.title ?? `${label} ${c.context_id}`}
          </div>
          {/* The project (name; its logo is the avatar), the sandbox the
              chat is linked to, and the time. A context label such as
              "Assistant" said nothing the avatar did not. */}
          <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
            {c.project_name && (
              <span className="min-w-0 flex-1 truncate">{c.project_name}</span>
            )}
            {c.workspace && !inWorkspace && (
              <span
                className="flex min-w-0 flex-1 items-center gap-1"
                title={`Part of the application ${c.workspace.name}`}
              >
                <Boxes className="size-3 shrink-0" aria-hidden />
                <span className="truncate">{c.workspace.name}</span>
              </span>
            )}
            {c.sandbox && (
              <ConversationSandboxChip
                name={c.sandbox.name}
                backend={c.sandbox.backend}
              />
            )}
            <ChatRunBadge
              turnStatus={c.turn_status}
              failureTitle={c.failure?.title}
            />
            <span
              className={cn(
                'min-w-0 truncate',
                c.project_name || (c.workspace && !inWorkspace)
                  ? 'shrink'
                  : 'flex-1'
              )}
            >
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

  const pick = useCallback((p: ProjectResponse) => onSelect(p), [onSelect])

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
              className="group flex w-full items-center gap-3 rounded-md border border-transparent px-2 py-2.5 text-left transition-colors hover:border-border hover:bg-accent"
            >
              <ProjectAvatar
                name={p.name}
                className="size-8 rounded-md"
                fallbackClassName="rounded-md bg-primary/10 text-xs text-primary"
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">{p.name}</div>
                {p.slug && (
                  <div className="truncate text-xs text-muted-foreground">
                    {p.slug}
                  </div>
                )}
              </div>
              <Plus className="h-4 w-4 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
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
