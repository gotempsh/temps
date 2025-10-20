'use client'

import { useEffect, useRef, useState, useCallback } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { ClipboardAddon } from '@xterm/addon-clipboard'
import { WebglAddon } from '@xterm/addon-webgl'
import 'xterm/css/xterm.css'
import { DevProjectDto } from '@/api/client'
import { useTheme } from 'next-themes'

// Debounce function to prevent excessive resize calls
function debounce<T extends (...args: any[]) => void>(
  func: T,
  wait: number
): T {
  let timeout: number
  return ((...args: any[]) => {
    clearTimeout(timeout)
    timeout = setTimeout(() => func(...args), wait) as unknown as number
  }) as T
}

interface ShellProps {
  selectedProject: DevProjectDto
  sessionId: string
  isActive: boolean
  provider: 'claude' | 'cursor' | 'terminal'
  terminalId?: string
}
function Shell({
  selectedProject,
  sessionId,
  isActive,
  provider = 'terminal',
  terminalId,
}: ShellProps) {
  const terminalRef = useRef<HTMLDivElement | null>(null)
  const terminal = useRef<Terminal | null>(null)
  const fitAddon = useRef<FitAddon | null>(null)
  const ws = useRef<WebSocket | null>(null)
  const [isConnected, setIsConnected] = useState(false)
  const [isInitialized, setIsInitialized] = useState(false)
  const [isRestarting, setIsRestarting] = useState(false)
  const [lastSessionId, setLastSessionId] = useState<string | null>(null)
  const [isConnecting, setIsConnecting] = useState(false)
  const [terminalSize, setTerminalSize] = useState<{
    cols: number
    rows: number
  }>({ cols: 0, rows: 0 })
  const { resolvedTheme } = useTheme()

  // Helper function to update terminal size state
  const updateTerminalSize = useCallback(() => {
    if (terminal.current) {
      setTerminalSize({
        cols: terminal.current.cols || 0,
        rows: terminal.current.rows || 0,
      })
    }
  }, [])

  // Debounced function to send resize messages to WebSocket
  const debouncedSendResize = useCallback(
    debounce(() => {
      if (
        ws.current &&
        ws.current.readyState === WebSocket.OPEN &&
        terminal.current &&
        isActive
      ) {
        // Double-check terminal is still visible before sending resize
        if (terminalRef.current) {
          const rect = terminalRef.current.getBoundingClientRect()
          if (rect.width === 0 || rect.height === 0) {
            console.log('Debounced resize: Terminal not visible, skipping')
            return
          }
        }

        ws.current.send(
          JSON.stringify({
            message_type: 'resize',
            session_id: sessionId || '',
            cols: terminal.current.cols || 0,
            rows: terminal.current.rows || 0,
          })
        )
        updateTerminalSize()
      }
    }, 300),
    [sessionId, updateTerminalSize, isActive]
  )

  // Update terminal theme when system theme changes
  useEffect(() => {
    if (terminal.current && resolvedTheme) {
      const theme = getTerminalTheme()
      terminal.current.options.theme = theme
    }
  }, [resolvedTheme])

  // Get terminal theme based on current theme
  const getTerminalTheme = () => {
    if (resolvedTheme === 'light') {
      return {
        // Light theme colors
        background: '#ffffff',
        foreground: '#333333',
        cursor: '#333333',
        cursorAccent: '#ffffff',
        selectionBackground: '#add6ff',
        selectionForeground: '#333333',

        // Standard ANSI colors (0-7) - lighter versions
        black: '#000000',
        red: '#cd3131',
        green: '#00bc00',
        yellow: '#949800',
        blue: '#0451a5',
        magenta: '#bc05bc',
        cyan: '#0598bc',
        white: '#555555',

        // Bright ANSI colors (8-15)
        brightBlack: '#797979',
        brightRed: '#ff5555',
        brightGreen: '#50fa7b',
        brightYellow: '#f1fa8c',
        brightBlue: '#5890ff',
        brightMagenta: '#ff79c6',
        brightCyan: '#8be9fd',
        brightWhite: '#ffffff',
      }
    } else {
      // Dark theme colors (original)
      return {
        background: '#1e1e1e',
        foreground: '#d4d4d4',
        cursor: '#ffffff',
        cursorAccent: '#1e1e1e',
        selectionBackground: '#264f78',
        selectionForeground: '#ffffff',

        // Standard ANSI colors (0-7)
        black: '#000000',
        red: '#cd3131',
        green: '#0dbc79',
        yellow: '#e5e510',
        blue: '#2472c8',
        magenta: '#bc3fbc',
        cyan: '#11a8cd',
        white: '#e5e5e5',

        // Bright ANSI colors (8-15)
        brightBlack: '#666666',
        brightRed: '#f14c4c',
        brightGreen: '#23d18b',
        brightYellow: '#f5f543',
        brightBlue: '#3b8eea',
        brightMagenta: '#d670d6',
        brightCyan: '#29b8db',
        brightWhite: '#ffffff',
      }
    }
  }

  // WebSocket connection function (called manually)
  const connectWebSocket = useCallback(async () => {
    if (isConnecting || isConnected) return

    // Clean up any existing connection first
    if (ws.current) {
      ws.current.close()
      ws.current = null
    }

    setIsConnecting(true)

    try {
      // A better approach is to construct the WebSocket URL dynamically based on the current location:
      const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
      const host = window.location.host
      const wsUrl = `${protocol}://${host}/api/dev-projects/${selectedProject.id}/terminal`

      ws.current = new WebSocket(wsUrl)

      ws.current.onopen = () => {
        console.log('WebSocket connection opened')
        setIsConnected(true)
        setIsConnecting(false)

        // Wait for terminal to be ready, then fit and send dimensions
        setTimeout(() => {
          if (fitAddon.current && terminal.current) {
            // Force a fit to ensure proper dimensions
            fitAddon.current.fit()

            // Wait a bit more for fit to complete, then send dimensions
            setTimeout(() => {
              if (
                !terminal.current ||
                !ws.current ||
                ws.current.readyState !== WebSocket.OPEN
              ) {
                return
              }

              const initPayload = {
                message_type: 'init',
                dev_project: selectedProject,
                session_id: sessionId || '',
                has_session: !!sessionId,
                provider: provider,
                terminal_type: provider, // Use provider as terminal_type
                cols: terminal.current.cols || 0,
                rows: terminal.current.rows || 0,
              }

              console.log('Sending init payload:', initPayload)
              ws.current.send(JSON.stringify(initPayload))

              // Update terminal size state
              updateTerminalSize()
            }, 50)
          }
        }, 200)
      }

      ws.current.onmessage = (event) => {
        console.log('Received WebSocket message:', event.data)

        try {
          const data = JSON.parse(event.data)
          console.log('Parsed message:', data)

          if (data.type === 'session') {
            // Server has created/updated session - store session info
            console.log('Received session info:', data)
            // You can store session info if needed
          } else if (data.type === 'output') {
            const output = data.data
            console.log(
              'Received terminal output:',
              output.length,
              'characters'
            )

            if (terminal.current) {
              terminal.current.write(output)
            }
          } else if (data.type === 'url_open') {
            // Handle explicit URL opening requests from server
            window.open(data.url, '_blank')
          } else {
            console.log('Unknown message type:', data.type, data)
          }
        } catch (error) {
          console.error('Failed to parse WebSocket message:', error, event.data)
        }
      }

      ws.current.onclose = (event) => {
        console.log('WebSocket onclose handler triggered')
        console.log('Close event details:', {
          code: event.code,
          reason: event.reason,
          wasClean: event.wasClean,
          timestamp: new Date().toISOString(),
        })

        setIsConnected(false)
        setIsConnecting(false)
        ws.current = null

        // Show connection lost message
        if (terminal.current && event.code !== 1000) {
          // Don't clear on normal closure
          terminal.current.write(
            `\x1b[31mConnection lost (${event.code}). Please reconnect manually.\x1b[0m\r\n`
          )
        }

        console.log('WebSocket connection closed:', event.code, event.reason)
      }

      ws.current.onerror = (error) => {
        console.error('WebSocket error:', error)
        setIsConnected(false)
        setIsConnecting(false)
      }
    } catch {
      setIsConnected(false)
      setIsConnecting(false)
    }
  }, [
    isConnecting,
    isConnected,
    selectedProject,
    sessionId,
    isInitialized,
    updateTerminalSize,
    provider,
  ])

  // Connect to shell function
  const connectToShell = useCallback(() => {
    if (!isInitialized || isConnected || isConnecting) return

    setIsConnecting(true)

    // Start the WebSocket connection
    connectWebSocket()
  }, [isInitialized, isConnected, isConnecting, connectWebSocket])

  // Force fit function
  const forceFit = useCallback(() => {
    if (
      fitAddon.current &&
      terminal.current &&
      terminalRef.current &&
      isActive
    ) {
      try {
        // Only fit if terminal is actually visible and active
        const rect = terminalRef.current.getBoundingClientRect()
        if (rect.width === 0 || rect.height === 0) {
          console.log('Terminal not visible, skipping resize')
          return
        }

        // Fit to container
        fitAddon.current.fit()

        // Send debounced resize message
        debouncedSendResize()
      } catch (error) {
        console.error('Error force-fitting terminal:', error)
      }
    }
  }, [debouncedSendResize, isActive])

  // Listen for terminal fit events
  useEffect(() => {
    if (!terminalRef.current) return

    const handleFitEvent = (event: CustomEvent) => {
      if (event.detail.terminalId === terminalId) {
        forceFit()
      }
    }

    const terminalElement = terminalRef.current
    terminalElement.addEventListener(
      'terminal-fit',
      handleFitEvent as EventListener
    )

    return () => {
      terminalElement.removeEventListener(
        'terminal-fit',
        handleFitEvent as EventListener
      )
    }
  }, [forceFit, terminalId])

  // Disconnect from shell function
  const disconnectFromShell = () => {
    if (ws.current) {
      ws.current.close()
      ws.current = null
    }

    // Clear terminal content completely
    if (terminal.current) {
      terminal.current.clear()
      terminal.current.write('\x1b[2J\x1b[H') // Clear screen and move cursor to home
    }

    setIsConnected(false)
    setIsConnecting(false)
  }

  // Restart shell function
  const restartShell = () => {
    setIsRestarting(true)

    // Close existing WebSocket
    if (ws.current) {
      ws.current.close()
      ws.current = null
    }

    // Clear and dispose existing terminal
    if (terminal.current) {
      // Dispose terminal immediately without writing text
      terminal.current.dispose()
      terminal.current = null
      fitAddon.current = null
    }

    // Reset states
    setIsConnected(false)
    setIsInitialized(false)

    // Force re-initialization after cleanup
    setTimeout(() => {
      setIsRestarting(false)
    }, 200)
  }

  // Watch for session changes and restart shell
  useEffect(() => {
    const currentSessionId = sessionId || null

    // Disconnect when session changes (user will need to manually reconnect)
    if (
      lastSessionId !== null &&
      lastSessionId !== currentSessionId &&
      isInitialized
    ) {
      // Disconnect from current shell
      disconnectFromShell()
    }

    setLastSessionId(currentSessionId)
  }, [sessionId, isInitialized, lastSessionId])

  // Initialize terminal when component mounts
  useEffect(() => {
    if (!terminalRef.current || !selectedProject || isRestarting) {
      return
    }

    if (terminal.current) {
      return
    }

    // Initialize new terminal
    terminal.current = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      allowProposedApi: true, // Required for clipboard addon
      allowTransparency: false,
      macOptionIsMeta: true,
      macOptionClickForcesSelection: false,
      // Use dynamic theme based on current theme
      theme: {
        ...getTerminalTheme(),
        // Extended colors for better Claude output
        extendedAnsi: [
          // 16-color palette extension for 256-color support
          '#000000',
          '#800000',
          '#008000',
          '#808000',
          '#000080',
          '#800080',
          '#008080',
          '#c0c0c0',
          '#808080',
          '#ff0000',
          '#00ff00',
          '#ffff00',
          '#0000ff',
          '#ff00ff',
          '#00ffff',
          '#ffffff',
        ],
      },
    })

    fitAddon.current = new FitAddon()
    const clipboardAddon = new ClipboardAddon()
    const webglAddon = new WebglAddon()

    terminal.current.loadAddon(fitAddon.current)
    terminal.current.loadAddon(clipboardAddon)

    try {
      terminal.current.loadAddon(webglAddon)
    } catch {
      // WebGL addon failed to load, continue without it
    }

    terminal.current.open(terminalRef.current)

    // Wait for terminal to be fully rendered, then fit
    setTimeout(() => {
      if (fitAddon.current) {
        fitAddon.current.fit()
      }
    }, 50)

    // Add keyboard shortcuts for copy/paste
    terminal.current.attachCustomKeyEventHandler((event) => {
      // Ctrl+C or Cmd+C for copy (when text is selected)
      if (
        (event.ctrlKey || event.metaKey) &&
        event.key === 'c' &&
        terminal.current?.hasSelection()
      ) {
        document.execCommand('copy')
        return false
      }

      // Ctrl+V or Cmd+V for paste
      if ((event.ctrlKey || event.metaKey) && event.key === 'v') {
        navigator.clipboard
          .readText()
          .then((text) => {
            if (ws.current && ws.current.readyState === WebSocket.OPEN) {
              ws.current.send(
                JSON.stringify({
                  message_type: 'input',
                  session_id: sessionId || '',
                  data: text,
                })
              )
            }
          })
          .catch(() => {
            // Failed to read clipboard
          })
        return false
      }

      return true
    })

    // Ensure terminal takes full space and notify backend of size
    setTimeout(() => {
      if (fitAddon.current) {
        fitAddon.current.fit()
        // Send debounced resize message
        debouncedSendResize()
      }
    }, 100)

    setIsInitialized(true)

    // Handle terminal input
    terminal.current.onData((data) => {
      if (ws.current && ws.current.readyState === WebSocket.OPEN) {
        ws.current?.send(
          JSON.stringify({
            message_type: 'input',
            session_id: sessionId || '',
            data: data,
          })
        )
      }
    })

    // Add resize observer to handle container size changes
    const resizeObserver = new ResizeObserver(() => {
      if (
        fitAddon.current &&
        terminal.current &&
        isActive &&
        terminalRef.current
      ) {
        // Check if terminal is actually visible
        const rect = terminalRef.current.getBoundingClientRect()
        if (rect.width === 0 || rect.height === 0) {
          console.log('ResizeObserver: Terminal not visible, skipping resize')
          return
        }

        setTimeout(() => {
          fitAddon.current?.fit()
          // Send debounced resize message
          debouncedSendResize()
        }, 50)
      }
    })

    if (terminalRef.current) {
      resizeObserver.observe(terminalRef.current)
    }

    return () => {
      resizeObserver.disconnect()

      // Clean up terminal and WebSocket
      if (terminal.current) {
        terminal.current.dispose()
        terminal.current = null
        fitAddon.current = null
      }

      if (ws.current) {
        ws.current.close()
        ws.current = null
      }
    }
  }, [selectedProject, sessionId, isRestarting, updateTerminalSize])

  // Fit terminal when tab becomes active
  useEffect(() => {
    if (!isActive || !isInitialized) return

    // Force a refresh and fit when terminal becomes active
    const fitTerminal = () => {
      if (fitAddon.current && terminal.current && terminalRef.current) {
        try {
          // Force the terminal to be visible first
          if (terminalRef.current.style.display === 'none') {
            terminalRef.current.style.display = 'block'
          }

          // Fit the terminal to container
          fitAddon.current.fit()

          // Send debounced resize message
          debouncedSendResize()
        } catch (error) {
          console.error('Error fitting terminal:', error)
        }
      }
    }

    // Multiple attempts with increasing delays
    const timer1 = setTimeout(fitTerminal, 10)
    const timer2 = setTimeout(fitTerminal, 100)
    const timer3 = setTimeout(fitTerminal, 250)
    const timer4 = setTimeout(fitTerminal, 500)

    return () => {
      clearTimeout(timer1)
      clearTimeout(timer2)
      clearTimeout(timer3)
      clearTimeout(timer4)
    }
  }, [isActive, isInitialized, updateTerminalSize, sessionId])

  if (!selectedProject) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-center text-muted-foreground">
          <div className="w-16 h-16 mx-auto mb-4 bg-muted rounded-full flex items-center justify-center">
            <svg
              className="w-8 h-8 text-muted-foreground"
              fill="none"
              stroke="currentColor"
              viewBox="0 0 24 24"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v14a2 2 0 002 2z"
              />
            </svg>
          </div>
          <h3 className="text-lg font-semibold mb-2">Select a Project</h3>
          <p>Choose a project to open an interactive shell in that directory</p>
        </div>
      </div>
    )
  }

  return (
    <div className="h-full flex flex-col bg-background w-full">
      {/* Header */}
      <div className="shrink-0 bg-muted border-b border-border px-4 py-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center space-x-2">
            <div
              className={`w-2 h-2 rounded-full ${isConnected ? 'bg-green-500' : 'bg-red-500'}`}
            />
            {sessionId &&
              (() => {
                const displaySessionName =
                  provider === 'cursor'
                    ? selectedProject.name || 'Untitled Session'
                    : selectedProject.name || 'New Session'
                return (
                  <span className="text-xs text-primary font-medium">
                    ({displaySessionName.slice(0, 30)}...)
                  </span>
                )
              })()}
            {!sessionId && (
              <span className="text-xs text-muted-foreground">
                (New Session)
              </span>
            )}
            {sessionId && (
              <span className="text-xs text-foreground/70 font-mono">
                ID: {sessionId.slice(0, 8)}...
              </span>
            )}
            {terminalSize.cols > 0 && terminalSize.rows > 0 && (
              <span className="text-xs text-green-600 dark:text-green-400 font-mono">
                {terminalSize.cols}×{terminalSize.rows}
              </span>
            )}
            {!isInitialized && (
              <span className="text-xs text-yellow-600 dark:text-yellow-400">
                (Initializing...)
              </span>
            )}
            {isRestarting && (
              <span className="text-xs text-blue-600 dark:text-blue-400">
                (Restarting...)
              </span>
            )}
          </div>
          <div className="flex items-center space-x-3">
            {isConnected && (
              <button
                onClick={disconnectFromShell}
                className="px-3 py-1 text-xs bg-red-600 text-white rounded hover:bg-red-700 flex items-center space-x-1"
                title="Disconnect from shell"
              >
                <svg
                  className="w-3 h-3"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M6 18L18 6M6 6l12 12"
                  />
                </svg>
                <span>Disconnect</span>
              </button>
            )}

            {isInitialized && !isConnected && !isConnecting && (
              <button
                onClick={connectToShell}
                className="px-3 py-1 text-xs bg-green-600 text-white rounded hover:bg-green-700 flex items-center space-x-1"
                title="Connect terminal"
              >
                <svg
                  className="w-3 h-3"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M7 4V2a1 1 0 011-1h8a1 1 0 011 1v2m-9 0h10a2 2 0 012 2v10a2 2 0 01-2 2H6a2 2 0 01-2-2V6a2 2 0 012-2z"
                  />
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M10 10l2 2 4-4"
                  />
                </svg>
                <span>Connect</span>
              </button>
            )}

            <button
              onClick={restartShell}
              disabled={isRestarting || isConnected}
              className="text-xs text-muted-foreground hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed flex items-center space-x-1"
              title="Restart Shell (disconnect first)"
            >
              <svg
                className="w-3 h-3"
                fill="none"
                stroke="currentColor"
                viewBox="0 0 24 24"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
                />
              </svg>
              <span>Restart</span>
            </button>
          </div>
        </div>
      </div>

      {/* Terminal */}
      <div className="flex-1 p-2 overflow-hidden relative">
        <div
          ref={terminalRef}
          className="h-full w-full focus:outline-none"
          style={{ outline: 'none' }}
        />

        {/* Loading state */}
        {!isInitialized && (
          <div className="absolute inset-0 flex items-center justify-center bg-background/90">
            <div className="text-foreground">Loading terminal...</div>
          </div>
        )}

        {/* Connecting state */}
        {isConnecting && (
          <div className="absolute inset-0 flex items-center justify-center bg-background/90 p-4">
            <div className="text-center max-w-sm w-full">
              <div className="flex items-center justify-center space-x-3 text-yellow-600 dark:text-yellow-400">
                <div className="w-6 h-6 animate-spin rounded-full border-2 border-yellow-600 dark:border-yellow-400 border-t-transparent"></div>
                <span className="text-base font-medium">
                  Connecting to shell...
                </span>
              </div>
              <p className="text-muted-foreground text-sm mt-3 px-2">
                Starting {provider} CLI in {selectedProject.name}
              </p>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

export default Shell
