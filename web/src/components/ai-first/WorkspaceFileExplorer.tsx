// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AlertCircle,
  ChevronDown,
  ChevronRight,
  File,
  FileQuestion,
  Folder,
  FolderOpen,
  Link2,
  Loader2,
  RefreshCw,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  getApplicationWorkspaceDirectory,
  getApplicationWorkspaceFile,
  getGlobalWorkspaceDirectory,
  getGlobalWorkspaceFile,
  type ApplicationWorkspaceDirectoryEntryResponse,
  type ApplicationWorkspaceFileContentResponse,
  type ApplicationWorkspaceFileResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { problemDetail } from './problem-detail'

const DIRECTORY_PAGE_SIZE = 100

type DirectoryState = {
  entries: ApplicationWorkspaceDirectoryEntryResponse[]
  nextCursor: number | null
  loading: boolean
  loaded: boolean
  truncated: boolean
  error: string | null
}

const emptyDirectory = (): DirectoryState => ({
  entries: [],
  nextCursor: null,
  loading: false,
  loaded: false,
  truncated: false,
  error: null,
})

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`
  if (bytes < 1_024 * 1_024) return `${(bytes / 1_024).toFixed(1)} KB`
  return `${(bytes / (1_024 * 1_024)).toFixed(1)} MB`
}

function entryIcon(
  entry: ApplicationWorkspaceDirectoryEntryResponse,
  expanded: boolean
) {
  if (entry.kind === 'directory') {
    return expanded ? FolderOpen : Folder
  }
  if (entry.kind === 'symlink') return Link2
  if (entry.kind === 'file') return File
  return FileQuestion
}

export function WorkspaceFileExplorer({
  applicationPublicId,
  changes,
}: {
  applicationPublicId?: string
  changes: ApplicationWorkspaceFileResponse[]
}) {
  const [directories, setDirectories] = useState<
    Record<string, DirectoryState>
  >({})
  const [expanded, setExpanded] = useState<Set<string>>(new Set(['']))
  const [selectedPath, setSelectedPath] = useState<string | null>(null)
  const [preview, setPreview] =
    useState<ApplicationWorkspaceFileContentResponse | null>(null)
  const [previewLoading, setPreviewLoading] = useState(false)
  const [previewError, setPreviewError] = useState<string | null>(null)
  const generation = useRef(0)
  const changeByPath = useMemo(
    () => new Map(changes.map((change) => [change.path, change.status])),
    [changes]
  )

  const loadDirectory = useCallback(
    async (path: string, cursor = 0, append = false) => {
      const requestGeneration = generation.current
      setDirectories((current) => ({
        ...current,
        [path]: {
          ...(current[path] ?? emptyDirectory()),
          loading: true,
          error: null,
        },
      }))
      try {
        const { data } = applicationPublicId
          ? await getApplicationWorkspaceDirectory({
              path: { application_public_id: applicationPublicId },
              query: {
                path: path || undefined,
                cursor,
                limit: DIRECTORY_PAGE_SIZE,
              },
              throwOnError: true,
            })
          : await getGlobalWorkspaceDirectory({
              query: {
                path: path || undefined,
                cursor,
                limit: DIRECTORY_PAGE_SIZE,
              },
              throwOnError: true,
            })
        if (requestGeneration !== generation.current) return
        setDirectories((current) => {
          const previous = current[path] ?? emptyDirectory()
          const entries = append
            ? [...previous.entries, ...data.entries]
            : data.entries
          return {
            ...current,
            [path]: {
              entries,
              nextCursor: data.next_cursor ?? null,
              loading: false,
              loaded: true,
              truncated: data.truncated,
              error: null,
            },
          }
        })
      } catch (cause) {
        if (requestGeneration !== generation.current) return
        setDirectories((current) => ({
          ...current,
          [path]: {
            ...(current[path] ?? emptyDirectory()),
            loading: false,
            loaded: true,
            error: problemDetail(cause, 'Could not load this directory.'),
          },
        }))
      }
    },
    [applicationPublicId]
  )

  const loadPreview = useCallback(
    async (path: string) => {
      const requestGeneration = generation.current
      setSelectedPath(path)
      setPreview(null)
      setPreviewError(null)
      setPreviewLoading(true)
      try {
        const { data } = applicationPublicId
          ? await getApplicationWorkspaceFile({
              path: { application_public_id: applicationPublicId },
              query: { path },
              throwOnError: true,
            })
          : await getGlobalWorkspaceFile({
              query: { path },
              throwOnError: true,
            })
        if (requestGeneration === generation.current) setPreview(data)
      } catch (cause) {
        if (requestGeneration === generation.current) {
          setPreviewError(problemDetail(cause, 'Could not preview this file.'))
        }
      } finally {
        if (requestGeneration === generation.current) setPreviewLoading(false)
      }
    },
    [applicationPublicId]
  )

  const reset = useCallback(() => {
    generation.current += 1
    setDirectories({})
    setExpanded(new Set(['']))
    setSelectedPath(null)
    setPreview(null)
    setPreviewError(null)
    setPreviewLoading(false)
    void loadDirectory('')
  }, [loadDirectory])

  useEffect(() => {
    reset()
    return () => {
      generation.current += 1
    }
  }, [reset])

  const toggleDirectory = (path: string) => {
    const willExpand = !expanded.has(path)
    setExpanded((current) => {
      const next = new Set(current)
      if (willExpand) next.add(path)
      else next.delete(path)
      return next
    })
    if (willExpand && !directories[path]?.loaded) void loadDirectory(path)
  }

  const renderDirectory = (path: string, depth: number) => {
    const directory = directories[path] ?? emptyDirectory()
    return (
      <div key={path || 'workspace-root'}>
        {directory.entries.map((entry) => {
          const isDirectory = entry.kind === 'directory'
          const isExpanded = isDirectory && expanded.has(entry.path)
          const Icon = entryIcon(entry, isExpanded)
          const status = changeByPath.get(entry.path)
          return (
            <div key={entry.path}>
              <button
                aria-expanded={isDirectory ? isExpanded : undefined}
                className={cn(
                  'flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left font-mono text-[10px] hover:bg-accent',
                  selectedPath === entry.path && 'bg-accent text-foreground',
                  !isDirectory &&
                    entry.kind !== 'file' &&
                    'cursor-not-allowed text-muted-foreground'
                )}
                disabled={!isDirectory && entry.kind !== 'file'}
                onClick={() =>
                  isDirectory
                    ? toggleDirectory(entry.path)
                    : void loadPreview(entry.path)
                }
                style={{ paddingLeft: `${depth * 14 + 6}px` }}
                title={entry.path}
                type="button"
              >
                {isDirectory ? (
                  isExpanded ? (
                    <ChevronDown className="size-3 shrink-0" />
                  ) : (
                    <ChevronRight className="size-3 shrink-0" />
                  )
                ) : (
                  <span className="size-3 shrink-0" />
                )}
                <Icon
                  className={cn(
                    'size-3.5 shrink-0',
                    isDirectory && 'text-amber-600 dark:text-amber-300'
                  )}
                />
                <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                {status && (
                  <span
                    aria-label={`${status} file`}
                    className="shrink-0 font-sans text-[8px] font-semibold uppercase text-amber-600 dark:text-amber-300"
                  >
                    {status.slice(0, 1)}
                  </span>
                )}
                {!isDirectory && entry.kind === 'file' && (
                  <span className="shrink-0 font-sans text-[8px] text-muted-foreground">
                    {formatBytes(entry.size_bytes)}
                  </span>
                )}
              </button>
              {isExpanded && renderDirectory(entry.path, depth + 1)}
            </div>
          )
        })}
        {directory.loading && (
          <div
            className="flex items-center gap-2 px-2 py-2 text-[10px] text-muted-foreground"
            style={{ paddingLeft: `${depth * 14 + 22}px` }}
          >
            <Loader2 className="size-3 animate-spin" /> Loading…
          </div>
        )}
        {directory.error && (
          <div
            className="flex items-start gap-2 px-2 py-2 text-[10px] text-destructive"
            role="alert"
            style={{ paddingLeft: `${depth * 14 + 22}px` }}
          >
            <AlertCircle className="mt-0.5 size-3 shrink-0" />
            <span>{directory.error}</span>
          </div>
        )}
        {directory.loaded &&
          !directory.loading &&
          !directory.error &&
          directory.entries.length === 0 && (
            <p
              className="px-2 py-2 text-[10px] text-muted-foreground"
              style={{ paddingLeft: `${depth * 14 + 22}px` }}
            >
              Empty directory
            </p>
          )}
        {directory.nextCursor != null && !directory.loading && (
          <button
            className="px-2 py-1.5 text-[10px] font-medium text-primary hover:underline"
            onClick={() =>
              void loadDirectory(path, directory.nextCursor ?? 0, true)
            }
            style={{ marginLeft: `${depth * 14 + 14}px` }}
            type="button"
          >
            Load more
          </button>
        )}
        {directory.truncated && !directory.nextCursor && (
          <p className="px-2 py-2 text-[9px] text-amber-600 dark:text-amber-300">
            Some entries are hidden by the workspace safety limit.
          </p>
        )}
      </div>
    )
  }

  return (
    <section className="overflow-hidden rounded-lg border border-border bg-background">
      <div className="flex items-center justify-between border-b border-border px-3 py-2">
        <div>
          <p className="text-xs font-medium">Workspace files</p>
          <p className="mt-0.5 text-[10px] text-muted-foreground">
            Persistent files, available while compute sleeps
          </p>
        </div>
        <Button
          aria-label="Refresh file explorer"
          className="size-7"
          onClick={reset}
          size="icon"
          type="button"
          variant="ghost"
        >
          <RefreshCw className="size-3.5" />
        </Button>
      </div>
      <div className="max-h-72 overflow-auto p-1.5">
        {renderDirectory('', 0)}
      </div>
      {(selectedPath || previewLoading || previewError) && (
        <div className="border-t border-border bg-muted/20">
          <div className="flex items-center gap-2 border-b border-border px-3 py-2 font-mono text-[10px]">
            <File className="size-3.5 shrink-0" />
            <span className="min-w-0 flex-1 truncate">{selectedPath}</span>
            {preview && (
              <span className="font-sans text-[9px] text-muted-foreground">
                {formatBytes(preview.size_bytes)}
              </span>
            )}
          </div>
          {previewLoading ? (
            <div className="flex items-center gap-2 px-3 py-5 text-[10px] text-muted-foreground">
              <Loader2 className="size-3 animate-spin" /> Loading preview…
            </div>
          ) : previewError ? (
            <p className="px-3 py-4 text-[10px] text-destructive" role="alert">
              {previewError}
            </p>
          ) : preview?.binary ? (
            <p className="px-3 py-4 text-[10px] text-muted-foreground">
              Binary files cannot be previewed here.
            </p>
          ) : (
            <>
              <pre className="max-h-80 overflow-auto whitespace-pre p-3 font-mono text-[10px] leading-4 text-foreground">
                {preview?.content}
              </pre>
              {preview?.truncated && (
                <p className="border-t border-border px-3 py-2 text-[9px] text-amber-600 dark:text-amber-300">
                  Preview limited to the first 256 KB.
                </p>
              )}
            </>
          )}
        </div>
      )}
    </section>
  )
}
