// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo } from 'react'
import { FileCode2, FilePlus2 } from 'lucide-react'
import { cn } from '@/lib/utils'
import { parseUnifiedDiff, type ParsedDiffLine } from './workspace-diff'

function DiffLine({ line }: { line: ParsedDiffLine }) {
  if (line.kind === 'hunk') {
    return (
      <div
        data-diff-line="true"
        className="border-y border-sky-400/15 bg-sky-400/[0.07] px-3 py-1.5 font-mono text-[9px] leading-4 text-sky-300"
      >
        <span className="mr-2 text-sky-400/60">•••</span>
        {line.content}
      </div>
    )
  }

  if (line.kind === 'metadata') {
    return (
      <div
        data-diff-line="true"
        className="border-y border-white/[0.06] bg-white/[0.025] px-3 py-1 font-mono text-[9px] italic leading-4 text-[#8b949e]"
      >
        {line.content}
      </div>
    )
  }

  const marker =
    line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ''

  return (
    <div
      data-diff-line="true"
      className={cn(
        'grid grid-cols-[2.5rem_2.5rem_1.15rem_minmax(0,1fr)] border-l-2 font-mono text-[10px] leading-5',
        line.kind === 'added' &&
          'border-l-emerald-400 bg-emerald-400/[0.09] text-emerald-50',
        line.kind === 'removed' &&
          'border-l-rose-400 bg-rose-400/[0.09] text-rose-50',
        line.kind === 'context' &&
          'border-l-transparent text-[#c9d1d9] hover:bg-white/[0.025]'
      )}
    >
      <span className="select-none border-r border-white/[0.06] px-1.5 text-right text-[#6e7681]">
        {line.oldLine ?? ''}
      </span>
      <span className="select-none border-r border-white/[0.06] px-1.5 text-right text-[#6e7681]">
        {line.newLine ?? ''}
      </span>
      <span
        className={cn(
          'select-none text-center font-semibold',
          line.kind === 'added' && 'text-emerald-400',
          line.kind === 'removed' && 'text-rose-400'
        )}
      >
        {marker}
      </span>
      <code className="min-w-0 whitespace-pre-wrap break-words py-px pr-3 [overflow-wrap:anywhere]">
        {line.content || ' '}
      </code>
    </div>
  )
}

export function WorkspaceDiffViewer({
  diff,
  path,
  truncated,
}: {
  diff: string
  path: string
  truncated: boolean
}) {
  const parsed = useMemo(() => parseUnifiedDiff(diff), [diff])

  return (
    <section className="overflow-hidden rounded-lg border border-border bg-[#0d1117] text-[#c9d1d9] shadow-inner">
      <div className="flex flex-wrap items-center gap-2 border-b border-white/10 bg-white/[0.025] px-3 py-2.5">
        {parsed.isNewFile ? (
          <FilePlus2 className="size-3.5 shrink-0 text-emerald-400" />
        ) : (
          <FileCode2 className="size-3.5 shrink-0 text-[#7ee787]" />
        )}
        <span
          className="min-w-0 flex-1 truncate font-mono text-[10px]"
          title={path}
        >
          {path}
        </span>
        {parsed.isNewFile && (
          <span className="rounded-full border border-emerald-400/20 bg-emerald-400/10 px-1.5 py-0.5 text-[8px] font-semibold uppercase tracking-wide text-emerald-300">
            New file
          </span>
        )}
        <span className="font-mono text-[9px] font-semibold text-emerald-400">
          +{parsed.additions}
        </span>
        <span className="font-mono text-[9px] font-semibold text-rose-400">
          −{parsed.deletions}
        </span>
      </div>

      <div className="max-h-[52vh] overflow-y-auto overscroll-contain py-1">
        {parsed.lines.map((line, index) => (
          <DiffLine key={`${index}-${line.kind}`} line={line} />
        ))}
        {parsed.omittedLines > 0 && (
          <div
            role="note"
            className="border-t border-amber-300/20 bg-amber-300/[0.08] px-3 py-2 text-[9px] text-amber-200"
          >
            Showing the first {parsed.lines.length.toLocaleString()} of{' '}
            {parsed.totalLines.toLocaleString()} diff lines.{' '}
            {parsed.omittedLines.toLocaleString()} lines were not rendered to
            keep this preview responsive. Open the file in the sandbox to
            inspect the remaining changes.
          </div>
        )}
        {truncated && (
          <div className="border-t border-amber-300/20 bg-amber-300/[0.08] px-3 py-2 text-[9px] text-amber-200">
            The server response was truncated at its byte limit, so additional
            lines may not be included. Open the file in the sandbox to inspect
            all changes.
          </div>
        )}
      </div>
    </section>
  )
}
