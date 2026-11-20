// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SandboxEntry } from '@/api/client'
import {
  getChatSandboxOptions,
  listChatSandboxFilesOptions,
  readChatSandboxFileOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useRefetchWhenShown } from '@/hooks/useRefetchWhenShown'
import { getErrorMessage } from '@/utils/errorHandling'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ChevronRight,
  File,
  FileSymlink,
  Folder,
  FolderOpen,
  FolderTree,
  RefreshCw,
} from 'lucide-react'
import { lazy, Suspense, useState, type ReactNode } from 'react'
import { SandboxWaking } from './ChatPreviewTab'
import { baseName, formatFileSize, relativeTo } from './chat-files'
import { CreateSandboxAction } from './ChatSandbox'
import { useChatSandboxWake } from './chat-sandbox-wake'

// Monaco is large and only chats that open a file need it.
const ChatCodeViewer = lazy(() =>
  import('./ChatCodeViewer').then((module) => ({
    default: module.ChatCodeViewer,
  }))
)

/** While the tab is shown, how often it picks up what the assistant wrote. */
const ACTIVE_REFRESH_MS = 10_000

export function PanelSkeleton({ label }: { label: string }) {
  return (
    <div className="space-y-3 p-4" role="status" aria-label={label}>
      <Skeleton className="h-6 w-2/3" />
      <Skeleton className="h-6 w-1/2" />
      <Skeleton className="h-6 w-3/5" />
    </div>
  )
}

/** What the tab shows when there is no sandbox to browse. */
export function FilesOnboarding({ reason }: { reason: string }) {
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <FolderTree className="size-7 text-muted-foreground" aria-hidden="true" />
      <p className="text-sm font-medium">No files yet</p>
      <p className="max-w-xs text-sm text-muted-foreground">
        {reason} Once it has one, every file the assistant writes, such as{' '}
        <code className="font-mono text-xs">src/app/page.tsx</code>, can be
        opened here.
      </p>
    </div>
  )
}

/**
 * Renders `children` once the chat's sandbox exists and is running, and the
 * matching onboarding or waking state until then. Shared by the Files and
 * Changes tabs, which both read from the sandbox.
 */
export function ChatSandboxGate({
  conversationPublicId,
  onboarding,
  children,
}: {
  conversationPublicId: string
  onboarding: (reason: string) => ReactNode
  children: ReactNode
}) {
  const status = useQuery(
    getChatSandboxOptions({ path: { public_id: conversationPublicId } })
  )
  const wake = useChatSandboxWake(conversationPublicId)
  if (status.isPending) return <PanelSkeleton label="Loading" />
  if (status.isError) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Temps could not load this chat&rsquo;s sandbox. It will try again
        shortly.
      </p>
    )
  }
  const { configured, linked, reason } = status.data
  if (!configured) {
    return onboarding(reason ?? 'This chat cannot use a sandbox.')
  }
  if (!linked) {
    return (
      <>
        {onboarding(
          'This chat has no sandbox yet. When the assistant builds something, it runs in one.'
        )}
        <CreateSandboxAction conversationPublicId={conversationPublicId} />
      </>
    )
  }
  if (wake.phase !== 'ready') {
    return <SandboxWaking error={wake.error} onRetry={wake.retry} />
  }
  return children
}

/** Every query this tab made, so Refresh reloads the tree and the open file. */
function isFilesQuery(queryKey: readonly unknown[]): boolean {
  const id = (queryKey[0] as { _id?: string } | undefined)?._id
  return id === 'listChatSandboxFiles' || id === 'readChatSandboxFile'
}

function EntryIcon({ entry, open }: { entry: SandboxEntry; open: boolean }) {
  const className = 'size-3.5 shrink-0 text-muted-foreground'
  if (entry.kind === 'directory') {
    return open ? (
      <FolderOpen className={className} aria-hidden="true" />
    ) : (
      <Folder className={className} aria-hidden="true" />
    )
  }
  if (entry.kind === 'symlink') {
    return <FileSymlink className={className} aria-hidden="true" />
  }
  return <File className={className} aria-hidden="true" />
}

interface TreeProps {
  conversationPublicId: string
  active: boolean
  expanded: ReadonlySet<string>
  selected: string | null
  onToggle: (path: string) => void
  onSelect: (path: string) => void
}

/** One directory's entries; subdirectories load when they are opened. */
function DirectoryEntries({
  path,
  depth,
  ...tree
}: TreeProps & { path: string | null; depth: number }) {
  const listing = useQuery({
    ...listChatSandboxFilesOptions({
      path: { public_id: tree.conversationPublicId },
      query: path === null ? undefined : { path },
    }),
    refetchInterval: tree.active ? ACTIVE_REFRESH_MS : false,
    refetchOnWindowFocus: tree.active,
  })
  useRefetchWhenShown(listing.refetch, tree.active)
  const indent = { paddingLeft: `${depth * 12 + 8}px` }

  if (listing.isPending) {
    return (
      <div className="space-y-1 py-1 pr-2" style={indent}>
        <Skeleton className="h-4 w-3/4" />
        <Skeleton className="h-4 w-1/2" />
      </div>
    )
  }
  if (listing.isError) {
    return (
      <p className="py-1 pr-2 text-xs text-destructive" style={indent}>
        {getErrorMessage(listing.error)}
      </p>
    )
  }
  const { entries, truncated } = listing.data
  if (entries.length === 0) {
    return (
      <p className="py-1 pr-2 text-xs text-muted-foreground" style={indent}>
        Empty folder
      </p>
    )
  }
  return (
    <ul role={depth === 0 ? 'tree' : 'group'} aria-label="Sandbox files">
      {entries.map((entry) => {
        const isDirectory = entry.kind === 'directory'
        const open = isDirectory && tree.expanded.has(entry.path)
        const isSelected = tree.selected === entry.path
        return (
          <li
            key={entry.path}
            role="treeitem"
            aria-expanded={isDirectory ? open : undefined}
            aria-selected={isSelected}
          >
            <button
              type="button"
              onClick={() =>
                isDirectory
                  ? tree.onToggle(entry.path)
                  : tree.onSelect(entry.path)
              }
              className={cn(
                'flex w-full items-center gap-1.5 py-0.5 pr-2 text-left text-xs hover:bg-muted',
                isSelected && 'bg-muted font-medium'
              )}
              style={indent}
              title={entry.path}
            >
              <ChevronRight
                className={cn(
                  'size-3 shrink-0 text-muted-foreground transition-transform',
                  !isDirectory && 'invisible',
                  open && 'rotate-90'
                )}
                aria-hidden="true"
              />
              <EntryIcon entry={entry} open={open} />
              <span className="truncate">{entry.name}</span>
            </button>
            {open && (
              <DirectoryEntries {...tree} path={entry.path} depth={depth + 1} />
            )}
          </li>
        )
      })}
      {truncated && (
        <li className="py-1 pr-2 text-xs text-muted-foreground" style={indent}>
          Only the first {entries.length} entries are shown.
        </li>
      )}
    </ul>
  )
}

/** The open file, highlighted, or why it cannot be shown. */
function FileView({
  conversationPublicId,
  path,
  active,
}: {
  conversationPublicId: string
  path: string
  active: boolean
}) {
  const file = useQuery({
    ...readChatSandboxFileOptions({
      path: { public_id: conversationPublicId },
      query: { path },
    }),
    refetchInterval: active ? ACTIVE_REFRESH_MS : false,
    refetchOnWindowFocus: active,
  })
  useRefetchWhenShown(file.refetch, active)
  if (file.isPending) return <PanelSkeleton label="Loading file" />
  if (file.isError) {
    return (
      <p role="alert" className="p-4 text-sm text-muted-foreground">
        {getErrorMessage(file.error)}
      </p>
    )
  }
  const { binary, content, size, truncated } = file.data
  if (binary || content === null || content === undefined) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {baseName(path)} is a binary file ({formatFileSize(size)}), so it is not
        shown as text.
      </p>
    )
  }
  return (
    <div className="flex h-full min-h-0 flex-col">
      {truncated && (
        <p className="shrink-0 border-b px-3 py-1.5 text-xs text-muted-foreground">
          This file is {formatFileSize(size)}; only the first part is shown.
        </p>
      )}
      <div className="min-h-0 flex-1">
        <Suspense fallback={<PanelSkeleton label="Loading editor" />}>
          <ChatCodeViewer path={path} content={content} />
        </Suspense>
      </div>
    </div>
  )
}

function FilesBrowser({
  conversationPublicId,
  active,
}: {
  conversationPublicId: string
  active: boolean
}) {
  const queryClient = useQueryClient()
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set())
  const [selected, setSelected] = useState<string | null>(null)
  // The root listing names the work directory the relative paths start at.
  const root = useQuery(
    listChatSandboxFilesOptions({ path: { public_id: conversationPublicId } })
  )
  const workDir = root.data?.work_dir ?? null
  const toggle = (path: string) =>
    setExpanded((current) => {
      const next = new Set(current)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  const refresh = () =>
    queryClient.invalidateQueries({
      predicate: (query) => isFilesQuery(query.queryKey),
    })

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b px-3 py-1.5">
        <p
          className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground"
          title={selected ?? workDir ?? undefined}
        >
          {selected && workDir
            ? relativeTo(workDir, selected)
            : (workDir ?? 'Workspace')}
        </p>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7 shrink-0"
          onClick={refresh}
          aria-label="Reload the files"
        >
          <RefreshCw className="size-3.5" />
        </Button>
      </div>
      <div className="@container flex min-h-0 flex-1">
        <div className="flex min-h-0 w-full flex-col @lg:flex-row">
          <nav
            aria-label="Files in the sandbox"
            className="max-h-48 shrink-0 overflow-auto border-b py-1 @lg:max-h-none @lg:w-56 @lg:border-r @lg:border-b-0"
          >
            <DirectoryEntries
              conversationPublicId={conversationPublicId}
              active={active}
              expanded={expanded}
              selected={selected}
              onToggle={toggle}
              onSelect={setSelected}
              path={null}
              depth={0}
            />
          </nav>
          <div className="min-h-0 min-w-0 flex-1">
            {selected === null ? (
              <p className="p-4 text-sm text-muted-foreground">
                Pick a file to read it. The view follows the assistant&rsquo;s
                edits while this tab is open.
              </p>
            ) : (
              <FileView
                key={selected}
                conversationPublicId={conversationPublicId}
                path={selected}
                active={active}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

/** The chat's sandbox as a file tree with a read-only viewer. */
export function ChatFilesTab({
  conversationPublicId,
  active,
}: {
  conversationPublicId: string
  active: boolean
}) {
  const [opened, setOpened] = useState(active)
  if (active && !opened) setOpened(true)
  return (
    <ChatSandboxGate
      conversationPublicId={conversationPublicId}
      onboarding={(reason) => <FilesOnboarding reason={reason} />}
    >
      {/* Nothing runs in the sandbox until the tab is first shown. */}
      {opened && (
        <FilesBrowser
          conversationPublicId={conversationPublicId}
          active={active}
        />
      )}
    </ChatSandboxGate>
  )
}
