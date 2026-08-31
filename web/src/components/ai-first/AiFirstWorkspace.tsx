// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Boxes,
  ChevronDown,
  Circle,
  Code2,
  FolderOpen,
  HardDrive,
  KeyRound,
  Loader2,
  LockKeyhole,
  Plus,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  TerminalSquare,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import {
  createApplication,
  createApplicationConversation,
  createProject,
  createSandbox,
  getAiProviderStatus,
  getProjects,
  listApplicationConversations,
  listApplications,
  listThreadArtifacts,
  type ApplicationResponse,
  type ConversationResponse,
  type ProjectResponse,
  type ThreadArtifactResponse,
} from '@/api/client'
import { DebugChatPanel } from '@/components/ai/DebugChatPanel'
import {
  type ChatProviderOption,
  type ChatRuntimeSelection,
  chatProviderLabel,
  resolveChatRuntimeSelection,
} from '@/components/ai/chat-runtime-options'
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
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { ArtifactRenderer } from './ArtifactRenderer'

export function AiFirstWorkspace() {
  const [applications, setApplications] = useState<ApplicationResponse[]>([])
  const [activeApplicationId, setActiveApplicationId] = useState<string | null>(
    null
  )
  const [conversations, setConversations] = useState<ConversationResponse[]>([])
  const [activeConversationId, setActiveConversationId] = useState<
    string | null
  >(null)
  const [artifacts, setArtifacts] = useState<ThreadArtifactResponse[]>([])
  const [providers, setProviders] = useState<ChatProviderOption[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [applicationDialogOpen, setApplicationDialogOpen] = useState(false)
  const [threadDialogOpen, setThreadDialogOpen] = useState(false)

  const activeApplication = applications.find(
    (application) => application.public_id === activeApplicationId
  )
  const activeConversation = conversations.find(
    (conversation) => conversation.public_id === activeConversationId
  )

  const loadApplications = useCallback(async () => {
    try {
      const { data } = await listApplications({ throwOnError: true })
      const next = data
      setApplications(next)
      setActiveApplicationId((current) =>
        current && next.some((application) => application.public_id === current)
          ? current
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
  }, [])

  useEffect(() => {
    const loadTimer = window.setTimeout(() => void loadApplications(), 0)
    getAiProviderStatus()
      .then(({ data }) => {
        const next = (data?.available_providers ?? []).map((provider) => {
          const extended = provider as typeof provider & ChatProviderOption
          return {
            id: provider.id,
            name: provider.name,
            auth_source: provider.auth_source,
            models: extended.models ?? [],
            default_model_id: extended.default_model_id,
            permission_modes: extended.permission_modes ?? [],
            default_permission_mode_id: extended.default_permission_mode_id,
          }
        })
        setProviders(next)
      })
      .catch(() => setProviders([]))
    return () => window.clearTimeout(loadTimer)
  }, [loadApplications])

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
            : (next[0]?.public_id ?? null)
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
  }, [activeApplicationId])

  useEffect(() => {
    if (!activeApplicationId || !activeConversationId) {
      return
    }
    let cancelled = false
    const refresh = () => {
      listThreadArtifacts({
        path: {
          application_public_id: activeApplicationId,
          conversation_public_id: activeConversationId,
        },
        throwOnError: true,
      })
        .then(({ data: next }) => {
          if (!cancelled) setArtifacts(next)
        })
        .catch(() => {
          // Chat remains useful if a background artifact refresh fails.
        })
    }
    refresh()
    const timer = window.setInterval(refresh, 3_000)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [activeApplicationId, activeConversationId])

  const handleApplicationCreated = (application: ApplicationResponse) => {
    setApplications((current) => [application, ...current])
    setActiveApplicationId(application.public_id)
    setApplicationDialogOpen(false)
    setThreadDialogOpen(true)
  }

  const selectApplication = (applicationId: string) => {
    setActiveApplicationId(applicationId)
    setConversations([])
    setActiveConversationId(null)
    setArtifacts([])
  }

  const handleThreadCreated = (conversation: ConversationResponse) => {
    setConversations((current) => [conversation, ...current])
    setActiveConversationId(conversation.public_id)
    setThreadDialogOpen(false)
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
          <div className="hidden items-center gap-2 rounded-md border border-border bg-muted px-3 py-1.5 text-xs text-muted-foreground sm:flex">
            <ShieldCheck className="size-3.5 stroke-success" />
            Human-approved execution
            <ChevronDown className="size-3" />
          </div>
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

      <div className="grid h-[calc(100dvh-3.5rem)] grid-cols-[240px_minmax(0,1fr)_330px]">
        <aside className="min-h-0 border-r border-border bg-card">
          <div className="flex items-center justify-between border-b border-border px-3 py-3">
            <span className="font-mono text-[10px] font-semibold tracking-wide text-muted-foreground">
              Applications
            </span>
            <button
              type="button"
              onClick={() => setApplicationDialogOpen(true)}
              className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
              aria-label="New application"
            >
              <Plus className="size-4" />
            </button>
          </div>
          <div className="space-y-1 p-2">
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
          {activeApplication && (
            <>
              <div className="mx-3 my-2 border-t border-border" />
              <div className="flex items-center justify-between px-3 py-2">
                <span className="font-mono text-[10px] font-semibold tracking-wide text-muted-foreground">
                  Threads
                </span>
                <button
                  type="button"
                  onClick={() => setThreadDialogOpen(true)}
                  className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                  aria-label="New thread"
                >
                  <Plus className="size-4" />
                </button>
              </div>
              <div className="space-y-1 px-2">
                {conversations.map((conversation) => (
                  <button
                    key={conversation.public_id}
                    type="button"
                    onClick={() =>
                      setActiveConversationId(conversation.public_id)
                    }
                    className={cn(
                      'w-full rounded-md px-3 py-2 text-left',
                      conversation.public_id === activeConversationId
                        ? 'bg-accent text-accent-foreground'
                        : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                    )}
                  >
                    <p className="truncate text-xs">
                      {conversation.title ?? 'Application thread'}
                    </p>
                    <p className="mt-1 truncate text-[10px]">
                      {conversation.ai_provider} · {conversation.ai_model}
                    </p>
                  </button>
                ))}
              </div>
            </>
          )}
        </aside>

        <main className="min-h-0 min-w-0">
          {loading ? (
            <CenteredMessage
              icon={Loader2}
              spin
              title="Loading AI workspace…"
            />
          ) : error && applications.length === 0 ? (
            <CenteredMessage
              icon={RefreshCw}
              title="The AI application API is unavailable"
              detail={error}
              action="Try again"
              onAction={() => void loadApplications()}
            />
          ) : !activeApplication ? (
            <CenteredMessage
              icon={Sparkles}
              title="Build and operate an application through chat"
              detail="Group one or more Temps projects, choose a local AI harness or API provider, and keep every decision in a persistent thread."
              action="Create application"
              onAction={() => setApplicationDialogOpen(true)}
            />
          ) : !activeConversation ? (
            <CenteredMessage
              icon={Code2}
              title={`Start a thread for ${activeApplication.name}`}
              detail="The provider is pinned to the thread. Every generated mutation remains scoped to a project and waits for approval."
              action="New thread"
              onAction={() => setThreadDialogOpen(true)}
            />
          ) : (
            <div className="flex h-full min-h-0 flex-col">
              <div className="border-b border-border px-5 py-3">
                <p className="text-sm font-medium">
                  {activeConversation.title ?? activeApplication.name}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {activeApplication.projects.length} linked projects ·{' '}
                  {activeConversation.ai_provider} · secrets blocked from chat
                </p>
              </div>
              <div className="min-h-0 flex-1">
                <DebugChatPanel
                  projectId={activeConversation.project_id}
                  contextType="application"
                  contextId={activeConversation.context_id}
                  lazyCreate
                  emptyHint="Describe the outcome you want across this application."
                  placeholder="Tell Temps what you want to ship…"
                />
              </div>
            </div>
          )}
        </main>

        <aside className="min-h-0 overflow-y-auto border-l border-border bg-card p-4">
          <div className="mb-4 flex items-center gap-2">
            <Boxes className="size-4 stroke-success" />
            <div>
              <p className="text-xs font-medium">Generated view</p>
              <p className="text-[10px] text-muted-foreground">
                Typed artifacts, never executable UI
              </p>
            </div>
          </div>
          <div className="space-y-3">
            {artifacts.map((artifact) => (
              <ArtifactRenderer key={artifact.public_id} artifact={artifact} />
            ))}
            {artifacts.length === 0 && activeApplication && (
              <ApplicationBoundary application={activeApplication} />
            )}
          </div>
        </aside>
      </div>

      <CreateApplicationDialog
        open={applicationDialogOpen}
        onOpenChange={setApplicationDialogOpen}
        onCreated={handleApplicationCreated}
      />
      {activeApplication && (
        <CreateThreadDialog
          application={activeApplication}
          providers={providers}
          open={threadDialogOpen}
          onOpenChange={setThreadDialogOpen}
          onCreated={handleThreadCreated}
        />
      )}
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
    </div>
  )
}

function CreateApplicationDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreated: (application: ApplicationResponse) => void
}) {
  const [projects, setProjects] = useState<ProjectResponse[]>([])
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [selected, setSelected] = useState<number[]>([])
  const [startMode, setStartMode] = useState<'existing' | 'new' | null>(null)
  const [newProjectName, setNewProjectName] = useState('')
  const [newProjectMode, setNewProjectMode] = useState<
    'manual' | 'workspace' | 'local'
  >('workspace')
  const [localFolderName, setLocalFolderName] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!open) return
    let cancelled = false
    getProjects({ query: { per_page: 100 } })
      .then(({ data }) => {
        if (cancelled) return
        const next = data?.projects ?? []
        setError(null)
        setProjects(next)
        setStartMode(next.length === 0 ? 'new' : null)
      })
      .catch(() => {
        if (cancelled) return
        setProjects([])
        setStartMode('new')
        setError(
          'Could not load existing projects. You can still start a new one.'
        )
      })
    return () => {
      cancelled = true
    }
  }, [open])

  const chooseLocalFolder = async () => {
    type DirectoryPickerWindow = Window & {
      showDirectoryPicker?: () => Promise<{ name: string }>
    }
    const picker = (window as DirectoryPickerWindow).showDirectoryPicker
    if (!picker) {
      setError(
        'This browser cannot select a local folder. Use a Chromium browser, or choose a Temps workspace.'
      )
      return
    }
    try {
      const folder = await picker()
      setLocalFolderName(folder.name)
      setError(null)
    } catch (cause) {
      if ((cause as DOMException).name !== 'AbortError') {
        setError('Temps could not select that local folder.')
      }
    }
  }

  const submit = async () => {
    if (
      !name.trim() ||
      !startMode ||
      (startMode === 'existing' && selected.length === 0) ||
      (startMode === 'new' && !newProjectName.trim()) ||
      (startMode === 'new' && newProjectMode === 'local' && !localFolderName)
    ) {
      return
    }
    setSaving(true)
    setError(null)
    try {
      let projectIds = selected
      if (startMode === 'new') {
        const { data: project } = await createProject({
          body: {
            name: newProjectName.trim(),
            preset: 'dockerfile',
            directory: './',
            main_branch: 'main',
            source_type: 'manual',
            project_type: 'docker',
            automatic_deploy: false,
            exposed_port: 3000,
            storage_service_ids: [],
          },
          throwOnError: true,
        })
        projectIds = [...projectIds, project.id]
        setProjects((current) => [project, ...current])

        if (newProjectMode === 'workspace') {
          try {
            await createSandbox({
              body: {
                name: `${project.name} workspace`,
                project_id: project.id,
                lifecycle: 'workspace',
                timeout_secs: 3_600,
              },
              throwOnError: true,
            })
          } catch (cause) {
            const reason =
              cause instanceof Error
                ? cause.message
                : 'The sandbox could not be created.'
            throw new Error(
              `Project “${project.name}” was created, but its Temps workspace was not ready: ${reason}`,
              { cause }
            )
          }
        }
      }
      const { data } = await createApplication({
        body: {
          name: name.trim(),
          description: description.trim() || null,
          project_ids: projectIds,
        },
        throwOnError: true,
      })
      onCreated(data)
      setName('')
      setDescription('')
      setSelected([])
      setNewProjectName('')
      setNewProjectMode('workspace')
      setLocalFolderName(null)
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : 'Could not create application.'
      )
    } finally {
      setSaving(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Start an application</DialogTitle>
          <DialogDescription>
            An application gives one or more projects a shared AI context.
            Access is checked against every linked project on every request.
          </DialogDescription>
        </DialogHeader>
        {startMode === null ? (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">
              What should this application start from?
            </p>
            <div className="grid gap-3 sm:grid-cols-2">
              <StartChoice
                description="Bring one or more current Temps projects into this AI context."
                icon={Boxes}
                label="Use existing projects"
                onSelect={() => {
                  setNewProjectName('')
                  setLocalFolderName(null)
                  setStartMode('existing')
                }}
              />
              <StartChoice
                description="Create the first deployable project and choose where the AI develops it."
                icon={Plus}
                label="Create a new project"
                onSelect={() => {
                  setSelected([])
                  setStartMode('new')
                }}
              />
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <>
            {projects.length > 0 && (
              <button
                className="text-sm text-muted-foreground underline underline-offset-4 hover:text-foreground"
                onClick={() => setStartMode(null)}
                type="button"
              >
                Change starting point
              </button>
            )}
            <div className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="ai-app-name">Name</Label>
                <Input
                  id="ai-app-name"
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="ai-app-description">Outcome</Label>
                <Textarea
                  id="ai-app-description"
                  value={description}
                  onChange={(event) => setDescription(event.target.value)}
                  placeholder="What should this application do?"
                />
              </div>
              <div className="space-y-4">
                {startMode === 'existing' && (
                  <div className="space-y-2">
                    <Label>Projects to include</Label>
                    <div className="max-h-52 space-y-1 overflow-y-auto rounded-lg border border-border bg-muted/50 p-2">
                      {projects.map((project) => {
                        const checked = selected.includes(project.id)
                        return (
                          <button
                            key={project.id}
                            type="button"
                            onClick={() =>
                              setSelected((current) =>
                                checked
                                  ? current.filter((id) => id !== project.id)
                                  : [...current, project.id]
                              )
                            }
                            className={cn(
                              'flex w-full items-center gap-3 rounded-md px-3 py-2 text-left text-sm',
                              checked
                                ? 'bg-accent text-accent-foreground'
                                : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                            )}
                          >
                            <Circle
                              className={cn(
                                'size-3',
                                checked && 'fill-primary text-primary'
                              )}
                            />
                            <span className="min-w-0 flex-1 truncate">
                              {project.name}
                            </span>
                            {project.is_public_repo === false && (
                              <LockKeyhole className="size-3.5 text-muted-foreground" />
                            )}
                          </button>
                        )
                      })}
                    </div>
                  </div>
                )}
                {startMode === 'new' && (
                  <div className="space-y-2">
                    <Label htmlFor="ai-new-project-name">First project</Label>
                    <Input
                      id="ai-new-project-name"
                      value={newProjectName}
                      onChange={(event) =>
                        setNewProjectName(event.target.value)
                      }
                      placeholder="storefront-web"
                    />
                    <p className="text-xs text-muted-foreground">
                      A deployable, Git-optional Temps project is created before
                      the application thread starts.
                    </p>
                  </div>
                )}
              </div>
              {startMode === 'new' && newProjectName.trim() && (
                <fieldset className="space-y-2">
                  <legend className="text-sm font-medium">
                    Development location
                  </legend>
                  <div className="grid gap-2 @container/development-choice sm:grid-cols-3">
                    <DevelopmentChoice
                      checked={newProjectMode === 'workspace'}
                      description="Persistent, isolated and wake-on-demand."
                      icon={HardDrive}
                      id="temps-workspace"
                      label="Temps workspace"
                      onSelect={() => setNewProjectMode('workspace')}
                    />
                    <DevelopmentChoice
                      checked={newProjectMode === 'manual'}
                      description="Create only the deploy target for now."
                      icon={TerminalSquare}
                      id="manual-project"
                      label="Manual project"
                      onSelect={() => setNewProjectMode('manual')}
                    />
                    <DevelopmentChoice
                      checked={newProjectMode === 'local'}
                      description="Keep code on this device with a local harness."
                      icon={FolderOpen}
                      id="local-folder"
                      label="Local folder"
                      onSelect={() => setNewProjectMode('local')}
                    />
                  </div>
                  {newProjectMode === 'workspace' && (
                    <p className="rounded-md border border-border bg-muted/50 p-3 text-xs text-muted-foreground">
                      Temps creates a project-scoped, persistent workspace. Its
                      filesystem is retained while idle and can be attached by
                      an authenticated host harness without exposing it to the
                      browser.
                    </p>
                  )}
                  {newProjectMode === 'local' && (
                    <div className="rounded-md border border-border bg-muted/50 p-3">
                      <div className="flex items-start justify-between gap-3">
                        <p className="text-xs text-muted-foreground">
                          The folder handle remains in this browser. Temps never
                          receives a filesystem path or directory contents; a
                          local Claude, Codex, or OpenCode harness must be
                          connected on this device before it can edit the
                          folder.
                        </p>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => void chooseLocalFolder()}
                        >
                          <FolderOpen className="mr-1.5 size-4" />
                          {localFolderName ? 'Change folder' : 'Choose folder'}
                        </Button>
                      </div>
                      {localFolderName && (
                        <p className="mt-2 text-xs font-medium">
                          Selected: {localFolderName}
                        </p>
                      )}
                    </div>
                  )}
                </fieldset>
              )}
              {error && <p className="text-sm text-destructive">{error}</p>}
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                disabled={
                  saving ||
                  startMode === null ||
                  !name.trim() ||
                  (startMode === 'existing' && selected.length === 0) ||
                  (startMode === 'new' && !newProjectName.trim()) ||
                  (startMode === 'new' &&
                    newProjectMode === 'local' &&
                    !localFolderName)
                }
                onClick={() => void submit()}
              >
                {saving && <Loader2 className="mr-1.5 size-4 animate-spin" />}{' '}
                Start application
              </Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}

function DevelopmentChoice({
  checked,
  description,
  icon: Icon,
  id,
  label,
  onSelect,
}: {
  checked: boolean
  description: string
  icon: typeof HardDrive
  id: string
  label: string
  onSelect: () => void
}) {
  return (
    <label
      htmlFor={id}
      className={cn(
        'flex cursor-pointer gap-2 rounded-md border p-3',
        checked ? 'border-primary bg-accent' : 'border-border bg-background'
      )}
    >
      <input
        checked={checked}
        className="mt-0.5 size-4 accent-primary"
        id={id}
        name="development-location"
        onChange={onSelect}
        type="radio"
      />
      <span className="min-w-0">
        <span className="flex items-center gap-1.5 text-sm font-medium">
          <Icon className="size-4 shrink-0" />
          {label}
        </span>
        <span className="mt-1 text-xs text-muted-foreground">
          {description}
        </span>
      </span>
    </label>
  )
}

function StartChoice({
  description,
  icon: Icon,
  label,
  onSelect,
}: {
  description: string
  icon: typeof Boxes
  label: string
  onSelect: () => void
}) {
  return (
    <button
      className="flex min-h-28 items-start gap-3 rounded-lg border border-border bg-background p-4 text-left hover:bg-accent"
      onClick={onSelect}
      type="button"
    >
      <Icon className="size-4 shrink-0 stroke-success" />
      <span className="min-w-0">
        <span className="block text-sm font-medium">{label}</span>
        <span className="mt-1 block text-xs text-muted-foreground">
          {description}
        </span>
      </span>
    </button>
  )
}

function CreateThreadDialog({
  application,
  providers,
  open,
  onOpenChange,
  onCreated,
}: {
  application: ApplicationResponse
  providers: ChatProviderOption[]
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreated: (conversation: ConversationResponse) => void
}) {
  const [selection, setSelection] = useState<ChatRuntimeSelection>(() =>
    resolveChatRuntimeSelection(providers, providers[0]?.id ?? 'gateway')
  )
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const provider = providers.find(
    (option) => option.id === selection.providerId
  )
  const model = provider?.models.find(
    (option) => option.id === selection.modelId
  )

  useEffect(() => {
    if (
      providers.length > 0 &&
      !providers.some((option) => option.id === selection.providerId)
    ) {
      const timer = window.setTimeout(
        () =>
          setSelection(resolveChatRuntimeSelection(providers, providers[0].id)),
        0
      )
      return () => window.clearTimeout(timer)
    }
  }, [providers, selection.providerId])

  const submit = async () => {
    setSaving(true)
    setError(null)
    try {
      const { data } = await createApplicationConversation({
        path: { application_public_id: application.public_id },
        body: {
          ai_provider: selection.providerId,
          ai_model: selection.modelId ?? null,
          ai_thinking_level: selection.thinkingOptionId ?? null,
          ai_permission_mode: selection.permissionModeId ?? null,
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
          <DialogTitle>New application thread</DialogTitle>
          <DialogDescription>
            Choose an authenticated host harness or a configured API provider.
            The harness is pinned to this thread.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="thread-provider">Provider</Label>
            <select
              id="thread-provider"
              value={selection.providerId}
              onChange={(event) =>
                setSelection(
                  resolveChatRuntimeSelection(providers, event.target.value)
                )
              }
              className="h-10 w-full rounded-md border bg-background px-3 text-sm"
            >
              {providers.map((option) => (
                <option key={option.id} value={option.id}>
                  {chatProviderLabel(option)}
                </option>
              ))}
            </select>
          </div>
          {provider && provider.models.length > 0 && (
            <div className="space-y-2">
              <Label htmlFor="thread-model">Model</Label>
              <select
                id="thread-model"
                value={selection.modelId ?? ''}
                onChange={(event) =>
                  setSelection(
                    resolveChatRuntimeSelection(providers, provider.id, {
                      ...selection,
                      modelId: event.target.value,
                    })
                  )
                }
                className="h-10 w-full rounded-md border bg-background px-3 text-sm"
              >
                {provider.models.map((option) => (
                  <option key={option.id} value={option.id}>
                    {option.name}
                  </option>
                ))}
              </select>
            </div>
          )}
          {model &&
            (model.tool_thinking_options ?? model.thinking_options).length >
              0 && (
              <div className="space-y-2">
                <Label htmlFor="thread-thinking">Reasoning</Label>
                <select
                  id="thread-thinking"
                  value={selection.thinkingOptionId ?? ''}
                  onChange={(event) =>
                    setSelection((current) => ({
                      ...current,
                      thinkingOptionId: event.target.value,
                    }))
                  }
                  className="h-10 w-full rounded-md border bg-background px-3 text-sm"
                >
                  {(model.tool_thinking_options ?? model.thinking_options).map(
                    (option) => (
                      <option key={option.id} value={option.id}>
                        {option.name}
                      </option>
                    )
                  )}
                </select>
              </div>
            )}
          {providers.length === 0 && (
            <p className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-sm text-amber-600">
              No AI runtime is ready. Authenticate Claude, Codex, or OpenCode on
              the host, or configure an API key in AI Gateway settings.
            </p>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            disabled={saving || providers.length === 0}
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
