import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
} from 'react'

/** A conversation the assistant can open directly into (e.g. a failed stage). */
export interface AiChatContext {
  contextType: string
  contextId: string | number
  title?: string
  description?: string
  startPrompt?: string
  /** For the "where was this started" source link in the dock header. */
  projectSlug?: string
  projectName?: string
}

interface OpenOptions {
  /** Project for an initial-context open; omitted for a global list-only open. */
  projectId?: number
  /** Open straight into this conversation; omit to open on the chat list. */
  context?: AiChatContext
}

interface AiAssistantValue {
  isOpen: boolean
  projectId: number | null
  initialContext?: AiChatContext
  /** Increments on every `open()` so the dock can remount into the new target. */
  openSeq: number
  open: (opts?: OpenOptions) => void
  close: () => void
  toggle: () => void
}

const AiAssistantContext = createContext<AiAssistantValue | null>(null)

/**
 * App-level state for the AI assistant dock (ADR-023). Holding it above the
 * router lets the dock stay open and keep streaming while the user navigates the
 * rest of the console — it is rendered once in the app shell, not per page.
 */
export function AiAssistantProvider({
  children,
}: {
  children: React.ReactNode
}) {
  const [isOpen, setIsOpen] = useState(false)
  const [projectId, setProjectId] = useState<number | null>(null)
  const [initialContext, setInitialContext] = useState<
    AiChatContext | undefined
  >(undefined)
  const [openSeq, setOpenSeq] = useState(0)

  const open = useCallback((opts?: OpenOptions) => {
    setProjectId(opts?.projectId ?? null)
    setInitialContext(opts?.context)
    setOpenSeq((n) => n + 1)
    setIsOpen(true)
  }, [])

  const close = useCallback(() => setIsOpen(false), [])
  const toggle = useCallback(() => setIsOpen((o) => !o), [])

  const value = useMemo(
    () => ({ isOpen, projectId, initialContext, openSeq, open, close, toggle }),
    [isOpen, projectId, initialContext, openSeq, open, close, toggle]
  )

  return (
    <AiAssistantContext.Provider value={value}>
      {children}
    </AiAssistantContext.Provider>
  )
}

export function useAiAssistant(): AiAssistantValue {
  const ctx = useContext(AiAssistantContext)
  if (!ctx) {
    throw new Error('useAiAssistant must be used within an AiAssistantProvider')
  }
  return ctx
}
