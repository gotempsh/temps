// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { cn } from '@/lib/utils'

type LogLevel = 'debug' | 'info' | 'warn' | 'error'

interface LogLine {
  ts: string
  level: LogLevel
  msg: string
  /** Structured JSONL fields, rendered key=value after the message. */
  fields?: Record<string, string | number>
}

interface LogViewerProps {
  lines: LogLine[]
  /** Keep the newest line in view as lines are appended. */
  follow?: boolean
  onFollowChange?: (follow: boolean) => void
  /** Visible height; the pane scrolls inside it. */
  className?: string
  title?: string
}

const LEVEL_CLASS: Record<LogLevel, string> = {
  debug: 'text-muted-foreground',
  info: 'text-foreground',
  warn: 'text-warning',
  error: 'text-destructive',
}

/**
 * Operator-console log pane. One component covers deploy build logs,
 * error-tracking stack traces and session-replay console output.
 *
 * - Gutter: tabular line numbers, fixed-width level column. Colour lives on
 *   the level token only, never on the whole line, so a screen of warnings
 *   is still readable.
 * - Search: `/` focuses the query box, `n` / `N` step through matches, Esc
 *   clears. Match count is always visible next to the box.
 * - Follow: on by default; scrolling up pauses it (like `tail -f` in a
 *   terminal multiplexer). A visible toggle re-arms it — no hidden state.
 */
export function LogViewer({
  lines,
  follow: followProp,
  onFollowChange,
  className,
  title,
}: LogViewerProps) {
  const [followState, setFollowState] = useState(true)
  const follow = followProp ?? followState
  const setFollow = (v: boolean) => {
    setFollowState(v)
    onFollowChange?.(v)
  }
  const [query, setQuery] = useState('')
  const [matchIdx, setMatchIdx] = useState(0)
  const paneRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  const matches = useMemo(() => {
    if (!query) return [] as number[]
    const q = query.toLowerCase()
    return lines
      .map((l, i) => ({ l, i }))
      .filter(
        ({ l }) =>
          l.msg.toLowerCase().includes(q) ||
          Object.entries(l.fields ?? {}).some(([k, v]) =>
            `${k}=${v}`.toLowerCase().includes(q)
          )
      )
      .map(({ i }) => i)
  }, [lines, query])

  useEffect(() => {
    setMatchIdx(0)
  }, [query])

  useEffect(() => {
    if (!follow || !paneRef.current) return
    paneRef.current.scrollTop = paneRef.current.scrollHeight
  }, [lines.length, follow])

  useEffect(() => {
    const target = matches[matchIdx]
    if (target === undefined || !paneRef.current) return
    const el = paneRef.current.querySelector<HTMLElement>(
      `[data-line="${target}"]`
    )
    el?.scrollIntoView({ block: 'center' })
  }, [matchIdx, matches])

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.target === inputRef.current) {
      if (e.key === 'Escape') {
        setQuery('')
        inputRef.current?.blur()
      }
      if (e.key === 'Enter' && matches.length) {
        setMatchIdx((i) => (e.shiftKey ? i - 1 + matches.length : i + 1) % matches.length)
      }
      return
    }
    if (e.key === '/') {
      e.preventDefault()
      inputRef.current?.focus()
    } else if (e.key === 'n' && matches.length) {
      setMatchIdx((i) => (i + 1) % matches.length)
    } else if (e.key === 'N' && matches.length) {
      setMatchIdx((i) => (i - 1 + matches.length) % matches.length)
    }
  }

  const onScroll = () => {
    const el = paneRef.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 4
    if (!atBottom && follow) setFollow(false)
  }

  return (
    <div
      className={cn('flex flex-col border', className)}
      onKeyDown={onKey}
      tabIndex={0}
    >
      <div className="flex h-8 items-center gap-2 border-b px-2 text-xs">
        {title && <span className="op-label min-w-0 truncate whitespace-nowrap">{title}</span>}
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <label className="flex items-center gap-1">
            <span className="select-none text-muted-foreground">/</span>
            <input
              ref={inputRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="search"
              aria-label="Search log lines"
              className="h-6 w-24 border bg-background sm:w-40 px-1.5 text-xs focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-ring"
            />
          </label>
          <span className="w-14 text-right tabular-nums text-muted-foreground">
            {query ? `${matches.length ? matchIdx + 1 : 0}/${matches.length}` : `${lines.length} ln`}
          </span>
          <button
            type="button"
            onClick={() => setFollow(!follow)}
            aria-pressed={follow}
            className={cn(
              'border px-1.5 py-0.5 text-[10px] uppercase tracking-[0.12em] focus-visible:outline-2 focus-visible:outline-ring',
              follow
                ? 'bg-foreground text-background'
                : 'text-muted-foreground hover:text-foreground'
            )}
          >
            follow
          </button>
        </div>
      </div>
      <div
        ref={paneRef}
        onScroll={onScroll}
        className="op-inset scrollbar-thin max-h-[inherit] flex-1 overflow-auto font-mono text-xs leading-5"
      >
        {lines.map((l, i) => {
          const hit = query && matches.includes(i)
          const current = matches[matchIdx] === i
          return (
            <div
              key={i}
              data-line={i}
              className={cn(
                'grid w-max min-w-full grid-cols-[3.5rem_4.5rem_3.5rem_max-content] gap-x-2 whitespace-pre px-2',
                hit && 'bg-muted',
                current && 'outline outline-1 -outline-offset-1 outline-ring'
              )}
            >
              <span className="select-none text-right tabular-nums text-muted-foreground/60">
                {i + 1}
              </span>
              <span className="tabular-nums text-muted-foreground">{l.ts}</span>
              <span className={cn('uppercase', LEVEL_CLASS[l.level])}>
                {l.level.padEnd(5)}
              </span>
              <span>
                {l.msg}
                {l.fields &&
                  Object.entries(l.fields).map(([k, v]) => (
                    <span key={k}>
                      {' '}
                      <span className="text-muted-foreground">{k}=</span>
                      {String(v)}
                    </span>
                  ))}
              </span>
            </div>
          )
        })}
      </div>
    </div>
  )
}
export type { LogLine, LogLevel }
