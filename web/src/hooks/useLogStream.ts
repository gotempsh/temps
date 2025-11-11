import { useCallback, useEffect, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'

export interface UseLogStreamOptions {
  wsUrl: string
  onError?: (error: string) => void
}

export interface UseLogStreamReturn {
  logs: string[]
  filteredLogs: string[]
  connectionStatus: 'connecting' | 'connected' | 'error'
  errorMessage: string
  searchTerm: string
  currentMatchIndex: number
  autoScroll: boolean
  showTimestamps: boolean
  parentRef: React.RefObject<HTMLDivElement | null>
  virtualizer: ReturnType<typeof useVirtualizer<HTMLDivElement, Element>>
  setSearchTerm: (term: string) => void
  setAutoScroll: (scroll: boolean) => void
  setShowTimestamps: (show: boolean) => void
  scrollToMatch: (index: number) => void
  handleScroll: (event: React.UIEvent<HTMLDivElement>) => void
  handleNextMatch: () => void
  handlePrevMatch: () => void
}

function estimateLineHeight(content: string, containerWidth: number) {
  const averageCharWidth = 9
  const lineHeight = 20
  const minHeight = 24

  if (!content || !containerWidth) return minHeight

  const paddingHeight = 8
  const effectiveWidth = containerWidth - 32
  const charactersPerLine = Math.max(
    1,
    Math.floor(effectiveWidth / averageCharWidth)
  )
  const estimatedLines = Math.max(
    1,
    Math.ceil(content.length / charactersPerLine)
  )

  return Math.max(minHeight, lineHeight * estimatedLines + paddingHeight)
}

export function useLogStream({
  wsUrl,
  onError,
}: UseLogStreamOptions): UseLogStreamReturn {
  const [logs, setLogs] = useState<string[]>([])
  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'connected' | 'error'
  >('connecting')
  const [errorMessage, setErrorMessage] = useState('')
  const [searchTerm, setSearchTerm] = useState('')
  const [currentMatchIndex, setCurrentMatchIndex] = useState(-1)
  const [autoScroll, setAutoScroll] = useState(true)
  const [showTimestamps, setShowTimestamps] = useState(false)
  const parentRef = useRef<HTMLDivElement>(null)
  const wsRef = useRef<WebSocket | null>(null)
  const isConnectingRef = useRef(false)
  const containerWidth = useRef<number>(0)

  const filteredLogs = searchTerm
    ? logs.filter((log) => log.toLowerCase().includes(searchTerm.toLowerCase()))
    : logs

  const virtualizer = useVirtualizer({
    count: filteredLogs.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (index) => {
      return estimateLineHeight(filteredLogs[index], containerWidth.current)
    },
    overscan: 5,
    measureElement: (element) => {
      return element?.getBoundingClientRect().height ?? 0
    },
  })

  // WebSocket connection effect
  useEffect(() => {
    if (isConnectingRef.current) return

    setLogs([])
    setErrorMessage('')
    isConnectingRef.current = true

    try {
      // Add timestamps query parameter to request server-side timestamps
      const url = new URL(
        wsUrl,
        typeof window !== 'undefined'
          ? window.location.origin
          : 'http://localhost'
      )
      url.searchParams.set('timestamps', showTimestamps.toString())

      const ws = new WebSocket(url.toString())

      ws.onopen = () => {
        setConnectionStatus('connected')
        setErrorMessage('')
      }

      ws.onmessage = (event) => {
        try {
          const parsed = JSON.parse(event.data)

          if (parsed.error && parsed.stack) {
            const formattedLog = `ERROR: ${parsed.error}\n${parsed.stack}`
            setLogs((prev) => [...prev, formattedLog])
          } else if (parsed.message) {
            setLogs((prev) => [...prev, parsed.message])
          } else if (parsed.log) {
            setLogs((prev) => [...prev, parsed.log])
          } else {
            setLogs((prev) => [...prev, JSON.stringify(parsed, null, 2)])
          }
        } catch {
          const line = event.data.trim()
          if (line) {
            setLogs((prev) => [...prev, line])
          }
        }
      }

      ws.onerror = () => {
        setConnectionStatus('error')
        const msg = 'Failed to connect to logs stream'
        setErrorMessage(msg)
        onError?.(msg)
      }

      ws.onclose = () => {
        setConnectionStatus('error')
        isConnectingRef.current = false
      }

      wsRef.current = ws
    } catch (error) {
      setConnectionStatus('error')
      const msg = error instanceof Error ? error.message : 'Connection failed'
      setErrorMessage(msg)
      onError?.(msg)
      isConnectingRef.current = false
    }

    return () => {
      if (wsRef.current) {
        wsRef.current.close()
      }
      isConnectingRef.current = false
    }
  }, [wsUrl, onError, showTimestamps])

  // Auto-scroll effect with proper timing for virtualizer
  useEffect(() => {
    if (autoScroll && parentRef.current && logs.length > 0) {
      // Schedule scroll after virtualizer updates
      requestAnimationFrame(() => {
        if (parentRef.current) {
          parentRef.current.scrollTop = parentRef.current.scrollHeight
        }
      })
    }
  }, [logs, autoScroll])

  // Handle scroll to detect if user is at bottom
  const handleScroll = (event: React.UIEvent<HTMLDivElement>) => {
    const { scrollTop, scrollHeight, clientHeight } = event.currentTarget
    const isAtBottom = scrollHeight - scrollTop - clientHeight < 1
    setAutoScroll(isAtBottom)
  }

  // Add ResizeObserver to track container width
  useEffect(() => {
    if (parentRef.current) {
      const resizeObserver = new ResizeObserver((entries) => {
        containerWidth.current = entries[0].contentRect.width
        virtualizer.measure()
      })

      resizeObserver.observe(parentRef.current)
      return () => resizeObserver.disconnect()
    }
  }, [virtualizer])

  // Search and match scrolling
  const scrollToMatch = useCallback(
    (index: number) => {
      if (index >= 0 && index < filteredLogs.length) {
        setCurrentMatchIndex(index)
        const element = document.querySelector(`[data-match-index="${index}"]`)
        element?.scrollIntoView({
          behavior: 'smooth',
          block: 'center',
        })
      }
    },
    [filteredLogs.length]
  )

  const handleNextMatch = () => {
    if (filteredLogs.length === 0) return
    const nextIndex = (currentMatchIndex + 1) % filteredLogs.length
    scrollToMatch(nextIndex)
  }

  const handlePrevMatch = () => {
    if (filteredLogs.length === 0) return
    const prevIndex =
      currentMatchIndex <= 0 ? filteredLogs.length - 1 : currentMatchIndex - 1
    scrollToMatch(prevIndex)
  }

  return {
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
    scrollToMatch,
    handleScroll,
    handleNextMatch,
    handlePrevMatch,
  }
}
