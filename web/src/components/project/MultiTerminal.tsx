'use client'

import { useState, useCallback } from 'react'
import { Plus, X, Terminal, Brain, MousePointer, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { DevProjectDto } from '@/api/client'
import ProjectDevelopTerminal from './ProjectDevelopTerminal'
import { cn } from '@/lib/utils'

interface TerminalInstance {
  id: string
  type: 'claude' | 'cursor' | 'terminal'
  sessionId: string
  name: string
  isActive: boolean
}

interface MultiTerminalProps {
  devProject: DevProjectDto
  className?: string
}

export function MultiTerminal({ devProject, className }: MultiTerminalProps) {
  const [terminals, setTerminals] = useState<TerminalInstance[]>([])
  const [activeTerminal, setActiveTerminal] = useState<string | null>(null)
  const [terminalRefs, setTerminalRefs] = useState<
    Map<string, React.RefObject<any>>
  >(new Map())

  // Handle terminal tab switching with fit addon
  const handleTabChange = useCallback((terminalId: string) => {
    setActiveTerminal(terminalId)

    // Trigger fit on the newly active terminal after a short delay
    setTimeout(() => {
      // Find the terminal element and trigger fit
      const terminalElement = document.querySelector(
        `[data-terminal-id="${terminalId}"] .xterm`
      )
      if (terminalElement) {
        // Dispatch a custom event to trigger terminal fitting
        const fitEvent = new CustomEvent('terminal-fit', {
          detail: { terminalId },
        })
        terminalElement.dispatchEvent(fitEvent)
      }
    }, 50)
  }, [])

  // Generate unique terminal ID
  const generateTerminalId = () => {
    return `terminal-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`
  }

  // Generate session ID
  const generateSessionId = () => {
    return `session-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`
  }

  // Add new terminal
  const addTerminal = useCallback(
    (type: 'claude' | 'cursor' | 'terminal') => {
      const id = generateTerminalId()
      const sessionId = generateSessionId()
      const typeNames = {
        claude: 'Claude',
        cursor: 'Cursor',
        terminal: 'Terminal',
      }

      const newTerminal: TerminalInstance = {
        id,
        type,
        sessionId,
        name: `${typeNames[type]} ${terminals.length + 1}`,
        isActive: false,
      }

      setTerminals((prev) => [...prev, newTerminal])
      setActiveTerminal(id)
    },
    [terminals.length]
  )

  // Remove terminal
  const removeTerminal = useCallback(
    (terminalId: string) => {
      // Clean up terminal references
      setTerminalRefs((prev) => {
        const newRefs = new Map(prev)
        newRefs.delete(terminalId)
        return newRefs
      })

      setTerminals((prev) => {
        const filtered = prev.filter((t) => t.id !== terminalId)

        // If we're removing the active terminal, switch to another one
        if (activeTerminal === terminalId && filtered.length > 0) {
          setActiveTerminal(filtered[0].id)
        } else if (filtered.length === 0) {
          setActiveTerminal(null)
        }

        return filtered
      })
    },
    [activeTerminal]
  )

  // Clear all terminals
  const clearAllTerminals = useCallback(() => {
    setTerminals([])
    setActiveTerminal(null)
  }, [])

  // Get terminal icon
  const getTerminalIcon = (type: 'claude' | 'cursor' | 'terminal') => {
    switch (type) {
      case 'claude':
        return <Brain className="h-3.5 w-3.5" />
      case 'cursor':
        return <MousePointer className="h-3.5 w-3.5" />
      case 'terminal':
        return <Terminal className="h-3.5 w-3.5" />
    }
  }

  // Get terminal color
  const getTerminalColor = (type: 'claude' | 'cursor' | 'terminal') => {
    switch (type) {
      case 'claude':
        return 'text-orange-400'
      case 'cursor':
        return 'text-blue-400'
      case 'terminal':
        return 'text-green-400'
    }
  }

  return (
    <div className={cn('flex flex-col h-full bg-card', className)}>
      {/* Header with controls */}
      <div className="shrink-0 bg-muted border-b border-border px-3 py-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 text-muted-foreground hover:text-foreground"
                >
                  <Plus className="h-3.5 w-3.5 mr-1" />
                  New Terminal
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                className="z-[10000]"
                side="bottom"
                align="start"
                sideOffset={4}
                container={document.body}
                portalled={true}
              >
                <DropdownMenuItem onClick={() => addTerminal('terminal')}>
                  <Terminal className="h-4 w-4 mr-2" />
                  Terminal
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => addTerminal('claude')}>
                  <Brain className="h-4 w-4 mr-2" />
                  Claude
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => addTerminal('cursor')}>
                  <MousePointer className="h-4 w-4 mr-2" />
                  Cursor
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>

          {terminals.length > 0 && (
            <Button
              variant="ghost"
              size="sm"
              onClick={clearAllTerminals}
              className="h-7 text-muted-foreground hover:text-destructive"
              title="Close all terminals"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          )}
        </div>
      </div>

      {/* Terminal content */}
      {terminals.length === 0 ? (
        <div className="flex-1 flex items-center justify-center text-muted-foreground">
          <div className="text-center">
            <Terminal className="h-16 w-16 mx-auto mb-4 opacity-20" />
            <h3 className="text-lg font-semibold mb-2">No terminals open</h3>
            <p className="text-sm mb-4">Create a new terminal to get started</p>
            <div className="flex gap-2 justify-center">
              <Button
                variant="outline"
                size="sm"
                onClick={() => addTerminal('terminal')}
                className="bg-muted border-border text-muted-foreground hover:bg-accent"
              >
                <Terminal className="h-4 w-4 mr-1" />
                Terminal
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => addTerminal('claude')}
                className="bg-muted border-border text-muted-foreground hover:bg-accent"
              >
                <Brain className="h-4 w-4 mr-1" />
                Claude
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => addTerminal('cursor')}
                className="bg-muted border-border text-muted-foreground hover:bg-accent"
              >
                <MousePointer className="h-4 w-4 mr-1" />
                Cursor
              </Button>
            </div>
          </div>
        </div>
      ) : (
        <Tabs
          value={activeTerminal || ''}
          onValueChange={handleTabChange}
          className="flex-1 flex flex-col"
        >
          {/* Terminal tabs */}
          <TabsList className="h-9 bg-muted border-b border-border rounded-none justify-start p-0">
            {terminals.map((terminal) => (
              <div key={terminal.id} className="flex items-center">
                <TabsTrigger
                  value={terminal.id}
                  className={cn(
                    'h-8 px-3 text-xs rounded-none border-r border-border data-[state=active]:bg-card data-[state=active]:text-foreground flex items-center gap-1.5',
                    getTerminalColor(terminal.type)
                  )}
                >
                  {getTerminalIcon(terminal.type)}
                  <span className="max-w-[120px] truncate">
                    {terminal.name}
                  </span>
                </TabsTrigger>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 text-muted-foreground hover:text-destructive hover:bg-accent -ml-1"
                  onClick={(e) => {
                    e.stopPropagation()
                    removeTerminal(terminal.id)
                  }}
                  title="Close terminal"
                >
                  <X className="h-3 w-3" />
                </Button>
              </div>
            ))}
          </TabsList>

          {/* Terminal content - render all terminals, show/hide based on active */}
          <div className="flex-1 relative">
            {terminals.map((terminal) => (
              <div
                key={terminal.id}
                data-terminal-id={terminal.id}
                className={`absolute inset-0 ${activeTerminal === terminal.id ? 'block' : 'hidden'}`}
              >
                <ProjectDevelopTerminal
                  selectedProject={devProject}
                  sessionId={terminal.sessionId}
                  isActive={activeTerminal === terminal.id}
                  provider={terminal.type}
                  terminalId={terminal.id}
                />
              </div>
            ))}
          </div>
        </Tabs>
      )}
    </div>
  )
}
