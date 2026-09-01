// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * StackTrace Component
 *
 * Renders a stack trace for error events with expandable source context.
 * Designed to display stack frames as returned by Sentry or Node.js error events.
 *
 * Frame properties:
 *   - filename: string (full path or module)
 *   - function: string (function name)
 *   - lineno: number (line number)
 *   - colno: number (column number)
 *   - context_line: string (source code line, optional)
 *   - pre_context: string[] (lines before context_line, optional)
 *   - post_context: string[] (lines after context_line, optional)
 *   - in_app: boolean (whether this is application code)
 *   - symbolicated: boolean (whether this frame was resolved via source maps)
 *   - original_filename: string (minified filename before symbolication)
 */

import { ChevronRight, Code2 } from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'

interface StackFrame {
  filename?: string
  function?: string
  lineno?: number
  colno?: number
  context_line?: string
  pre_context?: string[]
  post_context?: string[]
  in_app?: boolean
  symbolicated?: boolean
  original_filename?: string
  original_lineno?: number
  original_colno?: number
}

interface StackTraceProps {
  frames?: StackFrame[]
  detailed?: boolean
  className?: string
}

function hasSourceContext(frame: StackFrame): boolean {
  return !!(frame.context_line || (frame.pre_context && frame.pre_context.length > 0) || (frame.post_context && frame.post_context.length > 0))
}

interface SourceContextProps {
  frame: StackFrame
}

function SourceContext({ frame }: SourceContextProps) {
  const { pre_context, context_line, post_context, lineno } = frame
  if (!context_line && (!pre_context || pre_context.length === 0)) return null

  // Calculate starting line number for pre_context
  const contextLineNo = lineno || 1
  const preStartLine = contextLineNo - (pre_context?.length || 0)

  // Find the widest line number for padding
  const lastLine = contextLineNo + (post_context?.length || 0)
  const gutterWidth = String(lastLine).length

  return (
    <div className="mt-1 rounded-md overflow-hidden border border-border/50 bg-muted text-xs font-mono">
      {/* Pre-context lines */}
      {pre_context?.map((line, i) => {
        const lineNum = preStartLine + i
        return (
          <div key={`pre-${lineNum}`} className="flex hover:bg-muted-foreground/5">
            <span className="flex-shrink-0 w-[var(--gutter)] text-right pr-3 pl-2 py-px text-muted-foreground/40 select-none border-r border-border/30 bg-background/50" style={{ '--gutter': `${gutterWidth + 2}ch` } as React.CSSProperties}>
              {lineNum}
            </span>
            <pre className="flex-1 pl-3 py-px text-muted-foreground/60 overflow-x-auto"><code>{line || ' '}</code></pre>
          </div>
        )
      })}

      {/* Context line (the error line) */}
      {context_line != null && (
        <div className="flex bg-red-500/10 border-l-2 border-red-500">
          <span className="flex-shrink-0 w-[var(--gutter)] text-right pr-3 pl-2 py-px text-red-400/70 select-none border-r border-red-500/30 bg-red-500/5" style={{ '--gutter': `${gutterWidth + 2}ch` } as React.CSSProperties}>
            {contextLineNo}
          </span>
          <pre className="flex-1 pl-3 py-px text-foreground overflow-x-auto"><code>{context_line || ' '}</code></pre>
        </div>
      )}

      {/* Post-context lines */}
      {post_context?.map((line, i) => {
        const lineNum = contextLineNo + 1 + i
        return (
          <div key={`post-${lineNum}`} className="flex hover:bg-muted-foreground/5">
            <span className="flex-shrink-0 w-[var(--gutter)] text-right pr-3 pl-2 py-px text-muted-foreground/40 select-none border-r border-border/30 bg-background/50" style={{ '--gutter': `${gutterWidth + 2}ch` } as React.CSSProperties}>
              {lineNum}
            </span>
            <pre className="flex-1 pl-3 py-px text-muted-foreground/60 overflow-x-auto"><code>{line || ' '}</code></pre>
          </div>
        )
      })}
    </div>
  )
}

export function StackTrace({
  frames,
  detailed = false,
  className = '',
}: StackTraceProps) {
  // Show frames in reverse order (most recent first)
  const reversedFrames = frames && frames.length > 0 ? [...frames].reverse() : []

  // Track which frames are expanded (auto-expand first in-app frame with context)
  const firstInAppWithContext = reversedFrames.findIndex(
    (f) => f.in_app !== false && hasSourceContext(f),
  )

  const [expandedFrames, setExpandedFrames] = useState<Set<number>>(() => {
    const initial = new Set<number>()
    if (firstInAppWithContext >= 0) {
      initial.add(firstInAppWithContext)
    }
    return initial
  })

  function toggleFrame(index: number) {
    setExpandedFrames((prev) => {
      const next = new Set(prev)
      if (next.has(index)) {
        next.delete(index)
      } else {
        next.add(index)
      }
      return next
    })
  }

  if (reversedFrames.length === 0) return null

  return (
    <div
      className={`font-mono text-sm space-y-0 rounded-lg overflow-hidden border border-border/50 ${className}`}
    >
      {reversedFrames.map((frame, index) => {
        const functionName = frame.function || '<anonymous>'
        const filename = frame.filename || ''
        const lineNo = frame.lineno
        const colNo = frame.colno
        const hasContext = hasSourceContext(frame)
        const isExpanded = expandedFrames.has(index)
        const isInApp = frame.in_app !== false

        // Extract just the filename from the full path
        const shortFilename = filename ? filename.split('/').pop() : 'unknown'

        const frameKey = `${filename || 'unknown'}-${lineNo ?? 'x'}-${colNo ?? 'x'}-${index}`

        return (
          <div
            key={frameKey}
            className={cn(
              'border-b border-border/30 last:border-b-0',
              !isInApp && 'opacity-60',
            )}
          >
            {/* Frame header */}
            <button
              type="button"
              className={cn(
                'flex items-center gap-2 px-3 py-2 transition-colors w-full text-left',
                hasContext
                  ? 'cursor-pointer hover:bg-muted/50'
                  : 'cursor-default',
                isExpanded && hasContext && 'bg-muted/30',
              )}
              onClick={hasContext ? () => toggleFrame(index) : undefined}
              tabIndex={hasContext ? 0 : -1}
            >
              {/* Expand/collapse indicator */}
              <span className="flex-shrink-0 w-4 text-muted-foreground/50">
                {hasContext ? (
                  <ChevronRight
                    className={cn(
                      'h-3.5 w-3.5 transition-transform',
                      isExpanded && 'rotate-90',
                    )}
                  />
                ) : (
                  <span className="inline-block w-3.5" />
                )}
              </span>

              {/* Frame info */}
              <div className="flex-1 min-w-0 truncate">
                <span className="text-primary font-medium">
                  {functionName}
                </span>
                <span className="text-muted-foreground ml-2">
                  {detailed && filename ? filename : shortFilename}
                  {lineNo !== undefined && (
                    <span className="text-blue-500 dark:text-blue-400">
                      :{lineNo}
                    </span>
                  )}
                  {colNo !== undefined && (
                    <span className="text-blue-500/70 dark:text-blue-400/70">
                      :{colNo}
                    </span>
                  )}
                </span>
              </div>

              {/* Badges */}
              <div className="flex items-center gap-1.5 flex-shrink-0">
                {frame.symbolicated && (
                  <Code2 className="h-3 w-3 text-green-500" aria-label="Symbolicated via source map" />
                )}
                {isInApp && (
                  <span className="text-[10px] font-sans text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                    app
                  </span>
                )}
              </div>
            </button>

            {/* Source context (expandable) */}
            {hasContext && isExpanded && (
              <div className="px-3 pb-3 pt-0">
                <SourceContext frame={frame} />
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}
