// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ValueRevealState =
  | { phase: 'hidden' }
  | { phase: 'revealing' }
  | { phase: 'revealed'; value: string }

export interface ValueRevealController {
  getState(): ValueRevealState
  subscribe(listener: () => void): () => void
  /** Fetch and show the value. Ignored unless currently hidden. */
  reveal(): Promise<void>
  /** Hide the value and discard any in-flight response. */
  hide(): void
  /** Hide and stop notifying; any in-flight response is discarded. */
  dispose(): void
}

const HIDDEN: ValueRevealState = { phase: 'hidden' }
const REVEALING: ValueRevealState = { phase: 'revealing' }

/**
 * Framework-agnostic reveal state for one credential value. A response is only
 * applied if no hide/dispose happened since its request started, so plaintext
 * never appears after the user hid it or the variable changed underneath it.
 * A rejected fetch returns to `hidden`; reporting the error is the caller's
 * job (the fetch function owns it), so it is surfaced exactly once.
 */
export function createValueReveal(
  fetchValue: () => Promise<string>
): ValueRevealController {
  let state: ValueRevealState = HIDDEN
  let generation = 0
  const listeners = new Set<() => void>()

  const set = (next: ValueRevealState) => {
    state = next
    listeners.forEach((listener) => listener())
  }

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
    async reveal() {
      if (state.phase !== 'hidden') return
      const request = ++generation
      set(REVEALING)
      try {
        const value = await fetchValue()
        if (request === generation) set({ phase: 'revealed', value })
      } catch {
        if (request === generation) set(HIDDEN)
      }
    },
    hide() {
      generation++
      if (state.phase !== 'hidden') set(HIDDEN)
    },
    dispose() {
      generation++
      state = HIDDEN
      listeners.clear()
    },
  }
}
