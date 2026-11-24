// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GitChangedFile, GitRepoChanges } from '@/api/client'
import {
  getChatSandboxChangesOptions,
  getChatSandboxChangesQueryKey,
  getChatSandboxDiffOptions,
  trackChatSandboxChangesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { useRefetchWhenShown } from '@/hooks/useRefetchWhenShown'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { GitBranch, GitCompare, Loader2, RefreshCw } from 'lucide-react'
import { lazy, Suspense, useState } from 'react'
import { toast } from 'sonner'
import { ChatSandboxGate, PanelSkeleton } from './ChatFilesTab'
import { baseName, changeBadge, countChanges, relativeTo } from './chat-files'

const ChatDiffViewer = lazy(() =>
  import('./ChatCodeViewer').then((module) => ({
    default: module.ChatDiffViewer,
  }))
)

/** While the tab is shown, how often it picks up what the assistant changed. */
const ACTIVE_REFRESH_MS = 5_000

/** What the tab shows when there is no sandbox to compare. */
export function ChangesOnboarding({ reason }: { reason: string }) {
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <GitCompare className="size-7 text-muted-foreground" aria-hidden="true" />
      <p className="text-sm font-medium">No changes to review yet</p>
      <p className="max-w-xs text-sm text-muted-foreground">
        {reason} Once it has one, every edit the assistant makes shows here as a
        diff against the last commit, for example the lines it changed in{' '}
        <code className="font-mono text-xs">src/app/page.tsx</code>.
      </p>
    </div>
  )
}

/**
 * The workspace is not a Git repository, so there is nothing to diff against.
 * Offers to snapshot it now so later edits show as changes.
 */
export function TrackChangesPrompt({
  gitAvailable,
  pending,
  onTrack,
}: {
  gitAvailable: boolean
  pending: boolean
  onTrack: () => void
}) {
  if (!gitAvailable) {
    return (
      <div className="flex flex-col items-center gap-2 p-6 text-center">
        <GitCompare
          className="size-7 text-muted-foreground"
          aria-hidden="true"
        />
        <p className="text-sm font-medium">Git is not installed</p>
        <p className="max-w-xs text-sm text-muted-foreground">
          Changes are compared with Git, and this sandbox&rsquo;s image does not
          include it. Ask the assistant to install it (for example{' '}
          <code className="font-mono text-xs">apt-get install -y git</code>),
          then reload this tab.
        </p>
      </div>
    )
  }
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <GitCompare className="size-7 text-muted-foreground" aria-hidden="true" />
      <p className="text-sm font-medium">Changes are not tracked yet</p>
      <p className="max-w-xs text-sm text-muted-foreground">
        The workspace is not a Git repository. Start tracking to save what is
        there now as a first commit; every edit after that shows here as a diff.
        Dependencies, build output and <code className="font-mono">.env</code>{' '}
        files are left out.
      </p>
      <Button type="button" size="sm" onClick={onTrack} disabled={pending}>
        {pending && <Loader2 className="size-4 animate-spin" />}
        Start tracking changes
      </Button>
    </div>
  )
}

function fileKey(repo: GitRepoChanges, file: GitChangedFile): string {
  return `${repo.root}\u0000${file.path}`
}

interface Selection {
  repo: string
  file: GitChangedFile
}

function ChangedFileRow({
  repo,
  file,
  selected,
  onSelect,
}: {
  repo: GitRepoChanges
  file: GitChangedFile
  selected: boolean
  onSelect: (selection: Selection) => void
}) {
  const badge = changeBadge(file.change)
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect({ repo: repo.root, file })}
        aria-current={selected ? 'true' : undefined}
        className={cn(
          'flex w-full items-center gap-2 px-3 py-1 text-left text-xs hover:bg-muted',
          selected && 'bg-muted font-medium'
        )}
        title={file.old_path ? `${file.old_path} → ${file.path}` : file.path}
      >
        <span
          className={cn(
            'w-3 shrink-0 font-mono font-semibold',
            badge.className
          )}
          title={badge.label}
        >
          {badge.letter}
          <span className="sr-only"> {badge.label}</span>
        </span>
        <span className="truncate">{baseName(file.path)}</span>
        <span className="ml-auto truncate pl-2 text-muted-foreground">
          {file.path.includes('/')
            ? file.path.slice(0, file.path.lastIndexOf('/'))
            : ''}
        </span>
      </button>
    </li>
  )
}

/** A repository by its folder: the work directory's own repo by its name. */
function repoLabel(repo: GitRepoChanges, workDir: string): string {
  const relative = relativeTo(workDir, repo.root)
  return relative === '.' ? baseName(repo.root) : relative
}

function RepoChanges({
  repo,
  workDir,
  selectedKey,
  onSelect,
}: {
  repo: GitRepoChanges
  workDir: string
  selectedKey: string | null
  onSelect: (selection: Selection) => void
}) {
  return (
    <section aria-label={`Changes in ${repo.name}`}>
      <h3 className="flex items-center gap-1.5 px-3 pt-2 pb-1 text-xs font-medium">
        <span className="truncate" title={repo.root}>
          {repoLabel(repo, workDir)}
        </span>
        {repo.branch && (
          <span className="flex items-center gap-0.5 font-normal text-muted-foreground">
            <GitBranch className="size-3" aria-hidden="true" />
            {repo.branch}
          </span>
        )}
      </h3>
      {repo.files.length === 0 ? (
        <p className="px-3 py-1 text-xs text-muted-foreground">
          {repo.has_commits
            ? 'No changes since the last commit.'
            : 'No commits yet and no files.'}
        </p>
      ) : (
        <ul>
          {repo.files.map((file) => (
            <ChangedFileRow
              key={fileKey(repo, file)}
              repo={repo}
              file={file}
              selected={selectedKey === fileKey(repo, file)}
              onSelect={onSelect}
            />
          ))}
        </ul>
      )}
      {repo.truncated && (
        <p className="px-3 py-1 text-xs text-muted-foreground">
          Only the first {repo.files.length} changed files are listed.
        </p>
      )}
    </section>
  )
}

function FileDiff({
  conversationPublicId,
  selection,
  active,
}: {
  conversationPublicId: string
  selection: Selection
  active: boolean
}) {
  const { repo, file } = selection
  const diff = useQuery({
    ...getChatSandboxDiffOptions({
      path: { public_id: conversationPublicId },
      query: {
        repo,
        path: file.path,
        old_path: file.old_path ?? undefined,
      },
    }),
    refetchInterval: active ? ACTIVE_REFRESH_MS : false,
    refetchOnWindowFocus: active,
  })
  useRefetchWhenShown(diff.refetch, active)
  if (diff.isPending) return <PanelSkeleton label="Loading diff" />
  if (diff.isError) {
    return (
      <p role="alert" className="p-4 text-sm text-muted-foreground">
        {getErrorMessage(diff.error)}
      </p>
    )
  }
  const { binary, original, modified, truncated } = diff.data
  if (binary) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {baseName(file.path)} is a binary file, so there is no line diff to
        show.
      </p>
    )
  }
  return (
    <div className="flex h-full min-h-0 flex-col">
      <p className="shrink-0 border-b px-3 py-1.5 font-mono text-xs text-muted-foreground">
        {file.old_path ? `${file.old_path} → ${file.path}` : file.path}
        <span className="ml-2 font-sans">
          · {changeBadge(file.change).label}
        </span>
      </p>
      {truncated && (
        <p className="shrink-0 border-b px-3 py-1.5 text-xs text-muted-foreground">
          This file is large; only its first part is compared.
        </p>
      )}
      <div className="min-h-0 flex-1">
        <Suspense fallback={<PanelSkeleton label="Loading editor" />}>
          <ChatDiffViewer
            path={file.path}
            original={original ?? ''}
            modified={modified ?? ''}
          />
        </Suspense>
      </div>
    </div>
  )
}

function ChangesBrowser({
  conversationPublicId,
  active,
}: {
  conversationPublicId: string
  active: boolean
}) {
  const queryClient = useQueryClient()
  const path = { public_id: conversationPublicId }
  const changes = useQuery({
    ...getChatSandboxChangesOptions({ path }),
    refetchInterval: active ? ACTIVE_REFRESH_MS : false,
    refetchOnWindowFocus: active,
  })
  useRefetchWhenShown(changes.refetch, active)
  const [selection, setSelection] = useState<Selection | null>(null)
  const track = useMutation({
    ...trackChatSandboxChangesMutation(),
    onSuccess: (data) => {
      queryClient.setQueryData(getChatSandboxChangesQueryKey({ path }), data)
      toast.success('Changes are now tracked', {
        description: 'Edits from here on show up in this tab.',
      })
    },
    onError: (error) => {
      toast.error('Could not start tracking changes', {
        description: getErrorMessage(error),
      })
    },
  })
  const refresh = () =>
    queryClient.invalidateQueries({
      predicate: (query) => {
        const id = (query.queryKey[0] as { _id?: string } | undefined)?._id
        return id === 'getChatSandboxChanges' || id === 'getChatSandboxDiff'
      },
    })

  if (changes.isPending) return <PanelSkeleton label="Loading changes" />
  if (changes.isError) {
    return (
      <div role="alert" className="space-y-2 p-4">
        <p className="text-sm text-muted-foreground">
          {getErrorMessage(changes.error)}
        </p>
        <Button type="button" size="sm" variant="outline" onClick={refresh}>
          Try again
        </Button>
      </div>
    )
  }
  const { git_available, repos, work_dir } = changes.data
  if (!git_available || repos.length === 0) {
    return (
      <TrackChangesPrompt
        gitAvailable={git_available}
        pending={track.isPending}
        onTrack={() => track.mutate({ path, body: {} })}
      />
    )
  }

  const total = countChanges(repos)
  // A selected file that is no longer changed (committed, reverted) closes.
  const current =
    selection === null
      ? null
      : (repos
          .filter((repo) => repo.root === selection.repo)
          .flatMap((repo) => repo.files)
          .find((file) => file.path === selection.file.path) ?? null)
  const selectedKey =
    selection && current ? `${selection.repo}\u0000${current.path}` : null

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b px-3 py-1.5">
        <p className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          {total === 0
            ? 'No changes since the last commit'
            : `${total} changed file${total === 1 ? '' : 's'}`}
        </p>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7 shrink-0"
          onClick={refresh}
          aria-label="Reload the changes"
        >
          <RefreshCw className="size-3.5" />
        </Button>
      </div>
      <div className="@container flex min-h-0 flex-1">
        <div className="flex min-h-0 w-full flex-col @lg:flex-row">
          <nav
            aria-label="Changed files"
            className="max-h-48 shrink-0 overflow-auto border-b pb-1 @lg:max-h-none @lg:w-64 @lg:border-r @lg:border-b-0"
          >
            {repos.map((repo) => (
              <RepoChanges
                key={repo.root}
                repo={repo}
                workDir={work_dir}
                selectedKey={selectedKey}
                onSelect={setSelection}
              />
            ))}
          </nav>
          <div className="min-h-0 min-w-0 flex-1">
            {selection === null || current === null ? (
              <p className="p-4 text-sm text-muted-foreground">
                {total === 0
                  ? 'When the assistant edits a file, it is listed here with what changed.'
                  : 'Pick a file to see what changed in it.'}
              </p>
            ) : (
              <FileDiff
                key={selectedKey}
                conversationPublicId={conversationPublicId}
                selection={{ repo: selection.repo, file: current }}
                active={active}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

/** What the assistant changed in the chat's sandbox, as Git diffs. */
export function ChatChangesTab({
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
      onboarding={(reason) => <ChangesOnboarding reason={reason} />}
    >
      {opened && (
        <ChangesBrowser
          conversationPublicId={conversationPublicId}
          active={active}
        />
      )}
    </ChatSandboxGate>
  )
}
