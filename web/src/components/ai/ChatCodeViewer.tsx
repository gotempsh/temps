// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Skeleton } from '@/components/ui/skeleton'
import { DiffEditor, Editor } from '@monaco-editor/react'
import { useTheme } from 'next-themes'
import { languageForPath } from './chat-files'

function EditorSkeleton() {
  return (
    <div className="space-y-2 p-4" role="status" aria-label="Loading editor">
      <Skeleton className="h-4 w-3/4" />
      <Skeleton className="h-4 w-1/2" />
      <Skeleton className="h-4 w-2/3" />
    </div>
  )
}

const SHARED_OPTIONS = {
  readOnly: true,
  domReadOnly: true,
  minimap: { enabled: false },
  automaticLayout: true,
  fontSize: 12,
  scrollBeyondLastLine: false,
  renderLineHighlight: 'none',
  wordWrap: 'off',
} as const

/** A read-only, highlighted view of one sandbox file. */
export function ChatCodeViewer({
  path,
  content,
}: {
  path: string
  content: string
}) {
  const { resolvedTheme } = useTheme()
  return (
    <Editor
      height="100%"
      path={path}
      language={languageForPath(path)}
      value={content}
      theme={resolvedTheme === 'dark' ? 'vs-dark' : 'light'}
      loading={<EditorSkeleton />}
      options={SHARED_OPTIONS}
    />
  )
}

/**
 * The last commit's version of a file against the working copy. Side by side
 * when there is room, inline in a narrow panel.
 */
export function ChatDiffViewer({
  path,
  original,
  modified,
}: {
  path: string
  original: string
  modified: string
}) {
  const { resolvedTheme } = useTheme()
  return (
    <DiffEditor
      height="100%"
      language={languageForPath(path)}
      original={original}
      modified={modified}
      theme={resolvedTheme === 'dark' ? 'vs-dark' : 'light'}
      loading={<EditorSkeleton />}
      options={{
        ...SHARED_OPTIONS,
        originalEditable: false,
        renderSideBySide: true,
        useInlineViewWhenSpaceIsLimited: true,
        renderSideBySideInlineBreakpoint: 700,
        hideUnchangedRegions: { enabled: true },
      }}
    />
  )
}
