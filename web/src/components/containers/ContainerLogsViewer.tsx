'use client'

import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Checkbox } from '@/components/ui/checkbox'
import { cn } from '@/lib/utils'
import { AlertCircle, Search } from 'lucide-react'
import { useLogStream } from '@/hooks/useLogStream'

interface ContainerLogsViewerProps {
  fetchUrl: string
  containerId: string
}

export function ContainerLogsViewer({
  fetchUrl,
}: ContainerLogsViewerProps) {
  // Convert HTTP URL to WebSocket URL
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
    currentMatchIndex,
    autoScroll,
    showTimestamps,
    parentRef,
    virtualizer,
    setSearchTerm,
    setAutoScroll,
    setShowTimestamps,
    handleScroll,
    handleNextMatch,
    handlePrevMatch,
  } = useLogStream({ wsUrl })

  const handleSearch = (e: React.ChangeEvent<HTMLInputElement>) => {
    setSearchTerm(e.target.value)
  }

  return (
    <div className="flex flex-col h-full bg-background">
      {/* Toolbar */}
      <div className="flex-shrink-0 border-b bg-muted/30 p-4 space-y-3">
        <div className="flex items-center gap-2">
          <div className="flex-1 flex items-center gap-2">
            <Search className="h-4 w-4 text-muted-foreground" />
            <Input
              placeholder="Search logs..."
              value={searchTerm}
              onChange={handleSearch}
              className="flex-1"
            />
            {searchTerm && (
              <span className="text-sm text-muted-foreground">
                {currentMatchIndex >= 0
                  ? `${currentMatchIndex + 1} / ${filteredLogs.length}`
                  : `${filteredLogs.length} matches`}
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={handlePrevMatch}
              disabled={!searchTerm || filteredLogs.length === 0}
            >
              ↑
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={handleNextMatch}
              disabled={!searchTerm || filteredLogs.length === 0}
            >
              ↓
            </Button>
          </div>
        </div>

        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2">
            <Checkbox
              id="auto-scroll"
              checked={autoScroll}
              onCheckedChange={(checked) => setAutoScroll(checked as boolean)}
            />
            <Label htmlFor="auto-scroll" className="text-sm cursor-pointer">
              Auto-scroll
            </Label>
          </div>
          <div className="flex items-center gap-2">
            <Checkbox
              id="timestamps"
              checked={showTimestamps}
              onCheckedChange={(checked) =>
                setShowTimestamps(checked as boolean)
              }
            />
            <Label htmlFor="timestamps" className="text-sm cursor-pointer">
              Show timestamps
            </Label>
          </div>
          <div
            className={cn(
              'h-2 w-2 rounded-full',
              connectionStatus === 'connected'
                ? 'bg-green-500'
                : connectionStatus === 'connecting'
                  ? 'bg-yellow-500'
                  : 'bg-red-500'
            )}
          />
          <span className="text-xs text-muted-foreground">
            {connectionStatus === 'connected' && 'Connected'}
            {connectionStatus === 'connecting' && 'Connecting...'}
            {connectionStatus === 'error' && 'Disconnected'}
          </span>
        </div>
      </div>

      {/* Error Alert */}
      {errorMessage && (
        <Alert className="flex-shrink-0 m-4 border-destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>{errorMessage}</AlertDescription>
        </Alert>
      )}

      {/* Logs Container */}
      <div
        ref={parentRef}
        className={cn(
          'flex-1 max-h-96 overflow-y-auto bg-white dark:bg-slate-950 text-slate-900 dark:text-slate-50 p-4 font-mono text-sm min-h-0 border border-border',
          connectionStatus === 'connecting' && 'opacity-50'
        )}
        onScroll={handleScroll}
      >
        {logs.length === 0 && connectionStatus === 'connecting' && (
          <div className="text-muted-foreground">Connecting to logs...</div>
        )}

        {logs.length === 0 && connectionStatus !== 'connecting' && (
          <div className="text-muted-foreground">No logs available</div>
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
            const isMatch =
              searchTerm &&
              log.toLowerCase().includes(searchTerm.toLowerCase())

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
                }}
              >
                {isMatch ? (
                  <div
                    className="py-1 break-all"
                    data-match-index={virtualItem.index}
                  >
                    {log.split(new RegExp(`(${searchTerm})`, 'gi')).map(
                      (part, i) =>
                        part.toLowerCase() === searchTerm.toLowerCase() ? (
                          <span
                            key={i}
                            className="bg-yellow-500 text-slate-950 font-bold"
                          >
                            {part}
                          </span>
                        ) : (
                          <span key={i}>{part}</span>
                        )
                    )}
                  </div>
                ) : (
                  <div className="py-1 break-all">{log}</div>
                )}
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
