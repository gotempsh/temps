// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, useEffect } from 'react'

/** What the user is currently viewing, for the assistant + the input chip. */
export interface AssistantPageContext {
  /** Full framing sent to the model (not shown in history). */
  value: string
  /** Short label for the input chip, e.g. "this trace". */
  label: string
}

/** The project the user is currently in (so "new chat" defaults to it). */
export interface AssistantProject {
  id: number
  slug?: string
  name: string
}

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

export interface OpenOptions {
  /** Project for an initial-context open; omitted for a global list-only open. */
  projectId?: number
  /** Open straight into this conversation; omit to open on the chat list. */
  context?: AiChatContext
}

export interface AiAssistantValue {
  isOpen: boolean
  projectId: number | null
  initialContext?: AiChatContext
  /** Increments on every `open()` so the dock can remount into the new target. */
  openSeq: number
  open: (opts?: OpenOptions) => void
  close: () => void
  toggle: () => void
  /** What the user is currently viewing (drives the input chip + send). */
  pageContext: AssistantPageContext | null
  /**
   * Set/clear the current page context. It's attached to the next turn as
   * ephemeral framing — never persisted or shown in history. Prefer the
   * `useAssistantPageContext` hook over calling this directly.
   */
  setPageContext: (pc: AssistantPageContext | null) => void
  /** The project the user is currently in, so "new chat" can default to it. */
  currentProject: AssistantProject | null
  /** Prefer the `useAssistantProject` hook over calling this directly. */
  setCurrentProject: (p: AssistantProject | null) => void
}

export const AiAssistantContext = createContext<AiAssistantValue | null>(null)

export function useAiAssistant(): AiAssistantValue {
  const ctx = useContext(AiAssistantContext)
  if (!ctx) {
    throw new Error('useAiAssistant must be used within an AiAssistantProvider')
  }
  return ctx
}

/**
 * Register what the user is currently viewing so the assistant has context.
 * Pass a short description of the page/entity (e.g. the project + trace);
 * pass `null`/`undefined` when there's nothing useful. The value is set while
 * the component is mounted and cleared on unmount. Memoize the string (or keep
 * it stable) to avoid needless churn.
 *
 * ```tsx
 * useAssistantPageContext(
 *   `Viewing trace ${traceId} in project "${project.name}".`,
 *   'this trace',
 * )
 * ```
 *
 * `label` is the short text shown on the input chip (defaults to "this page").
 */
export function useAssistantPageContext(
  value: string | null | undefined,
  label = 'this page'
) {
  const { setPageContext } = useAiAssistant()
  useEffect(() => {
    const v = value?.trim()
    setPageContext(v ? { value: v, label } : null)
    return () => setPageContext(null)
  }, [value, label, setPageContext])
}

/**
 * Register the project the user is currently in, so the assistant's "new chat"
 * can default to it instead of forcing a project picker. Set while mounted,
 * cleared on unmount. Called once from the project-detail layout.
 */
export function useAssistantProject(
  project: AssistantProject | null | undefined
) {
  const { setCurrentProject } = useAiAssistant()
  const id = project?.id
  const slug = project?.slug
  const name = project?.name
  useEffect(() => {
    setCurrentProject(id != null && name ? { id, slug, name } : null)
    return () => setCurrentProject(null)
  }, [id, slug, name, setCurrentProject])
}
