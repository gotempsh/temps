// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useMemo, useState } from 'react'
import {
  type AssistantPageContext,
  type AssistantProject,
  type AiChatContext,
  type OpenOptions,
  AiAssistantContext,
} from './AiAssistantContext-shared'

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

  // Reactive so the dock can show a chip when context is attached. Only the
  // handful of `useAiAssistant()` consumers re-render (they already do on
  // `isOpen`), not the whole app.
  const [pageContext, setPageContextState] =
    useState<AssistantPageContext | null>(null)
  const setPageContext = useCallback(
    (pc: AssistantPageContext | null) => setPageContextState(pc),
    []
  )

  const [currentProject, setCurrentProjectState] =
    useState<AssistantProject | null>(null)
  const setCurrentProject = useCallback(
    (p: AssistantProject | null) => setCurrentProjectState(p),
    []
  )

  const value = useMemo(
    () => ({
      isOpen,
      projectId,
      initialContext,
      openSeq,
      open,
      close,
      toggle,
      pageContext,
      setPageContext,
      currentProject,
      setCurrentProject,
    }),
    [
      isOpen,
      projectId,
      initialContext,
      openSeq,
      open,
      close,
      toggle,
      pageContext,
      setPageContext,
      currentProject,
      setCurrentProject,
    ]
  )

  return (
    <AiAssistantContext.Provider value={value}>
      {children}
    </AiAssistantContext.Provider>
  )
}
