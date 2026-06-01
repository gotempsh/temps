'use client'

import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import AnsiToHtml from 'ansi-to-html'
import {
  AlertCircle,
  ArrowDown,
  Columns3,
  Pause,
  Play,
  Search,
  X,
} from 'lucide-react'
import { memo, useEffect, useMemo, useState } from 'react'
import {
  LiveLogLevel,
  LiveLogLine,
  useLogStream,
} from '@/hooks/useLogStream'

const ansiConverter = new AnsiToHtml({
  fg: 'var(--foreground)',
  bg: 'var(--background)',
  newline: false,
  escapeXML: true,
})

const LEVEL_OPTIONS: LiveLogLevel[] = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE']

const LEVEL_COLORS: Record<LiveLogLevel, string> = {
  ERROR: 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/20',
  WARN: 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 border-yellow-500/20',
  INFO: 'bg-blue-500/15 text-blue-700 dark:text-blue-400 border-blue-500/20',
  DEBUG: 'bg-zinc-500/15 text-zinc-700 dark:text-zinc-400 border-zinc-500/20',
  TRACE: 'bg-zinc-400/15 text-zinc-500 dark:text-zinc-500 border-zinc-400/20',
}

interface ColumnVisibility {
  timestamp: boolean
  level: boolean
  service: boolean
}

const COLUMNS_STORAGE_KEY = 'temps.live-log.columns'

// Defaults match the history viewer's terminal look: timestamp + level +
// service ALL on, so live and history feel like the same surface. Users who
// want a denser tail can still hide columns via the Columns dropdown — the
// choice persists per-browser.
const DEFAULT_COLUMNS: ColumnVisibility = {
  timestamp: true,
  level: true,
  service: true,
}

function loadColumns(): ColumnVisibility {
  if (typeof window === 'undefined') {
    return DEFAULT_COLUMNS
  }
  try {
    const raw = window.localStorage.getItem(COLUMNS_STORAGE_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<ColumnVisibility>
      return {
        timestamp: parsed.timestamp ?? DEFAULT_COLUMNS.timestamp,
        level: parsed.level ?? DEFAULT_COLUMNS.level,
        service: parsed.service ?? DEFAULT_COLUMNS.service,
      }
    }
  } catch {
    // Ignore corrupted storage and fall through to defaults.
  }
  return DEFAULT_COLUMNS
}

function formatTimestamp(iso?: string): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const base = d.toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
  return `${base}.${String(d.getMilliseconds()).padStart(3, '0')}`
}

interface LiveLogRowProps {
  line: LiveLogLine
  columns: ColumnVisibility
  searchTerm: string
  serviceLabel?: string | null
}

const LiveLogRow = memo(function LiveLogRow({
  line,
  columns,
  searchTerm,
  serviceLabel,
}: LiveLogRowProps) {
  // ANSI conversion is unconditional — the server may pass through escape
  // codes from any container stdout. ansi-to-html escapes XML so injection
  // through log content is not a concern. We then layer a search highlight
  // over the resulting HTML by splitting on the search term — done in plain
  // text so we don't mangle ANSI-converted spans.
  const messageHtml = useMemo(() => {
    const html = ansiConverter.toHtml(line.message)
    if (!searchTerm) return html
    const escaped = searchTerm.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    return html.replace(
      new RegExp(`(${escaped})`, 'gi'),
      '<mark class="bg-yellow-200 dark:bg-yellow-800 rounded px-1">$1</mark>'
    )
  }, [line.message, searchTerm])

  return (
    <div className="flex items-start gap-2 py-0.5 px-2 font-mono text-xs select-text hover:bg-muted/50">
      {columns.timestamp && (
        <span className="text-muted-foreground shrink-0 tabular-nums w-[85px]">
          {formatTimestamp(line.timestamp)}
        </span>
      )}
      {columns.level && (
        <Badge
          variant="outline"
          className={cn(
            'shrink-0 text-[10px] font-medium px-1.5 py-0 h-[18px] leading-[18px] rounded-sm',
            LEVEL_COLORS[line.level]
          )}
        >
          {line.level}
        </Badge>
      )}
      {columns.service && serviceLabel && (
        <span
          className="text-muted-foreground shrink-0 w-[70px] truncate"
          title={serviceLabel}
        >
          {serviceLabel}
        </span>
      )}
      <span
        className="whitespace-pre-wrap break-all min-w-0 flex-1"
        dangerouslySetInnerHTML={{ __html: messageHtml }}
      />
    </div>
  )
})

interface ContainerLogsViewerProps {
  fetchUrl: string
  containerId: string
  // Label shown in the Service column. Usually the container's service name;
  // falls back to the container name if unavailable. Left as a prop (rather
  // than derived from the WS payload) because Docker stdout has no service
  // metadata — the parent already knows what container this is.
  serviceName?: string | null
}

export function ContainerLogsViewer({
  fetchUrl,
  serviceName,
}: ContainerLogsViewerProps) {
  const wsUrl = (() => {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const path = fetchUrl.replace(/^https?:\/\/[^/]+/, '')
    return `${protocol}//${window.location.host}${path}`
  })()

  const {
    logs,
    filteredLogs,
    connectionStatus,
    errorMessage,
    searchTerm,
    selectedLevels,
    currentMatchIndex,
    autoScroll,
    showTimestamps,
    paused,
    bufferedCount,
    parentRef,
    virtualizer,
    setSearchTerm,
    setSelectedLevels,
    setShowTimestamps,
    setPaused,
    resumeAndFlush,
    scrollToBottom,
    handleScroll,
    handleNextMatch,
    handlePrevMatch,
  } = useLogStream({ wsUrl })

  const [columns, setColumns] = useState<ColumnVisibility>(loadColumns)
  useEffect(() => {
    try {
      window.localStorage.setItem(COLUMNS_STORAGE_KEY, JSON.stringify(columns))
    } catch {
      // Storage may be unavailable (private mode, quota); not worth surfacing.
    }
  }, [columns])

  // Keep the server's timestamp toggle in sync with the column visibility —
  // showing the column without server timestamps would render an empty slot.
  // The hook re-opens the WS when this changes, so we only do it when the
  // user actually wants the column on.
  useEffect(() => {
    if (columns.timestamp && !showTimestamps) {
      setShowTimestamps(true)
    }
  }, [columns.timestamp, showTimestamps, setShowTimestamps])

  const toggleLevel = (level: LiveLogLevel) => {
    setSelectedLevels((prev) =>
      prev.includes(level) ? prev.filter((l) => l !== level) : [...prev, level]
    )
  }

  const handlePauseToggle = () => {
    if (paused) resumeAndFlush()
    else setPaused(true)
  }

  const showJumpToLatest = !paused && !autoScroll && logs.length > 0

  return (
    <TooltipProvider delayDuration={150}>
      <div className="flex flex-col h-full bg-background">
        {/* Toolbar — mirrors history-log-viewer.tsx so the live + history
            views feel like one product. Layout: status dot + result counter
            on the left, search/filter controls on the right. */}
        <div className="flex-shrink-0 flex flex-wrap items-center gap-2 px-4 sm:px-6 lg:px-8 py-2 border-b">
          <div
            className={cn(
              'flex items-center gap-1.5 text-xs',
              connectionStatus === 'connected'
                ? 'text-emerald-600 dark:text-emerald-400'
                : connectionStatus === 'connecting'
                  ? 'text-yellow-600 dark:text-yellow-400'
                  : 'text-red-600 dark:text-red-400'
            )}
          >
            <span
              className={cn(
                'h-1.5 w-1.5 rounded-full',
                connectionStatus === 'connected'
                  ? paused
                    ? 'bg-amber-500'
                    : 'bg-emerald-500'
                  : connectionStatus === 'connecting'
                    ? 'bg-yellow-500'
                    : 'bg-red-500'
              )}
            />
            <span>
              {connectionStatus === 'connected' && (paused ? 'Paused' : 'Live')}
              {connectionStatus === 'connecting' && 'Connecting…'}
              {connectionStatus === 'error' && 'Disconnected'}
            </span>
          </div>

          <span className="text-xs text-muted-foreground tabular-nums">
            {filteredLogs.length.toLocaleString()}
            {filteredLogs.length !== logs.length
              ? ` of ${logs.length.toLocaleString()}`
              : ''}{' '}
            {logs.length === 1 ? 'line' : 'lines'}
          </span>

          {/* Level filter chips inline with the toolbar — wraps below the
              counter on narrow screens, sits on the same line on wide ones.
              Hidden on the smallest widths so the search input has room; the
              chips reappear from sm: upward. Empty selection means "show
              all"; an active selection adds a "Clear" affordance. */}
          <div className="hidden sm:flex items-center gap-1 flex-wrap">
            {LEVEL_OPTIONS.map((level) => (
              <button
                type="button"
                key={level}
                onClick={() => toggleLevel(level)}
                className={cn(
                  'px-2 py-0.5 text-[10px] font-medium rounded-full border transition-colors leading-[18px]',
                  selectedLevels.includes(level)
                    ? LEVEL_COLORS[level]
                    : 'bg-muted/50 text-muted-foreground border-border hover:bg-muted'
                )}
              >
                {level}
              </button>
            ))}
            {selectedLevels.length > 0 && (
              <button
                type="button"
                onClick={() => setSelectedLevels([])}
                className="px-1.5 py-0.5 text-[10px] text-muted-foreground hover:text-foreground"
              >
                Clear
              </button>
            )}
          </div>

          <div className="ml-auto flex items-center gap-1.5 flex-wrap">
            <div className="relative">
              <Search className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
              <Input
                placeholder="Search logs…"
                value={searchTerm}
                onChange={(e) => setSearchTerm(e.target.value)}
                className="h-8 pl-7 pr-2 text-sm w-48 sm:w-56"
              />
            </div>
            {searchTerm && (
              <>
                <span className="text-xs text-muted-foreground tabular-nums">
                  {filteredLogs.length === 0
                    ? '0'
                    : currentMatchIndex >= 0
                      ? `${currentMatchIndex + 1}/${filteredLogs.length}`
                      : filteredLogs.length}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 w-8 p-0"
                  onClick={handlePrevMatch}
                  disabled={filteredLogs.length === 0}
                  aria-label="Previous match"
                >
                  ↑
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 w-8 p-0"
                  onClick={handleNextMatch}
                  disabled={filteredLogs.length === 0}
                  aria-label="Next match"
                >
                  ↓
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-8 w-8 p-0"
                  onClick={() => setSearchTerm('')}
                  aria-label="Clear search"
                >
                  <X className="h-3.5 w-3.5" />
                </Button>
              </>
            )}

            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 gap-1.5"
                  aria-label="Toggle columns"
                >
                  <Columns3 className="h-3.5 w-3.5" />
                  <span className="hidden sm:inline text-xs">Columns</span>
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-44">
                <DropdownMenuLabel>Show columns</DropdownMenuLabel>
                <DropdownMenuSeparator />
                <DropdownMenuCheckboxItem
                  checked={columns.timestamp}
                  onCheckedChange={(v) =>
                    setColumns((c) => ({ ...c, timestamp: v === true }))
                  }
                >
                  Timestamp
                </DropdownMenuCheckboxItem>
                <DropdownMenuCheckboxItem
                  checked={columns.level}
                  onCheckedChange={(v) =>
                    setColumns((c) => ({ ...c, level: v === true }))
                  }
                >
                  Level
                </DropdownMenuCheckboxItem>
                <DropdownMenuCheckboxItem
                  checked={columns.service}
                  onCheckedChange={(v) =>
                    setColumns((c) => ({ ...c, service: v === true }))
                  }
                  disabled={!serviceName}
                >
                  Service
                </DropdownMenuCheckboxItem>
              </DropdownMenuContent>
            </DropdownMenu>

            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant={paused ? 'default' : 'outline'}
                  size="sm"
                  className="h-8 gap-1.5"
                  onClick={handlePauseToggle}
                  aria-label={paused ? 'Resume tail' : 'Pause tail'}
                >
                  {paused ? (
                    <Play className="h-3.5 w-3.5" />
                  ) : (
                    <Pause className="h-3.5 w-3.5" />
                  )}
                  <span className="hidden sm:inline text-xs">
                    {paused ? 'Resume' : 'Pause'}
                  </span>
                  {paused && bufferedCount > 0 && (
                    <Badge
                      variant="secondary"
                      className="h-4 px-1 text-[10px] tabular-nums"
                    >
                      +{bufferedCount.toLocaleString()}
                    </Badge>
                  )}
                </Button>
              </TooltipTrigger>
              <TooltipContent side="bottom">
                {paused
                  ? bufferedCount > 0
                    ? `Resume to append ${bufferedCount.toLocaleString()} buffered ${
                        bufferedCount === 1 ? 'line' : 'lines'
                      }`
                    : 'Resume tail'
                  : 'Pause to read without rows scrolling past'}
              </TooltipContent>
            </Tooltip>
          </div>
        </div>

        {/* Mobile-only level chip row — wide screens get the chips inline
            with the toolbar above; this fallback keeps level filtering
            reachable on phones where there isn't horizontal room. */}
        <div className="sm:hidden flex-shrink-0 flex gap-1.5 flex-wrap items-center px-4 py-2 border-b">
          {LEVEL_OPTIONS.map((level) => (
            <button
              type="button"
              key={level}
              onClick={() => toggleLevel(level)}
              className={cn(
                'px-2.5 py-0.5 text-xs font-medium rounded-full border transition-colors',
                selectedLevels.includes(level)
                  ? LEVEL_COLORS[level]
                  : 'bg-muted/50 text-muted-foreground border-border hover:bg-muted'
              )}
            >
              {level}
            </button>
          ))}
          {selectedLevels.length > 0 && (
            <button
              type="button"
              onClick={() => setSelectedLevels([])}
              className="px-2.5 py-0.5 text-xs text-muted-foreground hover:text-foreground"
            >
              Clear
            </button>
          )}
        </div>

        {/* Error Alert */}
        {errorMessage && (
          <Alert className="flex-shrink-0 mx-4 mt-2 border-destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>{errorMessage}</AlertDescription>
          </Alert>
        )}

        {/* Logs Container — wrapped in a bordered rounded card so the live
            tail visually matches the history viewer. The outer padding lets
            the card breathe inside the parent flex column; the inner div is
            the scroll viewport. */}
        <div className="flex-1 min-h-0 p-4 relative">
          <div className="h-full border rounded-lg bg-background overflow-hidden">
            <div
              ref={parentRef}
              className={cn(
                'h-full overflow-auto text-foreground select-text',
                connectionStatus === 'connecting' && 'opacity-50'
              )}
              onScroll={handleScroll}
            >
              {logs.length === 0 && connectionStatus === 'connecting' && (
                <div className="p-4 text-xs text-muted-foreground font-mono">
                  Connecting to logs…
                </div>
              )}

              {logs.length === 0 && connectionStatus !== 'connecting' && (
                <div className="p-4 text-xs text-muted-foreground font-mono">
                  No logs available
                </div>
              )}

              {logs.length > 0 && filteredLogs.length === 0 && (
                <div className="p-4 text-xs text-muted-foreground font-mono">
                  No lines match the current filters
                </div>
              )}

              <div
                style={{
                  height: `${virtualizer.getTotalSize()}px`,
                  width: '100%',
                  position: 'relative',
                }}
              >
                {virtualizer.getVirtualItems().map((virtualItem) => {
                  const log = filteredLogs[virtualItem.index]
                  if (!log) return null
                  return (
                    <div
                      key={virtualItem.key}
                      data-index={virtualItem.index}
                      ref={virtualizer.measureElement}
                      style={{
                        position: 'absolute',
                        top: `${virtualItem.start}px`,
                        left: 0,
                        width: '100%',
                        // No fixed height — rows size to their (possibly
                        // wrapped) content; measureElement reports it back.
                      }}
                    >
                      <LiveLogRow
                        line={log}
                        columns={columns}
                        searchTerm={searchTerm}
                        serviceLabel={serviceName}
                      />
                    </div>
                  )
                })}
              </div>
            </div>
          </div>

          {/* Floating "Jump to latest" pill — shown when the user has
              scrolled up and tail-follow has disengaged. One click both
              snaps back to the bottom AND re-arms auto-scroll. This is the
              escape hatch from "I scrolled up to read and now I'm lost". */}
          {showJumpToLatest && (
            <div className="absolute bottom-8 left-1/2 -translate-x-1/2 pointer-events-none">
              <Button
                size="sm"
                variant="secondary"
                className="pointer-events-auto h-8 gap-1.5 shadow-md"
                onClick={scrollToBottom}
              >
                <ArrowDown className="h-3.5 w-3.5" />
                Jump to latest
              </Button>
            </div>
          )}
        </div>
      </div>
    </TooltipProvider>
  )
}
