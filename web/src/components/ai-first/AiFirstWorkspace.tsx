// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Archive,
  ArchiveRestore,
  Boxes,
  Code2,
  FileCode2,
  FolderTree,
  GitBranch,
  GitCommitHorizontal,
  KeyRound,
  Loader2,
  LockKeyhole,
  MonitorPlay,
  Plus,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  Terminal,
  TerminalSquare,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import {
  archiveUserConversation,
  createApplication,
  createApplicationConversation,
  createApplicationPreviewLink,
  createGlobalConversation,
  controlApplicationWorkspace,
  getApplicationWorkspace,
  getApplicationWorkspaceChanges,
  getApplicationWorkspaceDiff,
  getGlobalAiWorkspace,
  listAiProviders,
  listAllConversations,
  listApplicationConversations,
  listApplications,
  listConnections,
  listGitProviders,
  listThreadArtifacts,
  restoreUserConversation,
  importApplicationWorkspaceGit,
  writeApplicationWorkspaceFiles,
  type ApplicationResponse,
  type ApplicationWorkspaceChangesResponse,
  type ApplicationWorkspaceDiffResponse,
  type ApplicationWorkspaceResponse,
  type ConversationResponse,
  type ConnectionResponse,
  type GlobalConversationResponse,
  type ProviderResponse,
  type ThreadArtifactResponse,
} from '@/api/client'
import { DebugChatPanel } from '@/components/ai/DebugChatPanel'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
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
import { Label } from '@/components/ui/label'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { cn } from '@/lib/utils'
import { ArtifactRenderer } from './ArtifactRenderer'
import { ApplicationPreviewPanel } from './ApplicationPreviewPanel'
import { ApplicationProjectsPanel } from './ApplicationProjectsPanel'
import { ApplicationWorkspaceSettingsPanel } from './ApplicationWorkspaceSettingsPanel'
import { WorkspaceDiffViewer } from './WorkspaceDiffViewer'
import { shouldRefreshArtifactsForLiveEvent } from './artifact-refresh'
import { problemDetail } from './problem-detail'
import {
  defaultWorkspaceSelection,
  initialApplicationThreadId,
  threadSelectionAfterRemoval,
} from './thread-selection'
import { threadDisplayStatus, type ThreadDisplayStatus } from './thread-status'
import { threadTitleFromLiveEvent } from './thread-title-event'
import {
  batchLocalImportFiles,
  fileToBase64,
  prepareLocalImport,
  type LocalImportSelection,
  type WorkspaceSourceMode,
} from './workspace-import'

import {
  workspaceHarnessOptions,
  workspaceStatusPresentation,
  type WorkspaceHarnessOption as HarnessOption,
} from './workspace-readiness'

const WORKSPACE_FILES_PAGE_SIZE = 100
const WORKSPACE_CLIENT_TIMEOUT_MS = 35_000

type RightView = 'generated' | 'files' | 'preview' | 'projects' | 'workspace'
type ThreadListMode = 'active' | 'archived'
type WorkspaceLoadPhase =
  'idle' | 'checking' | 'waking' | 'recovering' | 'inspecting'

// Claude is the first supported application runtime, while this stays open to
// any authenticated harness registered by Agent Sandbox.
function defaultHarnessId(harnesses: HarnessOption[]): string | null {
  return (
    harnesses.find((harness) => harness.id === 'claude_cli')?.id ??
    harnesses[0]?.id ??
    null
  )
}

function threadRuntimeLabel(
  conversation: Pick<ConversationResponse, 'ai_provider' | 'ai_model'>,
  harnesses: HarnessOption[]
): string {
  const harness = harnesses.find(
    (option) => option.id === conversation.ai_provider
  )
  const model = harness?.models.find(
    (option) => option.id === conversation.ai_model
  )
  return `${harness?.name ?? conversation.ai_provider} · ${
    model?.name ?? conversation.ai_model
  }`
}

export function AiFirstWorkspace() {
  const [searchParams, setSearchParams] = useSearchParams()
  const applicationFromUrl = searchParams.get('application')
  const threadFromUrl = searchParams.get('thread')
  const globalScopeFromUrl = searchParams.get('scope') === 'global'
  const [applications, setApplications] = useState<ApplicationResponse[]>([])
  const [activeApplicationId, setActiveApplicationId] = useState<string | null>(
    applicationFromUrl
  )
  const [conversations, setConversations] = useState<ConversationResponse[]>([])
  const [globalConversations, setGlobalConversations] = useState<
    GlobalConversationResponse[]
  >([])
  const [archivedConversations, setArchivedConversations] = useState<
    ConversationResponse[]
  >([])
  const [archivedGlobalConversations, setArchivedGlobalConversations] =
    useState<GlobalConversationResponse[]>([])
  const [threadListMode, setThreadListMode] = useState<ThreadListMode>('active')
  const [threadActionPending, setThreadActionPending] = useState<string | null>(
    null
  )
  const [threadActionError, setThreadActionError] = useState<string | null>(
    null
  )
  const [activeConversationId, setActiveConversationId] = useState<
    string | null
  >(null)
  const [artifacts, setArtifacts] = useState<ThreadArtifactResponse[]>([])
  const [harnesses, setHarnesses] = useState<HarnessOption[]>([])
  const [harnessesLoading, setHarnessesLoading] = useState(true)
  const [activeWorkspaceStatus, setActiveWorkspaceStatus] =
    useState<ApplicationWorkspaceResponse | null>(null)
  const [activeWorkspaceStatusLoading, setActiveWorkspaceStatusLoading] =
    useState(true)
  const [activeWorkspaceWaking, setActiveWorkspaceWaking] = useState(false)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [applicationDialogOpen, setApplicationDialogOpen] = useState(false)
  const [threadDialogOpen, setThreadDialogOpen] = useState(false)
  const [globalStartOpen, setGlobalStartOpen] = useState(false)
  const [rightView, setRightView] = useState<RightView>('generated')
  const [workspaceChanges, setWorkspaceChanges] =
    useState<ApplicationWorkspaceChangesResponse | null>(null)
  const [workspaceLoading, setWorkspaceLoading] = useState(false)
  const [workspaceLoadPhase, setWorkspaceLoadPhase] =
    useState<WorkspaceLoadPhase>('idle')
  const [workspaceFileCursor, setWorkspaceFileCursor] = useState(0)
  const [workspaceError, setWorkspaceError] = useState<string | null>(null)
  const [selectedWorkspacePath, setSelectedWorkspacePath] = useState<
    string | null
  >(null)
  const [workspaceDiff, setWorkspaceDiff] =
    useState<ApplicationWorkspaceDiffResponse | null>(null)
  const [workspaceDiffLoading, setWorkspaceDiffLoading] = useState(false)
  const artifactRequestGeneration = useRef(0)
  const workspaceRequestGeneration = useRef(0)
  const workspaceAbortController = useRef<AbortController | null>(null)
  const workspaceDiffGeneration = useRef(0)
  const harnessRequestGeneration = useRef(0)
  const visibleConversationRequestGeneration = useRef(0)
  const activeWorkspaceStatusRequestGeneration = useRef(0)
  const activeWorkspaceStatusRef = useRef<ApplicationWorkspaceResponse | null>(
    null
  )
  const workspaceWakeInFlight = useRef<string | null>(null)

  const activeApplication = applications.find(
    (application) => application.public_id === activeApplicationId
  )

  const handleApplicationChange = useCallback((next: ApplicationResponse) => {
    setApplications((current) =>
      current.map((application) =>
        application.public_id === next.public_id ? next : application
      )
    )
  }, [])
  const handleWorkspaceStatusChange = useCallback(
    (workspace: ApplicationWorkspaceResponse) => {
      activeWorkspaceStatusRef.current = workspace
      setActiveWorkspaceStatus(workspace)
    },
    []
  )
  const activeConversation = conversations.find(
    (conversation) => conversation.public_id === activeConversationId
  )
  const activeGlobalConversation = globalConversations.find(
    (conversation) => conversation.public_id === activeConversationId
  )

  const loadApplications = useCallback(async () => {
    try {
      const { data } = await listApplications({ throwOnError: true })
      const next = data
      setApplications(next)
      setActiveApplicationId((current) =>
        globalScopeFromUrl
          ? null
          : current &&
              next.some((application) => application.public_id === current)
            ? current
            : applicationFromUrl &&
                next.some(
                  (application) => application.public_id === applicationFromUrl
                )
              ? applicationFromUrl
              : (next[0]?.public_id ?? null)
      )
      setError(null)
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : 'Could not load applications.'
      )
    } finally {
      setLoading(false)
    }
  }, [applicationFromUrl, globalScopeFromUrl])

  const loadGlobalConversations = useCallback(async () => {
    try {
      const { data } = await listAllConversations({ throwOnError: true })
      const next = data.filter(
        (conversation) =>
          conversation.context_type === 'global' &&
          conversation.project_id == null
      )
      setGlobalConversations(next)
      if (globalScopeFromUrl) {
        setActiveConversationId((current) =>
          current &&
          next.some((conversation) => conversation.public_id === current)
            ? current
            : threadFromUrl &&
                next.some(
                  (conversation) => conversation.public_id === threadFromUrl
                )
              ? threadFromUrl
              : (next[0]?.public_id ?? null)
        )
      }
    } catch (cause) {
      setError(
        cause instanceof Error
          ? cause.message
          : 'Could not load workspace chats.'
      )
    }
  }, [globalScopeFromUrl, threadFromUrl])

  const loadArchivedConversations = useCallback(async () => {
    setThreadActionError(null)
    try {
      if (activeApplicationId) {
        const { data } = await listApplicationConversations({
          path: { application_public_id: activeApplicationId },
          query: { status: 'archived' },
          throwOnError: true,
        })
        setArchivedConversations(data)
        return
      }
      const { data } = await listAllConversations({
        query: { status: 'archived' },
        throwOnError: true,
      })
      setArchivedGlobalConversations(
        data.filter(
          (conversation) =>
            conversation.context_type === 'global' &&
            conversation.project_id == null
        )
      )
    } catch (cause) {
      setThreadActionError(
        cause instanceof Error
          ? cause.message
          : 'Could not load archived threads.'
      )
    }
  }, [activeApplicationId])

  useEffect(() => {
    if (threadListMode !== 'archived') return
    const timer = window.setTimeout(() => void loadArchivedConversations(), 0)
    return () => window.clearTimeout(timer)
  }, [loadArchivedConversations, threadListMode])

  const refreshVisibleConversations = useCallback(async () => {
    const requestGeneration = ++visibleConversationRequestGeneration.current
    try {
      if (activeApplicationId) {
        const { data: next } = await listApplicationConversations({
          path: { application_public_id: activeApplicationId },
          throwOnError: true,
        })
        if (
          requestGeneration === visibleConversationRequestGeneration.current
        ) {
          setConversations(next)
        }
        return
      }

      const { data } = await listAllConversations({ throwOnError: true })
      if (requestGeneration !== visibleConversationRequestGeneration.current) {
        return
      }
      setGlobalConversations(
        data.filter(
          (conversation) =>
            conversation.context_type === 'global' &&
            conversation.project_id == null
        )
      )
    } catch {
      // The active WebSocket and the next refresh remain authoritative. A
      // sidebar-only refresh failure must not interrupt the open conversation.
    }
  }, [activeApplicationId])

  // Provider credentials can be saved in Agent Sandbox while this workspace
  // remains open. Re-read the inventory whenever an application/thread
  // chooser opens instead of requiring a full-page reload to see the newly
  // configured harness.
  const loadHarnesses = useCallback(async (refreshModels = false) => {
    const requestGeneration = ++harnessRequestGeneration.current
    setHarnessesLoading(true)
    try {
      const { data } = await listAiProviders({
        query: { refresh_models: refreshModels },
        throwOnError: true,
      })
      if (requestGeneration !== harnessRequestGeneration.current) return
      setHarnesses(workspaceHarnessOptions(data?.providers ?? []))
    } catch {
      // Preserve the last confirmed inventory when a refresh fails. A
      // transient provider probe must not make harnesses disappear.
    } finally {
      if (requestGeneration === harnessRequestGeneration.current) {
        setHarnessesLoading(false)
      }
    }
  }, [])

  const loadActiveWorkspaceStatus = useCallback(async () => {
    const requestGeneration = ++activeWorkspaceStatusRequestGeneration.current
    if (!activeWorkspaceStatusRef.current) {
      setActiveWorkspaceStatusLoading(true)
    }
    try {
      if (activeApplicationId) {
        const { data } = await getApplicationWorkspace({
          path: { application_public_id: activeApplicationId },
          throwOnError: true,
        })
        if (
          requestGeneration !== activeWorkspaceStatusRequestGeneration.current
        ) {
          return
        }
        activeWorkspaceStatusRef.current = data
        setActiveWorkspaceStatus(data)
        if (
          data.desired_state === 'running' &&
          data.state === 'sleeping' &&
          workspaceWakeInFlight.current !== activeApplicationId
        ) {
          workspaceWakeInFlight.current = activeApplicationId
          setActiveWorkspaceWaking(true)
          try {
            const { data: resumed } = await controlApplicationWorkspace({
              path: { application_public_id: activeApplicationId },
              body: { action: 'resume' },
              throwOnError: true,
            })
            if (
              requestGeneration !==
              activeWorkspaceStatusRequestGeneration.current
            ) {
              return
            }
            activeWorkspaceStatusRef.current = resumed
            setActiveWorkspaceStatus(resumed)
          } finally {
            if (workspaceWakeInFlight.current === activeApplicationId) {
              workspaceWakeInFlight.current = null
              setActiveWorkspaceWaking(false)
            }
          }
        }
      } else {
        const { data } = await getGlobalAiWorkspace({ throwOnError: true })
        if (
          requestGeneration !== activeWorkspaceStatusRequestGeneration.current
        ) {
          return
        }
        activeWorkspaceStatusRef.current = data
        setActiveWorkspaceStatus(data)
      }
    } catch {
      if (!activeWorkspaceStatusRef.current) {
        setActiveWorkspaceStatus(null)
      }
    } finally {
      setActiveWorkspaceStatusLoading(false)
    }
  }, [activeApplicationId])

  useEffect(() => {
    const resetTimer = window.setTimeout(() => {
      activeWorkspaceStatusRef.current = null
      activeWorkspaceStatusRequestGeneration.current += 1
      workspaceWakeInFlight.current = null
      setActiveWorkspaceStatus(null)
      setActiveWorkspaceStatusLoading(true)
      setActiveWorkspaceWaking(false)
    }, 0)
    return () => window.clearTimeout(resetTimer)
  }, [activeApplicationId])

  useEffect(() => {
    const loadTimer = window.setTimeout(() => void loadApplications(), 0)
    const globalLoadTimer = window.setTimeout(
      () => void loadGlobalConversations(),
      0
    )
    const harnessLoadTimer = window.setTimeout(() => void loadHarnesses(), 0)
    return () => {
      window.clearTimeout(loadTimer)
      window.clearTimeout(globalLoadTimer)
      window.clearTimeout(harnessLoadTimer)
    }
  }, [loadApplications, loadGlobalConversations, loadHarnesses])

  useEffect(() => {
    const initial = window.setTimeout(() => void loadActiveWorkspaceStatus(), 0)
    const interval = window.setInterval(
      () => void loadActiveWorkspaceStatus(),
      5_000
    )
    return () => {
      window.clearTimeout(initial)
      window.clearInterval(interval)
    }
  }, [loadActiveWorkspaceStatus])

  useEffect(() => {
    if (!activeApplicationId) {
      return
    }
    let cancelled = false
    listApplicationConversations({
      path: { application_public_id: activeApplicationId },
      throwOnError: true,
    })
      .then(({ data: next }) => {
        if (cancelled) return
        setConversations(next)
        setActiveConversationId((current) =>
          current &&
          next.some((conversation) => conversation.public_id === current)
            ? current
            : initialApplicationThreadId(next, threadFromUrl)
        )
      })
      .catch((cause) => {
        if (!cancelled)
          setError(
            cause instanceof Error ? cause.message : 'Could not load threads.'
          )
      })
    return () => {
      cancelled = true
    }
  }, [activeApplicationId, threadFromUrl])

  const visibleConversations =
    threadListMode === 'archived'
      ? activeApplication
        ? archivedConversations
        : archivedGlobalConversations
      : activeApplication
        ? conversations
        : globalConversations
  const hasPendingConversation = (
    activeApplication ? conversations : globalConversations
  ).some((conversation) => conversation.turn_status === 'running')

  // A different tab or thread may start work in the same workspace. Poll only
  // while at least one row is active, then stop immediately at a terminal
  // state so idle workspaces create no background request traffic.
  useEffect(() => {
    if (!hasPendingConversation) return
    const refreshTimer = window.setInterval(
      () => void refreshVisibleConversations(),
      2000
    )
    return () => window.clearInterval(refreshTimer)
  }, [hasPendingConversation, refreshVisibleConversations])

  const refreshArtifacts = useCallback(async () => {
    const requestGeneration = ++artifactRequestGeneration.current
    if (!activeApplicationId || !activeConversationId) {
      setArtifacts([])
      return
    }
    try {
      const { data: next } = await listThreadArtifacts({
        path: {
          application_public_id: activeApplicationId,
          conversation_public_id: activeConversationId,
        },
        throwOnError: true,
      })
      if (requestGeneration === artifactRequestGeneration.current) {
        setArtifacts(next)
      }
    } catch {
      // Chat remains useful if an event-driven artifact refresh fails.
    }
  }, [activeApplicationId, activeConversationId])

  useEffect(() => {
    const refreshTimer = window.setTimeout(() => void refreshArtifacts(), 0)
    return () => {
      window.clearTimeout(refreshTimer)
      artifactRequestGeneration.current += 1
    }
  }, [refreshArtifacts])

  const loadWorkspacePage = useCallback(
    async (cursor: number) => {
      const requestGeneration = ++workspaceRequestGeneration.current
      workspaceAbortController.current?.abort()
      if (!activeApplicationId) {
        setWorkspaceChanges(null)
        setSelectedWorkspacePath(null)
        setWorkspaceFileCursor(0)
        setWorkspaceLoading(false)
        setWorkspaceLoadPhase('idle')
        return
      }
      const controller = new AbortController()
      workspaceAbortController.current = controller
      let clientTimedOut = false
      const clientTimeout = window.setTimeout(() => {
        clientTimedOut = true
        controller.abort()
      }, WORKSPACE_CLIENT_TIMEOUT_MS)
      setWorkspaceLoading(true)
      setWorkspaceLoadPhase('checking')
      setWorkspaceError(null)
      try {
        const { data: workspace } = await getApplicationWorkspace({
          path: { application_public_id: activeApplicationId },
          signal: controller.signal,
          throwOnError: true,
        })
        if (requestGeneration !== workspaceRequestGeneration.current) return
        if (workspace.desired_state !== 'running') {
          setWorkspaceError(
            workspace.desired_state === 'quarantined'
              ? 'This persistent workspace is quarantined because project access changed. Restore access, then resume it from Workspace settings.'
              : 'This persistent workspace is paused. Resume it from Workspace settings to inspect its files.'
          )
          return
        }
        setWorkspaceLoadPhase(workspaceLoadPhaseFor(workspace))
        const { data: payload } = await getApplicationWorkspaceChanges({
          path: { application_public_id: activeApplicationId },
          query: { cursor, limit: WORKSPACE_FILES_PAGE_SIZE },
          signal: controller.signal,
          throwOnError: true,
        })
        if (requestGeneration !== workspaceRequestGeneration.current) return
        setWorkspaceChanges(payload)
        setWorkspaceFileCursor(cursor)
        setSelectedWorkspacePath((current) =>
          current && payload.changes.some((change) => change.path === current)
            ? current
            : (payload.changes[0]?.path ?? null)
        )
      } catch (cause) {
        if (requestGeneration !== workspaceRequestGeneration.current) return
        setWorkspaceError(
          clientTimedOut
            ? 'The persistent workspace did not become ready within 35 seconds. Its files are still safe; retry or inspect recovery from Workspace settings.'
            : cause instanceof Error
              ? cause.message
              : 'Could not inspect workspace files.'
        )
      } finally {
        window.clearTimeout(clientTimeout)
        if (requestGeneration === workspaceRequestGeneration.current) {
          setWorkspaceLoading(false)
          setWorkspaceLoadPhase('idle')
          workspaceAbortController.current = null
        }
      }
    },
    [activeApplicationId]
  )

  const refreshWorkspace = useCallback(
    () => loadWorkspacePage(0),
    [loadWorkspacePage]
  )

  useEffect(() => {
    if (rightView !== 'files' || !activeApplicationId) return
    const refreshTimer = window.setTimeout(() => void refreshWorkspace(), 0)
    return () => {
      window.clearTimeout(refreshTimer)
      workspaceRequestGeneration.current += 1
      workspaceAbortController.current?.abort()
      workspaceAbortController.current = null
      setWorkspaceLoading(false)
      setWorkspaceLoadPhase('idle')
    }
  }, [activeApplicationId, refreshWorkspace, rightView])

  useEffect(() => {
    const requestGeneration = ++workspaceDiffGeneration.current
    if (!activeApplicationId || !selectedWorkspacePath) return
    const loadTimer = window.setTimeout(() => {
      setWorkspaceDiff(null)
      setWorkspaceDiffLoading(true)
      getApplicationWorkspaceDiff({
        path: { application_public_id: activeApplicationId },
        query: { path: selectedWorkspacePath },
        throwOnError: true,
      })
        .then(({ data: payload }) => {
          if (requestGeneration === workspaceDiffGeneration.current) {
            setWorkspaceDiff(payload)
          }
        })
        .catch((cause: unknown) => {
          if (
            requestGeneration === workspaceDiffGeneration.current &&
            cause != null
          ) {
            setWorkspaceError(
              cause instanceof Error
                ? cause.message
                : 'Could not load this diff.'
            )
          }
        })
        .finally(() => {
          if (requestGeneration === workspaceDiffGeneration.current) {
            setWorkspaceDiffLoading(false)
          }
        })
    }, 0)
    return () => {
      window.clearTimeout(loadTimer)
    }
  }, [activeApplicationId, selectedWorkspacePath])

  const handleChatLiveEvent = useCallback(
    (eventName: string, data: string) => {
      const title = threadTitleFromLiveEvent(eventName, data)
      if (title && activeConversationId) {
        setConversations((current) =>
          current.map((conversation) =>
            conversation.public_id === activeConversationId
              ? { ...conversation, title }
              : conversation
          )
        )
        setGlobalConversations((current) =>
          current.map((conversation) =>
            conversation.public_id === activeConversationId
              ? { ...conversation, title }
              : conversation
          )
        )
      }
      if (shouldRefreshArtifactsForLiveEvent(eventName)) {
        void refreshArtifacts()
      }
      let turnStatus: string | null = null
      if (eventName === 'user_message') turnStatus = 'running'
      if (eventName === 'error') turnStatus = 'failed'
      if (eventName === 'turn_state') {
        try {
          const state = JSON.parse(data) as { status?: string }
          turnStatus = state.status ?? null
        } catch {
          // The next server snapshot will reconcile malformed live data.
        }
      }
      if (turnStatus && activeConversationId) {
        setConversations((current) =>
          current.map((conversation) =>
            conversation.public_id === activeConversationId
              ? { ...conversation, turn_status: turnStatus }
              : conversation
          )
        )
        setGlobalConversations((current) =>
          current.map((conversation) =>
            conversation.public_id === activeConversationId
              ? { ...conversation, turn_status: turnStatus }
              : conversation
          )
        )
      }
      if (eventName === 'tool_result' || eventName === 'turn_complete') {
        // The server owns application topology. Refresh after every completed
        // tool result so a create/link/unlink operation performed through chat
        // appears in the Projects view without waiting for a page reload.
        void loadApplications()
      }
      if (eventName === 'turn_complete') {
        // Completion can represent success or failure. Re-read the persisted
        // terminal status instead of optimistically painting every turn green.
        void refreshVisibleConversations()
      }
      if (eventName === 'turn_complete' && rightView === 'files') {
        void refreshWorkspace()
      }
    },
    [
      activeConversationId,
      loadApplications,
      refreshArtifacts,
      refreshVisibleConversations,
      refreshWorkspace,
      rightView,
    ]
  )

  const handleApplicationCreated = (
    application: ApplicationResponse,
    conversation: ConversationResponse
  ) => {
    setApplications((current) => [application, ...current])
    setActiveApplicationId(application.public_id)
    setConversations([conversation])
    setActiveConversationId(conversation.public_id)
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.set('application', application.public_id)
        next.set('thread', conversation.public_id)
        return next
      },
      { replace: true }
    )
    setApplicationDialogOpen(false)
  }

  const selectApplication = (applicationId: string) => {
    setGlobalStartOpen(false)
    setApplicationDialogOpen(false)
    setThreadDialogOpen(false)
    setActiveApplicationId(applicationId)
    setConversations([])
    setActiveConversationId(null)
    setArtifacts([])
    setWorkspaceChanges(null)
    setSelectedWorkspacePath(null)
    setWorkspaceDiff(null)
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.set('application', applicationId)
        next.delete('scope')
        next.delete('thread')
        return next
      },
      { replace: true }
    )
  }

  const selectGlobalConversation = (conversationId: string) => {
    setGlobalStartOpen(false)
    setApplicationDialogOpen(false)
    setThreadDialogOpen(false)
    setActiveApplicationId(null)
    setConversations([])
    setArtifacts([])
    setRightView('generated')
    setWorkspaceChanges(null)
    setActiveConversationId(conversationId)
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete('application')
        next.set('scope', 'global')
        next.set('thread', conversationId)
        return next
      },
      { replace: true }
    )
  }

  const handleGlobalThreadCreated = (conversation: ConversationResponse) => {
    const globalConversation: GlobalConversationResponse = {
      ai_model: conversation.ai_model,
      ai_permission_mode: conversation.ai_permission_mode,
      ai_provider: conversation.ai_provider,
      ai_thinking_level: conversation.ai_thinking_level,
      context_id: conversation.context_id,
      context_type: conversation.context_type,
      created_at: conversation.created_at,
      last_activity_at: conversation.last_activity_at,
      project_id: null,
      project_name: null,
      project_slug: null,
      public_id: conversation.public_id,
      status: conversation.status,
      title: conversation.title,
      turn_status: conversation.turn_status,
    }
    setGlobalConversations((current) => [globalConversation, ...current])
    setGlobalStartOpen(false)
    selectGlobalConversation(conversation.public_id)
  }

  const handleThreadCreated = (conversation: ConversationResponse) => {
    setConversations((current) => [conversation, ...current])
    setActiveConversationId(conversation.public_id)
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.set('thread', conversation.public_id)
        return next
      },
      { replace: true }
    )
    setThreadDialogOpen(false)
  }

  const openApplicationDialog = () => {
    void loadHarnesses()
    setApplicationDialogOpen(true)
  }

  const openThreadDialog = () => {
    void loadHarnesses()
    setThreadDialogOpen(true)
  }

  const openGlobalStart = () => {
    void loadHarnesses()
    const selection = defaultWorkspaceSelection([])
    setActiveApplicationId(selection.applicationId)
    setConversations([])
    setArtifacts([])
    setRightView('generated')
    setWorkspaceChanges(null)
    setSelectedWorkspacePath(null)
    setWorkspaceDiff(null)
    setActiveConversationId(selection.conversationId)
    setApplicationDialogOpen(false)
    setThreadDialogOpen(false)
    setGlobalStartOpen(true)
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete('application')
        next.set('scope', 'global')
        next.delete('thread')
        return next
      },
      { replace: true }
    )
  }

  const selectDefaultWorkspace = () => {
    const selection = defaultWorkspaceSelection(
      globalConversations.map((conversation) => conversation.public_id)
    )
    if (selection.conversationId) {
      selectGlobalConversation(selection.conversationId)
      return
    }
    openGlobalStart()
  }

  const selectApplicationConversation = (conversationId: string) => {
    setActiveConversationId(conversationId)
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.set('thread', conversationId)
        return next
      },
      { replace: true }
    )
  }

  const handleArchiveThread = async (
    conversation: ConversationResponse | GlobalConversationResponse
  ) => {
    if (conversation.turn_status === 'running') {
      setThreadActionError('Stop this thread before archiving it.')
      return
    }
    setThreadActionPending(conversation.public_id)
    setThreadActionError(null)
    try {
      await archiveUserConversation({
        path: { public_id: conversation.public_id },
        throwOnError: true,
      })
      if (activeApplication) {
        const remaining = conversations.filter(
          (item) => item.public_id !== conversation.public_id
        )
        setConversations(remaining)
        setArchivedConversations((current) => [
          { ...conversation, status: 'archived' },
          ...current,
        ])
        if (activeConversationId === conversation.public_id) {
          const nextId = threadSelectionAfterRemoval(
            conversations.map((item) => item.public_id),
            activeConversationId,
            conversation.public_id
          )
          setActiveConversationId(nextId)
          setSearchParams(
            (current) => {
              const next = new URLSearchParams(current)
              if (nextId) next.set('thread', nextId)
              else next.delete('thread')
              return next
            },
            { replace: true }
          )
        }
      } else {
        const remaining = globalConversations.filter(
          (item) => item.public_id !== conversation.public_id
        )
        setGlobalConversations(remaining)
        setArchivedGlobalConversations((current) => [
          { ...conversation, status: 'archived' },
          ...current,
        ])
        if (activeConversationId === conversation.public_id) {
          const nextId = threadSelectionAfterRemoval(
            globalConversations.map((item) => item.public_id),
            activeConversationId,
            conversation.public_id
          )
          setActiveConversationId(nextId)
          setSearchParams(
            (current) => {
              const next = new URLSearchParams(current)
              if (nextId) next.set('thread', nextId)
              else next.delete('thread')
              return next
            },
            { replace: true }
          )
        }
      }
    } catch (cause) {
      setThreadActionError(
        cause instanceof Error ? cause.message : 'Could not archive the thread.'
      )
    } finally {
      setThreadActionPending(null)
    }
  }

  const handleRestoreThread = async (
    conversation: ConversationResponse | GlobalConversationResponse
  ) => {
    setThreadActionPending(conversation.public_id)
    setThreadActionError(null)
    try {
      await restoreUserConversation({
        path: { public_id: conversation.public_id },
        throwOnError: true,
      })
      if (activeApplication) {
        setArchivedConversations((current) =>
          current.filter((item) => item.public_id !== conversation.public_id)
        )
        setConversations((current) => [
          { ...conversation, status: 'active' },
          ...current,
        ])
      } else {
        setArchivedGlobalConversations((current) =>
          current.filter((item) => item.public_id !== conversation.public_id)
        )
        setGlobalConversations((current) => [
          { ...conversation, status: 'active' },
          ...current,
        ])
      }
    } catch (cause) {
      setThreadActionError(
        cause instanceof Error ? cause.message : 'Could not restore the thread.'
      )
    } finally {
      setThreadActionPending(null)
    }
  }

  return (
    <div className="fixed inset-0 z-40 overflow-hidden bg-background text-foreground antialiased">
      <header className="flex h-14 items-center justify-between border-b border-border bg-card px-4">
        <div className="flex items-center gap-3">
          <div className="flex size-7 items-center justify-center rounded-md bg-primary text-sm font-semibold text-primary-foreground">
            T
          </div>
          <span className="text-sm font-semibold">Temps</span>
          <span className="rounded-full border border-border px-2 py-0.5 font-mono text-[10px] tracking-wide text-muted-foreground">
            AI workspace
          </span>
        </div>
        <div className="flex items-center gap-2">
          <WorkspaceStatusIndicator
            loading={activeWorkspaceStatusLoading}
            onClick={
              activeApplication ? () => setRightView('workspace') : undefined
            }
            waking={activeWorkspaceWaking}
            workspace={activeWorkspaceStatus}
          />
          <Button asChild variant="ghost" size="sm">
            <Link to="/agent-sandbox/providers">
              <Terminal className="mr-1.5 size-4" /> Harnesses
              <span
                aria-label={
                  harnessesLoading
                    ? 'Harness readiness is being checked'
                    : harnesses.length > 0
                      ? `${harnesses.length} workspace harnesses ready`
                      : 'No workspace harnesses ready'
                }
                className={cn(
                  'ml-2 size-2 rounded-full',
                  harnessesLoading
                    ? 'animate-pulse bg-amber-500'
                    : harnesses.length > 0
                      ? 'bg-emerald-500'
                      : 'bg-red-500'
                )}
              />
            </Link>
          </Button>
          <Button
            asChild
            variant="ghost"
            size="sm"
            className="text-muted-foreground"
          >
            <a href="/projects">
              <X className="mr-1.5 size-4" /> Classic console
            </a>
          </Button>
        </div>
      </header>

      <div className="grid h-[calc(100dvh-3.5rem)] grid-cols-1 md:grid-cols-[220px_minmax(0,1fr)] xl:grid-cols-[240px_minmax(0,1fr)_420px]">
        <aside className="hidden min-h-0 border-r border-border bg-card md:block">
          <div className="flex items-center justify-between border-b border-border px-3 py-3">
            <span className="font-mono text-[10px] font-semibold tracking-wide text-muted-foreground">
              Workspaces
            </span>
            <button
              type="button"
              onClick={openApplicationDialog}
              className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
              aria-label="New workspace"
            >
              <Plus className="size-4" />
            </button>
          </div>
          <div className="space-y-1 p-2">
            <button
              type="button"
              onClick={selectDefaultWorkspace}
              className={cn(
                'w-full rounded-md px-3 py-2 text-left',
                !activeApplicationId
                  ? 'bg-accent text-accent-foreground'
                  : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
              )}
            >
              <p className="truncate text-sm">Default workspace</p>
              <p className="mt-0.5 text-[10px]">
                {globalConversations.length} thread
                {globalConversations.length === 1 ? '' : 's'} · persistent
              </p>
            </button>
            {applications.map((application) => (
              <button
                key={application.public_id}
                type="button"
                onClick={() => selectApplication(application.public_id)}
                className={cn(
                  'w-full rounded-md px-3 py-2 text-left',
                  application.public_id === activeApplicationId
                    ? 'bg-accent text-accent-foreground'
                    : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                )}
              >
                <p className="truncate text-sm">{application.name}</p>
                <p className="mt-0.5 text-[10px]">
                  {application.projects.length} project
                  {application.projects.length === 1 ? '' : 's'}
                </p>
              </button>
            ))}
          </div>
          <>
            <div className="mx-3 my-2 border-t border-border" />
            <div className="flex items-center justify-between px-3 py-2">
              <span className="font-mono text-[10px] font-semibold tracking-wide text-muted-foreground">
                {threadListMode === 'archived' ? 'Archived' : 'Threads'}
              </span>
              <span className="flex items-center gap-1">
                <button
                  type="button"
                  onClick={() => {
                    setThreadActionError(null)
                    setThreadListMode((current) =>
                      current === 'active' ? 'archived' : 'active'
                    )
                  }}
                  className={cn(
                    'rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground',
                    threadListMode === 'archived' &&
                      'bg-accent text-accent-foreground'
                  )}
                  aria-label={
                    threadListMode === 'archived'
                      ? 'Show active threads'
                      : 'Show archived threads'
                  }
                  title={
                    threadListMode === 'archived'
                      ? 'Back to active threads'
                      : 'Archived threads'
                  }
                >
                  {threadListMode === 'archived' ? (
                    <ArchiveRestore className="size-4" />
                  ) : (
                    <Archive className="size-4" />
                  )}
                </button>
                {threadListMode === 'active' && (
                  <button
                    type="button"
                    onClick={
                      activeApplication ? openThreadDialog : openGlobalStart
                    }
                    className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                    aria-label="New thread"
                  >
                    <Plus className="size-4" />
                  </button>
                )}
              </span>
            </div>
            {threadActionError && (
              <p className="mx-3 mb-2 rounded border border-destructive/30 bg-destructive/5 px-2 py-1.5 text-[10px] leading-4 text-destructive">
                {threadActionError}
              </p>
            )}
            <div className="space-y-1 px-2">
              {visibleConversations.map((conversation) => (
                <div
                  className={cn(
                    'group flex items-stretch rounded-md',
                    conversation.public_id === activeConversationId &&
                      threadListMode === 'active'
                      ? 'bg-accent text-accent-foreground'
                      : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                  )}
                  key={conversation.public_id}
                >
                  <button
                    className="min-w-0 flex-1 px-3 py-2 text-left"
                    disabled={threadListMode === 'archived'}
                    onClick={() =>
                      activeApplication
                        ? selectApplicationConversation(conversation.public_id)
                        : selectGlobalConversation(conversation.public_id)
                    }
                    type="button"
                  >
                    <span className="flex min-w-0 items-center justify-between gap-2">
                      <span className="truncate text-xs">
                        {conversation.title ?? 'Workspace thread'}
                      </span>
                      {threadListMode === 'active' && (
                        <ThreadStatusIndicator
                          status={threadDisplayStatus(
                            conversation.turn_status,
                            conversation.last_activity_at !==
                              conversation.created_at
                          )}
                        />
                      )}
                    </span>
                    <span className="mt-1 flex min-w-0 items-center gap-1.5 text-[10px]">
                      <AiHarnessLogo
                        providerId={conversation.ai_provider}
                        size={16}
                      />
                      <span className="truncate">
                        {threadRuntimeLabel(conversation, harnesses)}
                      </span>
                    </span>
                  </button>
                  <button
                    aria-label={`${threadListMode === 'archived' ? 'Restore' : 'Archive'} ${conversation.title ?? 'thread'}`}
                    className="w-8 shrink-0 rounded-r-md text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus:opacity-100 group-hover:opacity-100 disabled:cursor-wait"
                    disabled={threadActionPending === conversation.public_id}
                    onClick={() =>
                      void (threadListMode === 'archived'
                        ? handleRestoreThread(conversation)
                        : handleArchiveThread(conversation))
                    }
                    title={
                      threadListMode === 'archived'
                        ? 'Restore thread'
                        : conversation.turn_status === 'running'
                          ? 'Stop this thread before archiving'
                          : 'Archive thread'
                    }
                    type="button"
                  >
                    {threadActionPending === conversation.public_id ? (
                      <Loader2 className="mx-auto size-3.5 animate-spin" />
                    ) : threadListMode === 'archived' ? (
                      <ArchiveRestore className="mx-auto size-3.5" />
                    ) : (
                      <Archive className="mx-auto size-3.5" />
                    )}
                  </button>
                </div>
              ))}
              {visibleConversations.length === 0 && (
                <p className="px-3 py-5 text-center text-[10px] leading-4 text-muted-foreground">
                  {threadListMode === 'archived'
                    ? 'No archived threads.'
                    : 'No threads yet.'}
                </p>
              )}
            </div>
          </>
        </aside>

        <main className="min-h-0 min-w-0">
          {globalStartOpen ? (
            <GlobalChatStartScreen
              onCancel={() => setGlobalStartOpen(false)}
              onCreated={handleGlobalThreadCreated}
              harnesses={harnesses}
              harnessesLoading={harnessesLoading}
            />
          ) : applicationDialogOpen ? (
            <ApplicationStartScreen
              onCancel={() => setApplicationDialogOpen(false)}
              onCreated={handleApplicationCreated}
              harnesses={harnesses}
              harnessesLoading={harnessesLoading}
            />
          ) : loading ? (
            <CenteredMessage
              icon={Loader2}
              spin
              title="Loading AI workspace…"
            />
          ) : error &&
            applications.length === 0 &&
            globalConversations.length === 0 ? (
            <CenteredMessage
              icon={RefreshCw}
              title="The AI workspace API is unavailable"
              detail={error}
              action="Try again"
              onAction={() => void loadApplications()}
            />
          ) : activeGlobalConversation ? (
            <div className="flex h-full min-h-0 flex-col">
              <div className="border-b border-border px-5 py-3">
                <p className="text-sm font-medium">
                  {activeGlobalConversation.title ?? 'Temps workspace'}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  All accessible projects ·{' '}
                  {threadRuntimeLabel(activeGlobalConversation, harnesses)} ·
                  user-owned · current-role authorization
                </p>
              </div>
              <div className="min-h-0 flex-1">
                <DebugChatPanel
                  key={activeGlobalConversation.public_id}
                  conversationPublicId={activeGlobalConversation.public_id}
                  userScoped
                  contextType="global"
                  contextId={activeGlobalConversation.context_id}
                  emptyHint="Ask about any project or platform resource you can access. Temps will gather evidence before proposing changes."
                  placeholder="Ask Temps to inspect or operate your workspace…"
                  onLiveEvent={handleChatLiveEvent}
                  onConversationStatusInvalidated={refreshVisibleConversations}
                />
              </div>
            </div>
          ) : globalScopeFromUrl ? (
            <CenteredMessage
              icon={Sparkles}
              title="Start a thread in Default workspace"
              detail="This persistent workspace can operate every platform resource allowed by your current role."
              action="New thread"
              onAction={openGlobalStart}
            />
          ) : !activeApplication ? (
            <CenteredMessage
              icon={Sparkles}
              title="Build and operate through chat"
              detail="Create a persistent workspace, then ask the assistant to build files or operate any platform resource your role can access."
              action="Create workspace"
              onAction={openApplicationDialog}
            />
          ) : !activeConversation ? (
            <CenteredMessage
              icon={Code2}
              title={`Start a thread for ${activeApplication.name}`}
              detail="All threads share this workspace's persistent sandbox and files. Platform operations use your current role and native harness approval mode."
              action="New thread"
              onAction={openThreadDialog}
            />
          ) : (
            <div className="flex h-full min-h-0 flex-col">
              <div className="border-b border-border px-5 py-3">
                <p className="text-sm font-medium">
                  {activeConversation.title ?? activeApplication.name}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {activeApplication.projects.length} linked projects ·{' '}
                  {threadRuntimeLabel(activeConversation, harnesses)} ·
                  user-owned · current-role authorization
                </p>
              </div>
              <div className="min-h-0 flex-1">
                <DebugChatPanel
                  key={activeConversation.public_id}
                  conversationPublicId={activeConversation.public_id}
                  userScoped
                  contextType="application"
                  contextId={activeConversation.context_id}
                  emptyHint="Describe what you want to build or operate from this workspace."
                  placeholder="Tell Temps what you want to ship…"
                  onLiveEvent={handleChatLiveEvent}
                  onConversationStatusInvalidated={refreshVisibleConversations}
                />
              </div>
            </div>
          )}
        </main>

        <aside className="hidden min-h-0 flex-col border-l border-border bg-card xl:flex">
          <WorkspaceViewTabs
            activeView={rightView}
            changedFileCount={workspaceChanges?.changes.length ?? 0}
            hasApplication={Boolean(activeApplication)}
            onChange={setRightView}
          />
          <div className="min-h-0 flex-1 overflow-y-auto p-4">
            {rightView === 'preview' && activeApplication ? (
              <ApplicationPreviewPanel
                applicationPublicId={activeApplication.public_id}
                key={activeApplication.public_id}
              />
            ) : rightView === 'files' && activeApplication ? (
              <WorkspaceFilesPanel
                changes={workspaceChanges}
                diff={workspaceDiff}
                diffLoading={workspaceDiffLoading}
                error={workspaceError}
                loading={workspaceLoading}
                loadPhase={workspaceLoadPhase}
                fileCursor={workspaceFileCursor}
                onNextPage={() => {
                  if (workspaceChanges?.next_cursor != null) {
                    void loadWorkspacePage(workspaceChanges.next_cursor)
                  }
                }}
                onPreviousPage={() =>
                  void loadWorkspacePage(
                    Math.max(0, workspaceFileCursor - WORKSPACE_FILES_PAGE_SIZE)
                  )
                }
                onRefresh={() => void refreshWorkspace()}
                onOpenSettings={() => setRightView('workspace')}
                onSelect={setSelectedWorkspacePath}
                selectedPath={selectedWorkspacePath}
              />
            ) : rightView === 'projects' && activeApplication ? (
              <ApplicationProjectsPanel
                application={activeApplication}
                onApplicationChange={handleApplicationChange}
              />
            ) : rightView === 'workspace' && activeApplication ? (
              <ApplicationWorkspaceSettingsPanel
                applicationPublicId={activeApplication.public_id}
                initialWorkspace={activeWorkspaceStatus}
                key={activeApplication.public_id}
                onWorkspaceChange={handleWorkspaceStatusChange}
                waking={activeWorkspaceWaking}
              />
            ) : (
              <div className="space-y-3">
                <div className="mb-4 flex items-center gap-2">
                  <Boxes className="size-4 stroke-success" />
                  <div>
                    <p className="text-xs font-medium">Generated view</p>
                    <p className="text-[10px] text-muted-foreground">
                      Typed artifacts, never executable UI
                    </p>
                  </div>
                </div>
                {artifacts.map((artifact) => (
                  <ArtifactRenderer
                    key={artifact.public_id}
                    artifact={artifact}
                  />
                ))}
                {activeApplication && (
                  <ApplicationBoundary application={activeApplication} />
                )}
                {activeGlobalConversation && <GlobalChatBoundary />}
              </div>
            )}
          </div>
        </aside>
      </div>

      {activeApplication && (
        <CreateThreadDialog
          application={activeApplication}
          harnesses={harnesses}
          harnessesLoading={harnessesLoading}
          open={threadDialogOpen}
          onOpenChange={setThreadDialogOpen}
          onCreated={handleThreadCreated}
        />
      )}
    </div>
  )
}

export function ThreadStatusIndicator({
  status,
}: {
  status: ThreadDisplayStatus
}) {
  const presentation = {
    pending: {
      label: 'Pending',
      dot: 'bg-amber-500',
      text: 'text-amber-700 dark:text-amber-400',
    },
    error: {
      label: 'Error',
      dot: 'bg-destructive',
      text: 'text-destructive',
    },
    succeeded: {
      label: 'Succeeded',
      dot: 'bg-emerald-500',
      text: 'text-emerald-700 dark:text-emerald-400',
    },
  }[status]

  return (
    <span
      className={cn(
        'inline-flex shrink-0 items-center gap-1 text-[9px] font-medium',
        presentation.text
      )}
      aria-label={`Thread status: ${presentation.label}`}
    >
      <span
        aria-hidden="true"
        className={cn(
          'size-1.5 rounded-full',
          presentation.dot,
          status === 'pending' && 'animate-pulse'
        )}
      />
      {presentation.label}
    </span>
  )
}

export function WorkspaceStatusIndicator({
  loading,
  onClick,
  waking = false,
  workspace,
}: {
  loading: boolean
  onClick?: () => void
  waking?: boolean
  workspace: ApplicationWorkspaceResponse | null
}) {
  const presentation = workspaceStatusPresentation(workspace, loading, waking)
  return (
    <button
      aria-label={`${presentation.label}: ${presentation.detail}`}
      className={cn(
        'flex items-center gap-2 rounded-md border border-border px-2.5 py-1.5 text-xs',
        onClick && 'hover:bg-accent'
      )}
      disabled={!onClick}
      onClick={onClick}
      title={presentation.detail}
      type="button"
    >
      <span
        aria-hidden="true"
        className={cn('size-2 rounded-full', presentation.dot)}
      />
      <span>{presentation.label}</span>
      {workspace?.sandbox_public_id && (
        <span className="hidden max-w-28 truncate font-mono text-[10px] text-muted-foreground lg:inline">
          {workspace.sandbox_public_id}
        </span>
      )}
    </button>
  )
}

function workspaceLoadPhaseFor(
  workspace: ApplicationWorkspaceResponse
): WorkspaceLoadPhase {
  if (workspace.state === 'sleeping') return 'waking'
  if (workspace.state === 'recovering' || workspace.state === 'failed') {
    return 'recovering'
  }
  return 'inspecting'
}

function workspaceLoadMessage(phase: WorkspaceLoadPhase): string {
  switch (phase) {
    case 'checking':
      return 'Checking workspace state…'
    case 'waking':
      return 'Waking the persistent workspace…'
    case 'recovering':
      return 'Recovering the persistent workspace…'
    default:
      return 'Inspecting Git in the persistent workspace…'
  }
}

export function WorkspaceViewTabs({
  activeView,
  changedFileCount,
  hasApplication,
  onChange,
}: {
  activeView: RightView
  changedFileCount: number
  hasApplication: boolean
  onChange: (view: RightView) => void
}) {
  const tabs: Array<{
    view: RightView
    label: string
    icon: typeof Boxes
    count?: number
  }> = [
    { view: 'generated', label: 'Output', icon: Boxes },
    ...(hasApplication
      ? [
          { view: 'preview' as const, label: 'Preview', icon: MonitorPlay },
          {
            view: 'files' as const,
            label: 'Files',
            icon: FolderTree,
            count: changedFileCount,
          },
        ]
      : []),
  ]

  return (
    <div
      aria-label="Application workspace views"
      className={cn(
        'grid h-14 shrink-0 border-b border-border bg-card px-1',
        hasApplication ? 'grid-cols-3' : 'grid-cols-1'
      )}
      role="tablist"
    >
      {tabs.map((tab) => {
        const Icon = tab.icon
        const selected = activeView === tab.view
        return (
          <button
            aria-selected={selected}
            className={cn(
              'relative flex min-w-0 flex-col items-center justify-center gap-1 rounded-t-md px-1 text-[10px] text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground',
              selected && 'bg-muted/40 text-foreground'
            )}
            key={tab.view}
            onClick={() => onChange(tab.view)}
            role="tab"
            title={tab.label}
            type="button"
          >
            <span className="relative">
              <Icon className="size-4" />
              {tab.count !== undefined && tab.count > 0 && (
                <span className="absolute -right-3 -top-2 min-w-4 rounded-full bg-muted px-1 font-mono text-[8px] leading-4 text-foreground">
                  {tab.count > 99 ? '99+' : tab.count}
                </span>
              )}
            </span>
            <span className="w-full truncate text-center">{tab.label}</span>
            {selected && (
              <span className="absolute inset-x-2 bottom-0 h-0.5 bg-foreground" />
            )}
          </button>
        )
      })}
    </div>
  )
}

export function WorkspaceFilesPanel({
  changes,
  diff,
  diffLoading,
  error,
  fileCursor,
  loading,
  loadPhase,
  onNextPage,
  onOpenSettings,
  onPreviousPage,
  onRefresh,
  onSelect,
  selectedPath,
}: {
  changes: ApplicationWorkspaceChangesResponse | null
  diff: ApplicationWorkspaceDiffResponse | null
  diffLoading: boolean
  error: string | null
  fileCursor: number
  loading: boolean
  loadPhase: WorkspaceLoadPhase
  onNextPage: () => void
  onOpenSettings: () => void
  onPreviousPage: () => void
  onRefresh: () => void
  onSelect: (path: string) => void
  selectedPath: string | null
}) {
  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <div className="flex items-center gap-2 text-xs font-medium">
            <GitBranch className="size-4 stroke-success" />
            {changes?.branch ?? 'main'}
          </div>
          <p className="mt-1 font-mono text-[10px] text-muted-foreground">
            {changes?.head
              ? `HEAD ${changes.head}`
              : 'No commits yet · persistent workspace'}
          </p>
        </div>
        <Button
          aria-label="Refresh workspace files"
          className="size-8"
          disabled={loading}
          onClick={onRefresh}
          size="icon"
          type="button"
          variant="outline"
        >
          <RefreshCw className={cn('size-3.5', loading && 'animate-spin')} />
        </Button>
      </div>

      {error && (
        <div className="space-y-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-3 text-[11px] leading-5 text-destructive">
          <p>{error}</p>
          <Button
            onClick={onOpenSettings}
            size="sm"
            type="button"
            variant="outline"
          >
            Open workspace settings
          </Button>
        </div>
      )}

      {!changes && loading ? (
        <div className="flex items-center gap-2 rounded-md border border-border bg-background px-3 py-4 text-xs text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" />
          {workspaceLoadMessage(loadPhase)}
        </div>
      ) : (
        <>
          <section className="overflow-hidden rounded-lg border border-border bg-background">
            <div className="flex items-center justify-between border-b border-border px-3 py-2">
              <div>
                <p className="text-xs font-medium">Working changes</p>
                <p className="mt-0.5 text-[10px] text-muted-foreground">
                  {changes?.clean
                    ? 'Working tree clean'
                    : `${changes?.changes.length ?? 0} changed file${changes?.changes.length === 1 ? '' : 's'}`}
                </p>
              </div>
              <GitCommitHorizontal className="size-4 text-muted-foreground" />
            </div>
            <div className="max-h-56 overflow-y-auto p-1.5">
              {changes?.changes.map((change) => (
                <button
                  className={cn(
                    'flex w-full items-center gap-2 rounded px-2 py-1.5 text-left font-mono text-[10px] hover:bg-accent',
                    selectedPath === change.path && 'bg-accent text-foreground'
                  )}
                  key={change.path}
                  onClick={() => onSelect(change.path)}
                  title={change.path}
                  type="button"
                >
                  <span
                    className={cn(
                      'w-3 shrink-0 font-semibold uppercase',
                      change.status === 'deleted'
                        ? 'text-destructive'
                        : change.status === 'untracked' ||
                            change.status === 'added'
                          ? 'text-success'
                          : 'text-amber-600 dark:text-amber-300'
                    )}
                  >
                    {change.status?.slice(0, 1) ?? 'M'}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{change.path}</span>
                  {change.staged && (
                    <span className="rounded bg-success/10 px-1 text-[8px] text-success">
                      staged
                    </span>
                  )}
                </button>
              ))}
              {changes?.clean && (
                <p className="px-2 py-5 text-center text-[11px] text-muted-foreground">
                  Ask the assistant to edit files or create a commit.
                </p>
              )}
            </div>
          </section>

          {selectedPath && (
            <>
              {diffLoading ? (
                <section className="overflow-hidden rounded-lg border border-border bg-[#0d1117] text-[#c9d1d9] shadow-inner">
                  <div className="flex items-center gap-2 border-b border-white/10 px-3 py-2 font-mono text-[10px]">
                    <FileCode2 className="size-3.5 text-[#7ee787]" />
                    <span className="min-w-0 flex-1 truncate">
                      {selectedPath}
                    </span>
                  </div>
                  <div className="flex items-center gap-2 px-3 py-5 text-[10px] text-[#8b949e]">
                    <Loader2 className="size-3 animate-spin" /> Loading diff…
                  </div>
                </section>
              ) : diff?.diff ? (
                <WorkspaceDiffViewer
                  diff={diff.diff}
                  path={selectedPath}
                  truncated={diff.truncated}
                />
              ) : (
                <section className="overflow-hidden rounded-lg border border-border bg-[#0d1117] text-[#c9d1d9] shadow-inner">
                  <div className="flex items-center gap-2 border-b border-white/10 px-3 py-2 font-mono text-[10px]">
                    <FileCode2 className="size-3.5 text-[#7ee787]" />
                    <span className="min-w-0 flex-1 truncate">
                      {selectedPath}
                    </span>
                  </div>
                  <p className="px-3 py-5 text-[10px] text-[#8b949e]">
                    No textual diff is available for this file.
                  </p>
                </section>
              )}
            </>
          )}

          <section className="overflow-hidden rounded-lg border border-border bg-background">
            <div className="flex items-center justify-between gap-2 border-b border-border px-3 py-2">
              <div className="flex items-center gap-2 text-xs font-medium">
                <FolderTree className="size-3.5 text-muted-foreground" /> Files
                <span className="font-mono text-[9px] text-muted-foreground">
                  {changes
                    ? `${Math.min(fileCursor + 1, changes.listed_file_count)}–${Math.min(
                        fileCursor + changes.files.length,
                        changes.listed_file_count
                      )} of ${changes.listed_file_count}${changes.files_truncated ? '+' : ''}`
                    : '0'}
                </span>
              </div>
              <div className="flex items-center gap-1">
                <Button
                  className="h-6 px-2 text-[9px]"
                  disabled={loading || fileCursor === 0}
                  onClick={onPreviousPage}
                  size="sm"
                  type="button"
                  variant="ghost"
                >
                  Previous
                </Button>
                <Button
                  className="h-6 px-2 text-[9px]"
                  disabled={loading || changes?.next_cursor == null}
                  onClick={onNextPage}
                  size="sm"
                  type="button"
                  variant="ghost"
                >
                  Next
                </Button>
              </div>
            </div>
            <div className="max-h-52 overflow-y-auto p-1.5">
              {changes?.files.map((path) => (
                <div
                  className="flex items-center gap-2 px-2 py-1 font-mono text-[10px] text-muted-foreground"
                  key={path}
                  title={path}
                >
                  <FileCode2 className="size-3 shrink-0" />
                  <span className="truncate">{path}</span>
                </div>
              ))}
              {changes?.files.length === 0 && (
                <p className="px-2 py-5 text-center text-[11px] text-muted-foreground">
                  This repository is empty.
                </p>
              )}
            </div>
          </section>

          <p className="text-[10px] leading-4 text-muted-foreground">
            Git runs inside this application&apos;s persistent sandbox. Commits
            stay here until you explicitly connect and approve a remote push.
            Sensitive file paths and credential-like diff values are hidden.
          </p>
          {changes?.files_truncated && (
            <p className="text-[10px] text-amber-600 dark:text-amber-300">
              Large workspace: browsing is capped at the first 1,000 safe file
              paths.
            </p>
          )}
          {changes?.changes_truncated && (
            <p className="text-[10px] text-amber-600 dark:text-amber-300">
              Working changes are capped at the first 200 safe paths.
            </p>
          )}
        </>
      )}
    </div>
  )
}

function GlobalChatBoundary() {
  return (
    <div className="space-y-3">
      <section className="rounded-lg border border-border bg-background p-4">
        <div className="flex items-center gap-2 text-xs font-medium">
          <Sparkles className="size-4 stroke-success" /> User workspace
        </div>
        <p className="mt-2 text-[11px] leading-5 text-muted-foreground">
          This chat is yours and is not anchored to a project. Selectors are
          explicit, and every platform call re-checks your current role and
          project memberships.
        </p>
      </section>
      <section className="rounded-lg border border-success/30 bg-success/5 p-4">
        <div className="flex items-center gap-2 text-xs font-medium">
          <ShieldCheck className="size-4 stroke-success" /> Confirmation
          boundary
        </div>
        <p className="mt-2 text-[11px] leading-5 text-muted-foreground">
          Reads use your permissions. Changes remain proposals until you confirm
          them, and secret values never enter the model context.
        </p>
      </section>
    </div>
  )
}

function GlobalChatStartScreen({
  onCancel,
  onCreated,
  harnesses,
  harnessesLoading,
}: {
  onCancel: () => void
  onCreated: (conversation: ConversationResponse) => void
  harnesses: HarnessOption[]
  harnessesLoading: boolean
}) {
  const [harnessId, setHarnessId] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const selectedHarnessId = harnessId ?? defaultHarnessId(harnesses)

  const submit = async () => {
    if (!selectedHarnessId) return
    setSaving(true)
    setError(null)
    try {
      const { data } = await createGlobalConversation({
        body: { ai_provider: selectedHarnessId },
        throwOnError: true,
      })
      onCreated(data)
    } catch (cause) {
      setError(problemDetail(cause, 'Could not create workspace chat.'))
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="flex h-full min-h-0 overflow-y-auto bg-[radial-gradient(circle_at_top,theme(colors.muted)_0%,transparent_42%)] px-5 py-8 sm:px-8 lg:px-14">
      <section className="m-auto w-full max-w-3xl rounded-2xl border border-border bg-card/95 p-5 shadow-sm backdrop-blur sm:p-8">
        <div className="flex items-start justify-between gap-6 border-b border-border pb-6">
          <div>
            <p className="font-mono text-[10px] font-semibold uppercase tracking-[0.22em] text-success">
              New workspace chat
            </p>
            <h1 className="mt-2 text-2xl font-semibold tracking-tight">
              Ask Temps across your platform.
            </h1>
            <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
              This thread belongs to you, not a project. The harness runs in a
              persistent managed sandbox and every platform operation inherits
              your current role and project access.
            </p>
          </div>
          <Button onClick={onCancel} size="sm" type="button" variant="ghost">
            Cancel
          </Button>
        </div>
        <div className="mt-6">
          <HarnessPicker
            harnesses={harnesses}
            loading={harnessesLoading}
            selectedId={selectedHarnessId}
            onSelect={setHarnessId}
          />
          {error && <p className="mt-4 text-sm text-destructive">{error}</p>}
        </div>
        <div className="mt-6 flex justify-end gap-3 border-t border-border pt-5">
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            disabled={saving || !selectedHarnessId}
            onClick={() => void submit()}
          >
            {saving && <Loader2 className="mr-1.5 size-4 animate-spin" />}
            Start workspace chat
          </Button>
        </div>
      </section>
    </div>
  )
}

function CenteredMessage({
  icon: Icon,
  spin,
  title,
  detail,
  action,
  onAction,
}: {
  icon: React.ComponentType<{ className?: string }>
  spin?: boolean
  title: string
  detail?: string
  action?: string
  onAction?: () => void
}) {
  return (
    <div className="flex h-full items-center justify-center px-6 text-center">
      <div className="max-w-md">
        <Icon
          className={cn(
            'mx-auto size-7 stroke-success',
            spin && 'animate-spin'
          )}
        />
        <h1 className="mt-4 text-xl font-medium">{title}</h1>
        {detail && (
          <p className="mt-2 text-sm leading-6 text-muted-foreground">
            {detail}
          </p>
        )}
        {action && (
          <Button onClick={onAction} className="mt-5">
            <Plus className="mr-1.5 size-4" /> {action}
          </Button>
        )}
      </div>
    </div>
  )
}

function ApplicationBoundary({
  application,
}: {
  application: ApplicationResponse
}) {
  return (
    <div className="space-y-3">
      <section className="rounded-lg border border-border bg-background p-4">
        <p className="text-xs font-medium">{application.name}</p>
        <div className="mt-3 space-y-2">
          {application.projects.map((project) => (
            <div key={project.id} className="flex items-center gap-2 text-xs">
              <Code2 className="size-3.5 stroke-success" />
              <span className="min-w-0 flex-1 truncate">{project.name}</span>
              {project.is_private && (
                <LockKeyhole className="size-3 text-muted-foreground" />
              )}
            </div>
          ))}
          {application.projects.length === 0 && (
            <p className="text-[11px] leading-5 text-muted-foreground">
              No projects yet. Ask the thread to create the application topology
              when you are ready.
            </p>
          )}
        </div>
      </section>
      <section className="rounded-lg border border-success/30 bg-success/5 p-4">
        <div className="flex items-center gap-2 text-xs font-medium">
          <KeyRound className="size-4 stroke-success" /> Credential boundary
        </div>
        <p className="mt-2 text-[11px] leading-5 text-muted-foreground">
          The model can request capabilities and receive opaque references. It
          can never read secret values or host login tokens.
        </p>
      </section>
      <ApplicationPreview application={application} />
    </div>
  )
}

function ApplicationPreview({
  application,
}: {
  application: ApplicationResponse
}) {
  const [port, setPort] = useState('3000')
  const [opening, setOpening] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const openPreview = async () => {
    const parsedPort = Number(port)
    if (!Number.isInteger(parsedPort) || parsedPort < 1 || parsedPort > 65535) {
      setError('Enter a port from 1 to 65535.')
      return
    }
    // Open synchronously with the click so browsers keep this user-initiated
    // tab instead of treating the authenticated URL returned below as a popup.
    const previewWindow = window.open('', '_blank')
    if (previewWindow) previewWindow.opener = null
    setOpening(true)
    setError(null)
    try {
      const { data: payload } = await createApplicationPreviewLink({
        path: { application_public_id: application.public_id },
        body: { port: parsedPort, path: '/' },
        throwOnError: true,
      })
      if (previewWindow) {
        previewWindow.location.assign(payload.url)
      } else {
        window.location.assign(payload.url)
      }
    } catch (cause) {
      previewWindow?.close()
      setError(
        cause instanceof Error ? cause.message : 'Could not open preview.'
      )
    } finally {
      setOpening(false)
    }
  }

  return (
    <section className="rounded-lg border border-border bg-background p-4">
      <div className="flex items-center gap-2 text-xs font-medium">
        <TerminalSquare className="size-4 stroke-success" /> Sandbox preview
      </div>
      <p className="mt-2 text-[11px] leading-5 text-muted-foreground">
        Open a running development server through a one-hour authenticated link.
        Temps never exposes the container address or its preview password.
      </p>
      <div className="mt-3 flex gap-2">
        <Input
          aria-label="Development server port"
          className="h-8 font-mono text-xs"
          inputMode="numeric"
          onChange={(event) => setPort(event.target.value)}
          value={port}
        />
        <Button
          className="h-8 shrink-0 px-2.5 text-xs"
          disabled={opening}
          onClick={() => void openPreview()}
          size="sm"
          type="button"
          variant="outline"
        >
          {opening ? 'Opening…' : 'Open'}
        </Button>
      </div>
      {error && <p className="mt-2 text-[11px] text-destructive">{error}</p>}
    </section>
  )
}

export function ApplicationStartScreen({
  onCancel,
  onCreated,
  harnesses,
  harnessesLoading,
}: {
  onCancel: () => void
  onCreated: (
    application: ApplicationResponse,
    conversation: ConversationResponse
  ) => void
  harnesses: HarnessOption[]
  harnessesLoading: boolean
}) {
  const [name, setName] = useState('')
  const [provisionedApplication, setProvisionedApplication] =
    useState<ApplicationResponse | null>(null)
  const [importedApplicationId, setImportedApplicationId] = useState<
    string | null
  >(null)
  const [harnessId, setHarnessId] = useState<string | null>(null)
  const [sourceMode, setSourceMode] = useState<WorkspaceSourceMode>('blank')
  const [localImport, setLocalImport] = useState<LocalImportSelection | null>(
    null
  )
  const [gitUrl, setGitUrl] = useState('')
  const [gitRevision, setGitRevision] = useState('')
  const [gitConnectionId, setGitConnectionId] = useState<number | null>(null)
  const [connections, setConnections] = useState<ConnectionResponse[]>([])
  const [gitProviders, setGitProviders] = useState<ProviderResponse[]>([])
  const [connectionsLoading, setConnectionsLoading] = useState(false)
  const [saving, setSaving] = useState(false)
  const [savingStep, setSavingStep] = useState('')
  const [error, setError] = useState<string | null>(null)
  const folderInputRef = useRef<HTMLInputElement | null>(null)

  const selectedHarnessId = harnessId ?? defaultHarnessId(harnesses)

  useEffect(() => {
    if (sourceMode !== 'git' || connections.length > 0) return
    let cancelled = false
    void Promise.all([
      listConnections({ query: { per_page: 100 }, throwOnError: true }),
      listGitProviders({ throwOnError: true }),
    ])
      .then(([connectionResult, providerResult]) => {
        if (cancelled) return
        setConnections(connectionResult.data.connections)
        setGitProviders(providerResult.data)
      })
      .catch((cause) => {
        if (!cancelled) {
          setError(problemDetail(cause, 'Could not load Git connections.'))
        }
      })
      .finally(() => {
        if (!cancelled) setConnectionsLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [connections.length, sourceMode])

  const providerForConnection = (connection: ConnectionResponse) =>
    gitProviders.find((provider) => provider.id === connection.provider_id)

  const importSource = async (application: ApplicationResponse) => {
    if (sourceMode === 'blank') return
    const project =
      application.projects.find((candidate) => candidate.is_primary) ??
      application.projects[0]
    if (!project) {
      throw new Error('The workspace was created without a starter project.')
    }

    setSavingStep('Starting persistent sandbox…')
    const { data: workspace } = await controlApplicationWorkspace({
      path: { application_public_id: application.public_id },
      body: { action: 'resume' },
      throwOnError: true,
    })
    if (!workspace.sandbox_public_id) {
      throw new Error('The persistent sandbox started without a public ID.')
    }
    if (sourceMode === 'git') {
      setSavingStep('Cloning repository…')
      await importApplicationWorkspaceGit({
        path: {
          application_public_id: application.public_id,
          project_id: project.id,
        },
        body: {
          url: gitUrl.trim(),
          revision: gitRevision.trim() || null,
          depth: 1,
          git_connection_id: gitConnectionId,
        },
        throwOnError: true,
      })
      return
    }

    if (!localImport) {
      throw new Error('Choose a local folder before creating the workspace.')
    }
    const batches = batchLocalImportFiles(localImport.accepted)
    for (let index = 0; index < batches.length; index += 1) {
      setSavingStep(`Uploading local files ${index + 1}/${batches.length}…`)
      const files = await Promise.all(
        batches[index].map(async ({ file, path }) => ({
          path,
          contents_b64: await fileToBase64(file),
        }))
      )
      await writeApplicationWorkspaceFiles({
        path: {
          application_public_id: application.public_id,
          project_id: project.id,
        },
        body: { files },
        throwOnError: true,
      })
    }
  }

  const submit = async () => {
    if (
      !name.trim() ||
      !selectedHarnessId ||
      (sourceMode === 'git' && !gitUrl.trim()) ||
      (sourceMode === 'local' && !localImport)
    )
      return
    setSaving(true)
    setSavingStep('Creating workspace…')
    setError(null)
    try {
      const application =
        provisionedApplication ??
        (
          await createApplication({
            body: {
              name: name.trim(),
              description: null,
              project_ids: [],
              starter_project: {
                name: name.trim(),
                preset: 'autopack',
                exposed_port: 3000,
              },
            },
            throwOnError: true,
          })
        ).data
      if (!provisionedApplication) {
        setProvisionedApplication(application)
      }
      if (importedApplicationId !== application.public_id) {
        await importSource(application)
        setImportedApplicationId(application.public_id)
      }
      setSavingStep('Starting first thread…')
      let conversation: ConversationResponse
      try {
        const { data } = await createApplicationConversation({
          path: { application_public_id: application.public_id },
          body: { ai_provider: selectedHarnessId },
          throwOnError: true,
        })
        conversation = data
      } catch (cause) {
        const reason =
          cause instanceof Error
            ? cause.message
            : 'The selected harness could not start a thread.'
        throw new Error(
          `Application “${application.name}” was created, but its starter thread could not start with the selected harness: ${reason}`,
          { cause }
        )
      }
      onCreated(application, conversation)
      setName('')
      setProvisionedApplication(null)
      setImportedApplicationId(null)
      setHarnessId(null)
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : 'Could not create workspace.'
      )
    } finally {
      setSaving(false)
      setSavingStep('')
    }
  }

  return (
    <div className="flex h-full min-h-0 overflow-y-auto bg-[radial-gradient(circle_at_top,theme(colors.muted)_0%,transparent_42%)] px-5 py-8 sm:px-8 lg:px-14">
      <section className="m-auto w-full max-w-3xl rounded-2xl border border-border bg-card/95 p-5 shadow-sm backdrop-blur sm:p-8">
        <div className="flex items-start justify-between gap-6 border-b border-border pb-6">
          <div>
            <p className="font-mono text-[10px] font-semibold uppercase tracking-[0.22em] text-success">
              New workspace
            </p>
            <h1 className="mt-2 text-2xl font-semibold tracking-tight">
              Start a persistent machine.
            </h1>
            <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
              Name the workspace and choose a local harness. Every thread shares
              its persistent sandbox and files, while the assistant can operate
              any Temps resource allowed by your current role.
            </p>
          </div>
          <Button onClick={onCancel} size="sm" type="button" variant="ghost">
            Cancel
          </Button>
        </div>
        <div className="mt-6 space-y-5">
          <div className="space-y-2">
            <Label htmlFor="ai-app-name">Workspace name</Label>
            <Input
              autoFocus
              id="ai-app-name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Product workspace"
            />
            <p className="text-xs text-muted-foreground">
              Temps creates a deployable Autopack project and keeps its files on
              this machine between threads.
            </p>
          </div>
          <div className="space-y-3">
            <div>
              <Label>Start from</Label>
              <p className="mt-1 text-xs text-muted-foreground">
                Import code now, or let the assistant build the project from
                scratch.
              </p>
            </div>
            <div className="grid gap-2 sm:grid-cols-3">
              {(
                [
                  {
                    id: 'blank',
                    label: 'Blank project',
                    detail: 'Build with AI',
                    icon: Sparkles,
                  },
                  {
                    id: 'local',
                    label: 'Local folder',
                    detail: 'Upload existing code',
                    icon: FolderTree,
                  },
                  {
                    id: 'git',
                    label: 'Git repository',
                    detail: 'Public or connected',
                    icon: GitBranch,
                  },
                ] as const
              ).map((option) => {
                const Icon = option.icon
                return (
                  <button
                    className={cn(
                      'flex items-start gap-3 rounded-lg border p-3 text-left transition-colors',
                      sourceMode === option.id
                        ? 'border-primary bg-accent text-accent-foreground'
                        : 'border-border bg-background hover:bg-accent/60'
                    )}
                    disabled={Boolean(provisionedApplication)}
                    key={option.id}
                    onClick={() => {
                      setSourceMode(option.id)
                      setConnectionsLoading(
                        option.id === 'git' && connections.length === 0
                      )
                      setError(null)
                    }}
                    type="button"
                  >
                    <Icon className="mt-0.5 size-4 shrink-0" />
                    <span className="min-w-0">
                      <span className="block text-sm font-medium">
                        {option.label}
                      </span>
                      <span className="block text-[11px] text-muted-foreground">
                        {option.detail}
                      </span>
                    </span>
                  </button>
                )
              })}
            </div>

            {sourceMode === 'local' && (
              <div className="rounded-lg border border-border bg-muted/30 p-3">
                <input
                  className="hidden"
                  disabled={Boolean(provisionedApplication)}
                  multiple
                  onChange={(event) => {
                    try {
                      const selection = prepareLocalImport(
                        Array.from(event.target.files ?? [])
                      )
                      setLocalImport(selection)
                      setError(null)
                    } catch (cause) {
                      setLocalImport(null)
                      setError(
                        problemDetail(cause, 'Could not read the local folder.')
                      )
                    }
                  }}
                  ref={(element) => {
                    folderInputRef.current = element
                    element?.setAttribute('webkitdirectory', '')
                    element?.setAttribute('directory', '')
                  }}
                  type="file"
                />
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium">
                      {localImport?.rootName ?? 'Choose a folder'}
                    </p>
                    <p className="mt-0.5 text-xs text-muted-foreground">
                      {localImport
                        ? `${localImport.accepted.length.toLocaleString()} files · ${(
                            localImport.totalBytes /
                            1024 /
                            1024
                          ).toFixed(1)} MB${
                            localImport.skipped.length > 0
                              ? ` · ${localImport.skipped.length} excluded`
                              : ''
                          }`
                        : 'Dependencies, Git metadata, and credential files are excluded.'}
                    </p>
                  </div>
                  <Button
                    disabled={Boolean(provisionedApplication)}
                    onClick={() => folderInputRef.current?.click()}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    Browse
                  </Button>
                </div>
              </div>
            )}

            {sourceMode === 'git' && (
              <div className="space-y-3 rounded-lg border border-border bg-muted/30 p-3">
                <div className="grid gap-3 sm:grid-cols-[1fr_10rem]">
                  <div className="space-y-1.5">
                    <Label htmlFor="workspace-git-url">Repository URL</Label>
                    <Input
                      disabled={Boolean(provisionedApplication)}
                      id="workspace-git-url"
                      onChange={(event) => setGitUrl(event.target.value)}
                      placeholder="https://github.com/org/repository.git"
                      value={gitUrl}
                    />
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="workspace-git-revision">
                      Branch or tag
                    </Label>
                    <Input
                      disabled={Boolean(provisionedApplication)}
                      id="workspace-git-revision"
                      onChange={(event) => setGitRevision(event.target.value)}
                      placeholder="Default"
                      value={gitRevision}
                    />
                  </div>
                </div>
                <div>
                  <p className="text-xs font-medium">Access</p>
                  <div className="mt-2 grid gap-2 sm:grid-cols-2">
                    <button
                      className={cn(
                        'flex items-center gap-2 rounded-md border p-2 text-left text-xs',
                        gitConnectionId === null
                          ? 'border-primary bg-accent'
                          : 'border-border bg-background hover:bg-accent/60'
                      )}
                      disabled={Boolean(provisionedApplication)}
                      onClick={() => setGitConnectionId(null)}
                      type="button"
                    >
                      <GitBranch className="size-5 shrink-0" />
                      <span>
                        <span className="block font-medium">
                          Public repository
                        </span>
                        <span className="text-muted-foreground">
                          No credential
                        </span>
                      </span>
                    </button>
                    {connectionsLoading && (
                      <div className="flex items-center gap-2 rounded-md border border-border bg-background p-2 text-xs text-muted-foreground">
                        <Loader2 className="size-4 animate-spin" /> Loading
                        connections…
                      </div>
                    )}
                    {connections.map((connection) => {
                      const provider = providerForConnection(connection)
                      const available =
                        connection.is_active &&
                        !connection.is_expired &&
                        connection.has_authenticated_credentials
                      return (
                        <button
                          className={cn(
                            'flex items-center gap-2 rounded-md border p-2 text-left text-xs',
                            gitConnectionId === connection.id
                              ? 'border-primary bg-accent'
                              : 'border-border bg-background hover:bg-accent/60',
                            !available && 'opacity-50'
                          )}
                          disabled={
                            Boolean(provisionedApplication) || !available
                          }
                          key={connection.id}
                          onClick={() => setGitConnectionId(connection.id)}
                          type="button"
                        >
                          <ProviderLogo
                            className="size-5 shrink-0"
                            providerType={provider?.provider_type}
                          />
                          <span className="min-w-0">
                            <span className="block truncate font-medium">
                              {connection.account_name}
                            </span>
                            <span className="block truncate text-muted-foreground">
                              {provider?.name ?? 'Git provider'} ·{' '}
                              {available
                                ? connection.health_status
                                : 'unavailable'}
                            </span>
                          </span>
                        </button>
                      )
                    })}
                  </div>
                  <p className="mt-2 text-[11px] leading-4 text-muted-foreground">
                    Stored credentials are resolved only for this clone and are
                    never sent to the browser, chat, or repository URL.
                  </p>
                </div>
              </div>
            )}
          </div>
          <HarnessPicker
            harnesses={harnesses}
            loading={harnessesLoading}
            selectedId={selectedHarnessId}
            onSelect={setHarnessId}
          />
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <div className="mt-6 flex justify-end gap-3 border-t border-border pt-5">
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            disabled={
              saving ||
              !name.trim() ||
              !selectedHarnessId ||
              (sourceMode === 'git' && !gitUrl.trim()) ||
              (sourceMode === 'local' && !localImport)
            }
            onClick={() => void submit()}
          >
            {saving && <Loader2 className="mr-1.5 size-4 animate-spin" />}
            {saving ? savingStep : 'Start workspace'}
          </Button>
        </div>
      </section>
    </div>
  )
}

export function HarnessPicker({
  harnesses,
  loading,
  selectedId,
  onSelect,
}: {
  harnesses: HarnessOption[]
  loading: boolean
  selectedId: string | null
  onSelect: (providerId: string) => void
}) {
  return (
    <section className="rounded-lg border border-border bg-muted/40 p-3">
      <div className="flex items-center justify-between gap-3">
        <div>
          <p className="text-sm font-medium">Detected harnesses</p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            The selected harness starts the first thread and remains pinned to
            it.
          </p>
        </div>
        <Button asChild size="sm" type="button" variant="ghost">
          <Link to="/agent-sandbox/providers">
            <TerminalSquare className="mr-1.5 size-4" /> Manage
          </Link>
        </Button>
      </div>
      {loading ? (
        <div className="mt-3 flex items-center gap-2 rounded-md border border-border bg-background p-3 text-xs text-muted-foreground">
          <Loader2 className="size-4 animate-spin" />
          Checking installed harnesses…
        </div>
      ) : harnesses.length > 0 ? (
        <div className="mt-3 flex flex-wrap gap-2">
          {harnesses.map((harness) => (
            <button
              key={harness.id}
              type="button"
              onClick={() => onSelect(harness.id)}
              className={cn(
                'flex items-center gap-2 rounded-lg border px-2.5 py-2 text-left text-xs transition-colors',
                harness.id === selectedId
                  ? 'border-primary bg-accent text-accent-foreground'
                  : 'border-border bg-background text-foreground hover:bg-accent'
              )}
            >
              <AiHarnessLogo providerId={harness.id} size={24} />
              <span>
                <span className="font-medium">{harness.name}</span>
                {harness.authMethod && (
                  <span className="ml-1 text-muted-foreground">
                    · {harness.authMethod}
                  </span>
                )}
              </span>
            </button>
          ))}
        </div>
      ) : (
        <div className="mt-3 flex items-center justify-between gap-3 rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-xs text-amber-700 dark:text-amber-300">
          <p>
            No harness is ready for persistent workspace execution. Configure
            Claude Code to start a thread; Codex and OpenCode remain available
            for host workflows until their secure workspace relays are added.
          </p>
          <Button asChild size="sm" type="button" variant="outline">
            <Link to="/agent-sandbox/providers">Configure harness</Link>
          </Button>
        </div>
      )}
    </section>
  )
}

function CreateThreadDialog({
  application,
  harnesses,
  harnessesLoading,
  open,
  onOpenChange,
  onCreated,
}: {
  application: ApplicationResponse
  harnesses: HarnessOption[]
  harnessesLoading: boolean
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreated: (conversation: ConversationResponse) => void
}) {
  const [harnessId, setHarnessId] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const selectedHarnessId = harnessId ?? defaultHarnessId(harnesses)

  useEffect(() => {
    if (harnessId && !harnesses.some((harness) => harness.id === harnessId)) {
      const resetTimer = window.setTimeout(() => setHarnessId(null), 0)
      return () => window.clearTimeout(resetTimer)
    }
  }, [harnessId, harnesses])

  const submit = async () => {
    if (!selectedHarnessId) {
      setError('Choose an authenticated development harness first.')
      return
    }
    setSaving(true)
    setError(null)
    try {
      const { data } = await createApplicationConversation({
        path: { application_public_id: application.public_id },
        body: {
          ai_provider: selectedHarnessId,
        },
        throwOnError: true,
      })
      onCreated(data)
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : 'Could not create thread.'
      )
    } finally {
      setSaving(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New workspace thread</DialogTitle>
          <DialogDescription>
            Choose the development harness that can use Temps tools and the
            shared workspace. The harness is pinned to this thread.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <HarnessPicker
            harnesses={harnesses}
            loading={harnessesLoading}
            selectedId={selectedHarnessId}
            onSelect={setHarnessId}
          />
          <p className="rounded-md border border-border bg-muted/50 p-3 text-xs leading-5 text-muted-foreground">
            The harness chooses its own model and runs inside a persistent Temps
            sandbox mounted on this workspace. It can change project files but
            never receives host login tokens or secret values. Platform actions
            remain explicitly approval-gated.
          </p>
          {harnesses.length === 0 && (
            <p className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-sm text-amber-600">
              No development harness is ready. Authenticate Claude Code, Codex,
              or OpenCode in Agent Sandbox settings.
            </p>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            disabled={saving || !selectedHarnessId}
            onClick={() => void submit()}
          >
            {saving && <Loader2 className="mr-1.5 size-4 animate-spin" />}{' '}
            Create thread
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
