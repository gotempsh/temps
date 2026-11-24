// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getChatWorkspaceQueryKey,
  saveBlueprintVersionMutation,
  setChatWorkspaceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  BlueprintDocument,
  BlueprintResponse,
  Component,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  ExternalLink,
  Network,
  Pencil,
  Rocket,
  Sparkles,
  type LucideIcon,
} from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { BlueprintDiagram } from './BlueprintDiagram'
import { BlueprintEditor } from './BlueprintEditor'
import { BlueprintProposalCard } from './BlueprintProposalCard'
import { savedProblems } from './blueprint-editor'
import {
  LIVE_LABELS,
  LIVE_TONES,
  componentDetail,
  continueChat,
  deployAllMessage,
  editedMessage,
  invalidateBlueprint,
  storeBlueprint,
  useChatBlueprint,
} from './chat-blueprint'

/** What the assistant is asked when the user wants a design. */
export const DESIGN_REQUEST =
  'Design the architecture of what we are building and propose it before building anything.'

/** A first draft for a user who would rather draw the design themselves. */
const STARTER: BlueprintDocument = {
  summary: '',
  components: [
    {
      id: 'browser',
      name: 'Browser',
      purpose: 'Where people use the app',
      kind: 'client',
      surface: 'browser',
    },
    {
      id: 'web',
      name: 'Web app',
      purpose: 'Serves the pages and the API',
      kind: 'app',
      role: 'web',
      runtime: 'preset',
      stack: 'Vite + Bun',
    },
  ],
  flows: [
    { from: 'browser', to: 'web', kind: 'http', label: 'pages and /api' },
  ],
}

function Onboarding({
  icon: Icon,
  title,
  children,
}: {
  icon: LucideIcon
  title: string
  children: ReactNode
}) {
  return (
    <div className="space-y-3 p-4">
      <div className="flex items-start gap-2">
        <Icon
          className="mt-0.5 size-4 shrink-0 text-muted-foreground"
          aria-hidden
        />
        <div className="min-w-0 space-y-1">
          <p className="text-sm font-medium">{title}</p>
          {children}
        </div>
      </div>
    </div>
  )
}

/** What an architecture is, for a chat that has none yet. */
function DesignOnboarding() {
  return (
    <Onboarding
      icon={Network}
      title="Design the application before building it"
    >
      <p className="text-sm text-muted-foreground">
        The architecture names every part (pages, an API, workers, a database, a
        broker, outside services) and how data flows between them. You accept it
        before anything is built, and every chat of the application builds
        against it. For example: a Vite web app that calls a Bun API, which
        keeps tasks in Postgres and sends email through an outside service.
      </p>
    </Onboarding>
  )
}

/** A save the user sent, kept until it lands so a refused one is not lost. */
interface SubmittedDraft {
  document: BlueprintDocument
  changeSummary: string
}

/**
 * The application's architecture: the parts it is made of, how data flows
 * between them, and how each built part is doing. The assistant proposes it
 * and the user accepts it; every chat of the workspace builds against the
 * accepted version, and the user can redraw it here.
 */
export function ArchitectureTab({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const queryClient = useQueryClient()
  const { workspace, applicationId, blueprint } =
    useChatBlueprint(conversationPublicId)
  const [editing, setEditing] = useState(false)
  const [workspaceName, setWorkspaceName] = useState('')
  const [selected, setSelected] = useState<string | null>(null)
  const [submitted, setSubmitted] = useState<SubmittedDraft | null>(null)
  const createWorkspace = useMutation({
    ...setChatWorkspaceMutation(),
    meta: { errorTitle: 'Could not create the application' },
  })
  const save = useMutation({
    ...saveBlueprintVersionMutation(),
    onSuccess: (data, variables) => {
      storeBlueprint(queryClient, data)
      invalidateBlueprint(
        queryClient,
        data.application_id,
        conversationPublicId
      )
      setSubmitted(null)
      setEditing(false)
      const version = data.accepted?.version
      toast.success(
        version ? `Saved as version ${version}` : 'Architecture saved'
      )
      if (version) {
        continueChat(
          conversationPublicId,
          editedMessage(version, variables.body.change_summary)
        )
      }
    },
  })
  const saveDocument = async (
    document: BlueprintDocument,
    changeSummary: string
  ) => {
    setSubmitted({ document, changeSummary })
    let target = applicationId
    if (!target) {
      const name = workspaceName.trim()
      if (!name) {
        toast.error('Name the application first')
        return
      }
      const created = await createWorkspace
        .mutateAsync({
          path: { public_id: conversationPublicId },
          body: { create: { name }, keep_sandbox: 'chat' },
        })
        .catch(() => null)
      target = created?.workspace?.public_id ?? null
      if (!created || !target) return
      // The chat now has an application: show it at once rather than after
      // the workspace read comes back.
      queryClient.setQueryData(
        getChatWorkspaceQueryKey({ path: { public_id: conversationPublicId } }),
        created
      )
      invalidateBlueprint(queryClient, target, conversationPublicId)
    }
    save.mutate({
      path: { public_id: target },
      body: { document, change_summary: changeSummary },
    })
  }

  const chatWorkspace = workspace.data
  if (!chatWorkspace && workspace.isPending) {
    return (
      <div className="space-y-3 p-4">
        <Skeleton className="h-4 w-48" />
        <Skeleton className="h-40 w-full" />
      </div>
    )
  }
  // A failed refetch keeps the data it had; only a chat that never loaded
  // shows the error, so an open editor is not torn down by a blip.
  if (!chatWorkspace) {
    return (
      <Onboarding icon={Network} title="The architecture could not be loaded">
        <p className="text-sm text-muted-foreground">
          {(workspace.error as { detail?: string })?.detail ??
            'Try again in a moment.'}
        </p>
      </Onboarding>
    )
  }
  if (!chatWorkspace.joinable) {
    return (
      <Onboarding icon={Network} title="No architecture in a project chat">
        <p className="text-sm text-muted-foreground">{chatWorkspace.reason}</p>
      </Onboarding>
    )
  }

  const data = blueprint.data
  const accepted = data?.accepted ?? null
  const proposal = data?.proposal ?? null
  const hasDesign = accepted !== null || proposal !== null

  // One layout for the whole editing session, whatever the queries are doing:
  // saving into a new application refetches the chat's workspace and loads a
  // blueprint, and the editor (with the user's drawing in its state) must not
  // unmount while that happens, or a refused save would come back as the
  // starter design with errors describing a drawing that is gone.
  if (editing) {
    return (
      <div className="space-y-4 p-4">
        {hasDesign && data ? (
          <p className="text-sm font-medium">
            Edit the architecture of {data.workspace_name}
          </p>
        ) : (
          <DesignOnboarding />
        )}
        <div className={cn('space-y-3', !hasDesign && 'px-4')}>
          {!applicationId && (
            <div className="space-y-1">
              <Label htmlFor="new-application-name" className="text-xs">
                Application name
              </Label>
              <Input
                id="new-application-name"
                value={workspaceName}
                placeholder="Team tasks"
                onChange={(event) => setWorkspaceName(event.target.value)}
                className="h-8 text-xs"
              />
            </div>
          )}
          <BlueprintEditor
            initial={
              submitted?.document ??
              accepted?.document ??
              proposal?.document ??
              STARTER
            }
            initialChangeSummary={submitted?.changeSummary}
            saving={save.isPending || createWorkspace.isPending}
            problems={savedProblems(save.error)}
            onSave={(document, summary) => void saveDocument(document, summary)}
            onCancel={() => {
              save.reset()
              setSubmitted(null)
              setEditing(false)
            }}
          />
        </div>
      </div>
    )
  }

  if (!applicationId || (data && !hasDesign)) {
    return (
      <div className="space-y-4 p-4">
        <DesignOnboarding />
        <div className="flex flex-wrap gap-2 px-4">
          <Button
            type="button"
            size="sm"
            className="gap-1.5"
            onClick={() => continueChat(conversationPublicId, DESIGN_REQUEST)}
          >
            <Sparkles className="size-4" />
            Ask the assistant to design it
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="gap-1.5"
            onClick={() => setEditing(true)}
          >
            <Pencil className="size-4" />
            Draw it myself
          </Button>
        </div>
      </div>
    )
  }

  if (blueprint.isPending || !data) {
    return (
      <div className="space-y-3 p-4">
        <Skeleton className="h-4 w-48" />
        <Skeleton className="h-40 w-full" />
      </div>
    )
  }

  const selectedPart =
    accepted?.document.components.find((c) => c.id === selected) ?? null
  const deployAll = accepted ? deployAllMessage(accepted.document) : null
  return (
    <div className="space-y-4 p-4">
      {proposal && (
        <BlueprintProposalCard
          conversationPublicId={conversationPublicId}
          receipt={{
            applicationId: data.application_id,
            workspaceName: data.workspace_name,
            workspaceCreated: false,
            version: proposal.version,
            changeSummary: proposal.change_summary,
            document: proposal.document,
          }}
        />
      )}
      {accepted && (
        <section className="space-y-3" aria-label="Accepted architecture">
          <header className="flex flex-wrap items-start gap-2">
            <div className="min-w-0 flex-1 basis-64">
              <p className="text-sm font-medium">
                {data.workspace_name}
                <span className="font-normal text-muted-foreground">
                  {' '}
                  · version {accepted.version}
                  {accepted.decided_at && (
                    <>
                      {' '}
                      · accepted <TimeAgo date={accepted.decided_at} />
                    </>
                  )}
                </span>
              </p>
              <p
                className="line-clamp-3 text-sm text-muted-foreground"
                title={accepted.document.summary}
              >
                {accepted.document.summary}
              </p>
            </div>
            <div className="flex shrink-0 gap-2">
              <Button
                type="button"
                size="sm"
                variant="outline"
                className="h-8 gap-1.5"
                onClick={() => setEditing(true)}
              >
                <Pencil className="size-4" />
                <span className="max-sm:sr-only">Edit</span>
              </Button>
              <Button
                type="button"
                size="sm"
                className="h-8 gap-1.5"
                disabled={!deployAll}
                title={
                  deployAll
                    ? 'Ask the assistant to deploy every app part, APIs first'
                    : 'The architecture has no app parts to deploy'
                }
                onClick={() =>
                  deployAll && continueChat(conversationPublicId, deployAll)
                }
              >
                <Rocket className="size-4" />
                Deploy all
              </Button>
            </div>
          </header>
          <BlueprintDiagram
            document={accepted.document}
            live={data.live}
            selected={selected}
            onSelect={(id) => setSelected(id === selected ? null : id)}
          />
          {selectedPart ? (
            <PartDetails part={selectedPart} blueprint={data} />
          ) : (
            <p className="text-xs text-muted-foreground">
              Select a part to see what realises it and how it is doing.
            </p>
          )}
        </section>
      )}
    </div>
  )
}

function PartDetails({
  part,
  blueprint,
}: {
  part: Component
  blueprint: BlueprintResponse
}) {
  const binding = blueprint.bindings[part.id]
  const live = blueprint.live[part.id]
  const href = live?.project_slug
    ? `/projects/${live.project_slug}`
    : binding?.service_id
      ? `/storage/${binding.service_id}`
      : null
  return (
    <section
      aria-label={`${part.name} details`}
      className="space-y-2 rounded-md border p-3 text-sm"
    >
      <div className="flex flex-wrap items-center gap-2">
        <p className="min-w-0 flex-1 font-medium">{part.name}</p>
        {live && (
          <span
            className={cn(
              'rounded px-1.5 py-0.5 text-[11px] font-medium',
              LIVE_TONES[live.status]
            )}
          >
            {LIVE_LABELS[live.status]}
          </span>
        )}
      </div>
      <p className="text-muted-foreground">{part.purpose}</p>
      <p className="text-xs text-muted-foreground">{componentDetail(part)}</p>
      {binding ? (
        href ? (
          <Link
            to={href}
            className="inline-flex items-center gap-1 text-xs font-medium underline-offset-4 hover:underline"
          >
            {live?.name ?? 'Open in the console'}
            <ExternalLink className="size-3" />
          </Link>
        ) : (
          <p className="text-xs text-muted-foreground">
            {live?.name ?? 'Bound'}
          </p>
        )
      ) : part.kind === 'app' || part.kind === 'data' ? (
        <p className="text-xs text-muted-foreground">
          Not built yet. The assistant binds it when it creates the{' '}
          {part.kind === 'app' ? 'project' : 'service'}.
        </p>
      ) : null}
    </section>
  )
}
