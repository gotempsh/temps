// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getChatSandboxQueryKey,
  getChatWorkspaceQueryKey,
  leaveChatWorkspaceMutation,
  listApplicationsOptions,
  listApplicationsQueryKey,
  renameApplicationMutation,
  setChatWorkspaceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ApplicationResponse,
  ChatWorkspace,
  ChatWorkspaceResponse,
  WorkspaceChatSummary,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from '@tanstack/react-query'
import {
  Boxes,
  Check,
  Loader2,
  LogOut,
  MessageSquare,
  Pencil,
  Plus,
  X,
} from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { toast } from 'sonner'
import { ChatRunBadge } from './ChatRunBadge'
import { invalidateChatResources } from './chat-resources'
import {
  chatListChanged,
  invalidateBlueprint,
  openChat,
  startChatIn,
  useChatBlueprint,
} from './chat-blueprint'

/** Every chat's workspace query, so a rename shows in all of them at once. */
function invalidateChatWorkspaces(queryClient: QueryClient) {
  const sample = getChatWorkspaceQueryKey({ path: { public_id: '' } })[0]
  return queryClient.invalidateQueries({
    predicate: (query) => {
      const head = query.queryKey[0]
      return (
        typeof head === 'object' &&
        head !== null &&
        '_id' in head &&
        head._id === sample._id
      )
    },
  })
}

/**
 * The application (workspace) this chat belongs to. The chats of one
 * application share its sandbox, its projects and its architecture, so a new
 * chat about the same app picks up where the others left off.
 *
 * The popover answers three questions in order: which chats work on this
 * application (and a new one), what the application is called (rename it),
 * and where this chat belongs (move it, start an application with it, or
 * take it out).
 */
export function WorkspaceChip({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const queryClient = useQueryClient()
  const { workspace } = useChatBlueprint(conversationPublicId)
  const [open, setOpen] = useState(false)
  const applications = useQuery({
    ...listApplicationsOptions({ query: { page_size: 50 } }),
    enabled: open,
  })
  const settled = (data: ChatWorkspaceResponse, message: string) => {
    queryClient.setQueryData(
      getChatWorkspaceQueryKey({ path: { public_id: conversationPublicId } }),
      data
    )
    if (data.workspace) {
      invalidateBlueprint(queryClient, data.workspace.public_id)
    }
    // The chat now shares the application's sandbox, or has its own again.
    void queryClient.invalidateQueries({
      queryKey: getChatSandboxQueryKey({
        path: { public_id: conversationPublicId },
      }),
    })
    void invalidateChatResources(queryClient)
    void invalidateChatWorkspaces(queryClient)
    chatListChanged()
    toast.success(message)
    setOpen(false)
  }
  const join = useMutation({
    ...setChatWorkspaceMutation(),
    meta: { errorTitle: 'Could not move the chat' },
    onSuccess: (data) =>
      settled(
        data,
        data.workspace
          ? `This chat is now part of ${data.workspace.name}`
          : 'Moved'
      ),
  })
  const leave = useMutation({
    ...leaveChatWorkspaceMutation(),
    meta: { errorTitle: 'Could not take the chat out of the application' },
    onSuccess: (data) => settled(data, 'This chat is on its own again'),
  })
  const path = { public_id: conversationPublicId }
  const current = workspace.data?.workspace ?? null
  const busy = join.isPending || leave.isPending

  if (workspace.isPending) return <Skeleton className="h-8 w-28" />
  // A project chat works on its project alone.
  if (!workspace.data?.joinable) return null

  const others = (applications.data ?? []).filter(
    (application) => application.public_id !== current?.public_id
  )
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className={cn(
            'size-8',
            current ? 'text-foreground' : 'text-muted-foreground'
          )}
          title={
            current
              ? `Part of the application ${current.name}`
              : 'Group this chat with others about the same application'
          }
          aria-label={current ? `Application: ${current.name}` : 'Application'}
        >
          <Boxes className="size-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 space-y-4 p-3">
        {current ? (
          <>
            <ApplicationName application={current} />
            <ApplicationChats
              application={current}
              chats={workspace.data.chats ?? []}
              conversationPublicId={conversationPublicId}
              onLeavePopover={() => setOpen(false)}
            />
          </>
        ) : (
          <div>
            <p className="text-sm font-medium">Not part of an application</p>
            <p className="text-xs text-muted-foreground">
              Chats of one application share its sandbox, its projects and its
              architecture, so a new chat picks up where the others left off.
            </p>
          </div>
        )}
        <section
          aria-label={
            current ? 'Move this chat' : 'Add this chat to an application'
          }
          className="space-y-1 border-t pt-3"
        >
          <p className="text-xs font-medium text-muted-foreground">
            {current ? 'Move this chat to…' : 'Add this chat to…'}
          </p>
          <OtherApplications
            loading={applications.isPending}
            applications={others}
            busy={busy}
            onPick={(application) =>
              join.mutate({
                path,
                body: {
                  application_id: application.public_id,
                  keep_sandbox: 'workspace',
                },
              })
            }
          />
          <NewApplicationForm
            busy={busy}
            pending={join.isPending}
            onCreate={(name) =>
              join.mutate({
                path,
                body: { create: { name }, keep_sandbox: 'chat' },
              })
            }
          />
          {current && (
            <button
              type="button"
              disabled={busy}
              className={cn(
                'flex w-full items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-muted',
                busy && 'opacity-60'
              )}
              onClick={() => leave.mutate({ path })}
            >
              {leave.isPending ? (
                <Loader2 className="size-3.5 shrink-0 animate-spin" />
              ) : (
                <LogOut className="size-3.5 shrink-0 text-muted-foreground" />
              )}
              <span className="min-w-0 flex-1 truncate">
                Out of {current.name}, on its own
              </span>
            </button>
          )}
        </section>
      </PopoverContent>
    </Popover>
  )
}

/** The application's name, with a pencil that renames it in place. */
function ApplicationName({ application }: { application: ChatWorkspace }) {
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState(false)
  const [name, setName] = useState(application.name)
  const rename = useMutation({
    ...renameApplicationMutation(),
    meta: { errorTitle: 'Could not rename the application' },
    onSuccess: (renamed) => {
      void invalidateChatWorkspaces(queryClient)
      void queryClient.invalidateQueries({
        queryKey: listApplicationsQueryKey(),
      })
      chatListChanged()
      toast.success(`Renamed to ${renamed.name}`)
      setEditing(false)
    },
  })
  const trimmed = name.trim()
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!trimmed || trimmed === application.name) {
      setEditing(false)
      return
    }
    rename.mutate({
      path: { application_public_id: application.public_id },
      body: { name: trimmed },
    })
  }

  if (editing) {
    return (
      <form className="flex items-center gap-1" onSubmit={submit}>
        <Input
          value={name}
          onChange={(event) => setName(event.target.value)}
          aria-label="Application name"
          maxLength={200}
          autoFocus
          className="h-8 text-sm"
        />
        <Button
          type="submit"
          size="icon"
          variant="ghost"
          className="size-8 shrink-0"
          disabled={rename.isPending || !trimmed}
          aria-label="Save the name"
        >
          {rename.isPending ? (
            <Loader2 className="size-3.5 animate-spin" />
          ) : (
            <Check className="size-3.5" />
          )}
        </Button>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-8 shrink-0"
          onClick={() => setEditing(false)}
          aria-label="Keep the current name"
        >
          <X className="size-3.5" />
        </Button>
      </form>
    )
  }
  return (
    <div>
      <div className="flex items-center gap-1">
        <Boxes className="size-4 shrink-0 text-muted-foreground" />
        <p className="min-w-0 truncate text-sm font-medium">
          {application.name}
        </p>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7 shrink-0"
          onClick={() => {
            setName(application.name)
            setEditing(true)
          }}
          aria-label={`Rename ${application.name}`}
          title="Rename the application"
        >
          <Pencil className="size-3.5" />
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Its chats share one sandbox, its projects and its architecture.
      </p>
    </div>
  )
}

/** Every chat of the application, this one included, and a new one. */
function ApplicationChats({
  application,
  chats,
  conversationPublicId,
  onLeavePopover,
}: {
  application: ChatWorkspace
  chats: readonly WorkspaceChatSummary[]
  conversationPublicId: string
  onLeavePopover: () => void
}) {
  return (
    <section aria-label={`Chats in ${application.name}`} className="space-y-1">
      <p className="text-xs font-medium text-muted-foreground">
        Chats · {chats.length}
      </p>
      <ul className="max-h-48 overflow-y-auto">
        {chats.map((chat) => (
          <li key={chat.public_id}>
            <ApplicationChatRow
              chat={chat}
              isCurrent={chat.public_id === conversationPublicId}
              onOpen={() => {
                onLeavePopover()
                openChat(chat.public_id)
              }}
            />
          </li>
        ))}
      </ul>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="h-8 w-full justify-start gap-1.5 text-xs"
        onClick={() => {
          onLeavePopover()
          startChatIn(application)
        }}
      >
        <Plus className="size-3.5" />
        <span className="truncate">New chat in {application.name}</span>
      </Button>
    </section>
  )
}

function ApplicationChatRow({
  chat,
  isCurrent,
  onOpen,
}: {
  chat: WorkspaceChatSummary
  isCurrent: boolean
  onOpen: () => void
}) {
  const content = (
    <>
      <MessageSquare className="size-3.5 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate">
        {chat.title ?? 'Untitled chat'}
      </span>
      <ChatRunBadge turnStatus={chat.turn_status} compact />
      <span className="shrink-0 text-muted-foreground">
        {isCurrent ? 'this chat' : <TimeAgo date={chat.last_activity_at} />}
      </span>
    </>
  )
  if (isCurrent) {
    return (
      <div
        aria-current="true"
        className="flex w-full items-center gap-2 rounded bg-muted px-2 py-1 text-xs"
      >
        {content}
      </div>
    )
  }
  return (
    <button
      type="button"
      className="flex w-full items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-muted"
      onClick={onOpen}
    >
      {content}
    </button>
  )
}

function OtherApplications({
  loading,
  applications,
  busy,
  onPick,
}: {
  loading: boolean
  applications: readonly ApplicationResponse[]
  busy: boolean
  onPick: (application: ApplicationResponse) => void
}) {
  if (loading) {
    return (
      <div className="space-y-1">
        <Skeleton className="h-6 w-full" />
        <Skeleton className="h-6 w-2/3" />
      </div>
    )
  }
  if (applications.length === 0) return null
  return (
    <ul className="max-h-40 overflow-y-auto">
      {applications.map((application) => (
        <li key={application.public_id}>
          <button
            type="button"
            disabled={busy}
            className={cn(
              'flex w-full items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-muted',
              busy && 'opacity-60'
            )}
            onClick={() => onPick(application)}
          >
            <Boxes className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 truncate">{application.name}</span>
          </button>
        </li>
      ))}
    </ul>
  )
}

/**
 * "A new application…": one click opens the name field, so the popover never
 * shows an unlabelled box that reads like a rename.
 */
function NewApplicationForm({
  busy,
  pending,
  onCreate,
}: {
  busy: boolean
  pending: boolean
  onCreate: (name: string) => void
}) {
  const [editing, setEditing] = useState(false)
  const [name, setName] = useState('')
  const trimmed = name.trim()

  if (!editing) {
    return (
      <button
        type="button"
        disabled={busy}
        className={cn(
          'flex w-full items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-muted',
          busy && 'opacity-60'
        )}
        onClick={() => setEditing(true)}
      >
        <Plus className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate">A new application…</span>
      </button>
    )
  }
  return (
    <form
      className="flex items-center gap-1 px-1"
      onSubmit={(event) => {
        event.preventDefault()
        if (trimmed) onCreate(trimmed)
      }}
    >
      <Input
        value={name}
        onChange={(event) => setName(event.target.value)}
        placeholder="Name of the new application"
        aria-label="Name of the new application"
        maxLength={200}
        autoFocus
        className="h-8 text-xs"
      />
      <Button
        type="submit"
        size="sm"
        variant="outline"
        className="h-8 shrink-0 gap-1"
        disabled={busy || !trimmed}
      >
        {pending ? <Loader2 className="size-3.5 animate-spin" /> : null}
        Create
      </Button>
    </form>
  )
}
