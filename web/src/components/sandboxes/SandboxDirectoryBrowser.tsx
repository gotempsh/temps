// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowUp,
  File as FileIcon,
  Folder,
  Link2,
  Loader2,
  RefreshCw,
} from 'lucide-react'
import { listDirOptions } from '@/api/client/@tanstack/react-query.gen'
import type { DirectoryEntry } from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  initialDirectory,
  joinSandboxPath,
  parentDirectory,
} from './sandbox-files'

/** Entries requested per page; the server's largest page. */
const LIST_PAGE_SIZE = 100

/** Pages needed for `total` entries; at least one, even for an empty directory. */
function pageCount(total: number, pageSize: number): number {
  return Math.max(1, Math.ceil(total / Math.max(1, pageSize)))
}

interface SandboxDirectoryBrowserProps {
  sandboxId: string
  /** Where the browser starts; normally the sandbox's working directory. */
  workDir: string
  /** The sandbox cannot serve files right now; the card explains why. */
  blocked: boolean
  /** A file (or a link that may be one) was chosen: open it in the editor. */
  onOpenFile: (path: string) => void
}

/**
 * Browse the directories of a sandbox (`fs/list`) and pick a file to open.
 *
 * Symbolic links are listed but never followed by the server, so a link is
 * entered like a directory; when it turns out to be a file the browser says so
 * and offers to open it instead of guessing.
 */
export function SandboxDirectoryBrowser({
  sandboxId,
  workDir,
  blocked,
  onOpenFile,
}: SandboxDirectoryBrowserProps) {
  const [directory, setCurrentDirectory] = useState(() =>
    initialDirectory(workDir)
  )
  const [page, setPage] = useState(1)
  // A page number means nothing in another directory, so every move starts
  // again from the first page.
  const setDirectory = (path: string) => {
    setCurrentDirectory(path)
    setPage(1)
  }

  const listing = useQuery({
    ...listDirOptions({
      path: { id: sandboxId },
      query: { path: directory, page, page_size: LIST_PAGE_SIZE },
    }),
    enabled: !blocked,
    retry: false,
    // Another tool (the terminal beside this card) changes the tree, so a
    // directory is read again whenever it is shown, not served from cache.
    staleTime: 0,
  })

  const data = listing.data
  const atRoot = directory === '/'

  return (
    <div className="space-y-2 rounded-md border p-3">
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="icon"
          aria-label="Up one directory"
          disabled={blocked || atRoot}
          onClick={() => setDirectory(parentDirectory(directory))}
        >
          <ArrowUp className="h-4 w-4" aria-hidden="true" />
        </Button>
        <code
          className="min-w-0 flex-1 truncate font-mono text-sm"
          title={directory}
        >
          {directory}
        </code>
        <Button
          type="button"
          variant="outline"
          size="icon"
          aria-label="Refresh directory"
          disabled={blocked || listing.isFetching}
          onClick={() => void listing.refetch()}
        >
          {listing.isFetching ? (
            <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
          ) : (
            <RefreshCw className="h-4 w-4" aria-hidden="true" />
          )}
        </Button>
      </div>

      <DirectoryBody
        blocked={blocked}
        loading={listing.isPending && !blocked}
        error={listing.isError ? listing.error : null}
        directory={directory}
        listing={data}
        page={page}
        onPage={setPage}
        onEnter={setDirectory}
        onOpenFile={onOpenFile}
        onUp={() => setDirectory(parentDirectory(directory))}
      />
    </div>
  )
}

interface DirectoryBodyProps {
  blocked: boolean
  loading: boolean
  error: unknown
  directory: string
  listing:
    | {
        exists: boolean
        is_dir: boolean
        entries: DirectoryEntry[]
        page_size: number
        total: number
      }
    | undefined
  page: number
  onPage: (page: number) => void
  onEnter: (path: string) => void
  onOpenFile: (path: string) => void
  onUp: () => void
}

function DirectoryBody({
  blocked,
  loading,
  error,
  directory,
  listing,
  page,
  onPage,
  onEnter,
  onOpenFile,
  onUp,
}: DirectoryBodyProps) {
  if (blocked) {
    return (
      <p className="text-xs text-muted-foreground">
        Directories can be browsed once the sandbox is running.
      </p>
    )
  }
  if (loading) {
    return (
      <div className="space-y-1">
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
      </div>
    )
  }
  if (error) {
    return (
      <Alert variant="destructive">
        <AlertDescription>
          Could not list {directory}: {problemDetail(error, 'request failed.')}
        </AlertDescription>
      </Alert>
    )
  }
  if (!listing) return null
  if (!listing.exists) {
    return (
      <p className="text-xs text-muted-foreground">
        {directory} does not exist in this sandbox.
      </p>
    )
  }
  if (!listing.is_dir) {
    return (
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="min-w-0 truncate font-mono">{directory}</span>
        <span className="text-muted-foreground">is a file.</span>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => onOpenFile(directory)}
        >
          Open file
        </Button>
        <Button type="button" size="sm" variant="ghost" onClick={onUp}>
          Back to folder
        </Button>
      </div>
    )
  }
  if (listing.entries.length === 0 && page === 1) {
    return (
      <p className="text-xs text-muted-foreground">This directory is empty.</p>
    )
  }
  if (listing.entries.length === 0) {
    // Entries were removed after this page was chosen.
    return (
      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <span>This page is past the end of the directory.</span>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => onPage(1)}
        >
          Go to first page
        </Button>
      </div>
    )
  }
  return (
    <div className="space-y-2">
      <ul className="max-h-64 divide-y overflow-y-auto rounded-md border">
        {listing.entries.map((entry) => (
          <li key={entry.name}>
            <EntryRow
              entry={entry}
              onActivate={() => {
                const path = joinSandboxPath(directory, entry.name)
                if (entry.kind === 'file') onOpenFile(path)
                else onEnter(path)
              }}
            />
          </li>
        ))}
      </ul>
      {pageCount(listing.total, listing.page_size) > 1 && (
        <ResponsivePagination
          page={page}
          pageSize={listing.page_size}
          total={listing.total}
          totalPages={pageCount(listing.total, listing.page_size)}
          onPageChange={onPage}
          ariaLabel="Directory pages"
        />
      )}
    </div>
  )
}

function EntryRow({
  entry,
  onActivate,
}: {
  entry: DirectoryEntry
  onActivate: () => void
}) {
  // `other` (sockets, devices, pipes) cannot be opened or entered.
  const disabled = entry.kind === 'other'
  return (
    <button
      type="button"
      onClick={onActivate}
      disabled={disabled}
      className="flex min-h-9 w-full items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-60"
    >
      <EntryIcon kind={entry.kind} />
      <span className="min-w-0 flex-1 truncate font-mono">{entry.name}</span>
      <span className="sr-only">{entryKindLabel(entry.kind)}</span>
    </button>
  )
}

function EntryIcon({ kind }: { kind: DirectoryEntry['kind'] }) {
  const className = 'h-4 w-4 shrink-0 text-muted-foreground'
  if (kind === 'directory') {
    return <Folder className={className} aria-hidden="true" />
  }
  if (kind === 'symlink') {
    return <Link2 className={className} aria-hidden="true" />
  }
  return <FileIcon className={className} aria-hidden="true" />
}

function entryKindLabel(kind: DirectoryEntry['kind']): string {
  switch (kind) {
    case 'directory':
      return 'directory'
    case 'symlink':
      return 'symbolic link'
    case 'file':
      return 'file'
    case 'other':
      return 'special file, cannot be opened'
  }
}
