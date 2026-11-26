// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createContext,
  useContext,
  useEffect,
  useId,
  useMemo,
  useSyncExternalStore,
} from 'react'
import { AiAssistantContext } from '@/components/ai/AiAssistantContext-shared'
import {
  describeSubjectForAssistant,
  subjectLabel,
  type HandoffSubject,
} from '@/lib/agent-handoff'

/**
 * What the page on screen is showing, declared once by the page and read by
 * the header's "Hand off to agent" control.
 *
 * It is a store outside React state on purpose. A logs page redeclares its
 * subject as its filters change; if that lived in context state, the header
 * and everything else under the provider would re-render with every change.
 * Here the header subscribes to one boolean (is anything declared?) and the
 * open panel subscribes to the subject itself, so a filter change re-renders
 * the panel while it is open and nothing while it is closed.
 */
export interface PageSubjectStore {
  get(): HandoffSubject | null
  /** `owner` identifies the declaring page, so a page only clears its own. */
  set(owner: string, subject: HandoffSubject | null): void
  clear(owner: string): void
  subscribe(listener: () => void): () => void
}

export function createPageSubjectStore(): PageSubjectStore {
  let current: { owner: string; subject: HandoffSubject } | null = null
  const listeners = new Set<() => void>()
  const emit = () => {
    for (const listener of listeners) listener()
  }
  const clear = (owner: string) => {
    // A page that unmounts after the next one has declared its subject must
    // not wipe that one.
    if (current?.owner !== owner) return
    current = null
    emit()
  }
  return {
    get: () => current?.subject ?? null,
    set(owner, subject) {
      if (subject === null) {
        clear(owner)
        return
      }
      current = { owner, subject }
      emit()
    },
    clear,
    subscribe(listener) {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
  }
}

const defaultStore = createPageSubjectStore()

/** Overridden in tests; the app uses the default store. */
export const PageSubjectStoreContext =
  createContext<PageSubjectStore>(defaultStore)

export interface PageSubjectOptions {
  /**
   * Details of the record for the built-in assistant only (a trace's root
   * span, an error's title). The handoff prompt carries references, never
   * data, so these do not reach it.
   */
  assistantDetails?: string[]
}

/**
 * Declare what this page is showing. Call it once per page, before any early
 * return, with `null` while the record is not loaded.
 *
 * The same declaration feeds "Hand off to agent" and the built-in
 * assistant's page context, so both describe the page identically.
 *
 * Pass the values the page's query actually uses (the debounced search, not
 * the text being typed): that is what is on screen, and it keeps a keystroke
 * from being a change.
 */
export function usePageSubject(
  subject: HandoffSubject | null,
  options: PageSubjectOptions = {}
): void {
  const store = useContext(PageSubjectStoreContext)
  const owner = useId()

  // A subject is plain data; its JSON is its identity. Pages build it inline
  // on every render, so effects key on the serialised form, not the object.
  const subjectKey = subject ? JSON.stringify(subject) : null
  const detailsKey = JSON.stringify(options.assistantDetails ?? [])

  // Declaring and leaving are separate effects. With one effect, its cleanup
  // would clear the store before every redeclaration, and the header would
  // see "nothing declared" and then "something declared" on each filter
  // change: exactly the re-render this store exists to avoid.
  useEffect(() => {
    store.set(
      owner,
      subjectKey ? (JSON.parse(subjectKey) as HandoffSubject) : null
    )
  }, [store, owner, subjectKey])
  useEffect(() => () => store.clear(owner), [store, owner])

  const assistant = useMemo(() => {
    if (!subjectKey) return null
    const parsed = JSON.parse(subjectKey) as HandoffSubject
    return {
      value: describeSubjectForAssistant(
        parsed,
        JSON.parse(detailsKey) as string[]
      ),
      label: subjectLabel(parsed),
    }
  }, [subjectKey, detailsKey])
  // The log viewers and record pages are also rendered where there is no
  // assistant (embedded, or on their own in a test). Declaring a subject must
  // never be what breaks them, so a missing provider means "no assistant to
  // tell", not an error.
  const setPageContext = useContext(AiAssistantContext)?.setPageContext
  const assistantValue = assistant?.value
  const assistantLabel = assistant?.label
  useEffect(() => {
    if (!setPageContext) return
    setPageContext(
      assistantValue && assistantLabel
        ? { value: assistantValue, label: assistantLabel }
        : null
    )
    return () => setPageContext(null)
  }, [setPageContext, assistantValue, assistantLabel])
}

/** Whether the page on screen has declared a subject. */
export function useHasPageSubject(): boolean {
  const store = useContext(PageSubjectStoreContext)
  return useSyncExternalStore(
    store.subscribe,
    () => store.get() !== null,
    () => false
  )
}

/** The declared subject itself. Subscribe only where it is rendered. */
export function useDeclaredPageSubject(): HandoffSubject | null {
  const store = useContext(PageSubjectStoreContext)
  return useSyncExternalStore(store.subscribe, store.get, () => null)
}
