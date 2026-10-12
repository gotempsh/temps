// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'

/**
 * Decides when a text box that mirrors a URL value should adopt that value.
 *
 * The box owns what the user is typing; the URL is written from it. The router
 * applies URL changes asynchronously (inside a transition), so a value we wrote
 * a moment ago can arrive back after the user has already typed further. Those
 * echoes must not overwrite the box. Only a value we did not write -- back and
 * forward, a followed `?q=` link, "clear all filters" -- is adopted.
 */
export interface UrlTextSync {
  /** Record that `text` was written to the URL. */
  committed(text: string): void
  /** The URL now reads `urlValue`. Returns true when the box should adopt it. */
  observe(urlValue: string): boolean
}

const MAX_IN_FLIGHT = 32

/**
 * How long a write can be waiting for its echo. A router that skipped an
 * intermediate value never renders it, so without an expiry that write would
 * stay "in flight" for good and a later navigation to exactly that value
 * would be mistaken for our own echo and not adopted.
 */
const IN_FLIGHT_MAX_AGE_MS = 2_000

export interface UrlTextSyncOptions {
  maxAgeMs?: number
  now?: () => number
}

export function createUrlTextSync(
  initial: string,
  { maxAgeMs = IN_FLIGHT_MAX_AGE_MS, now = Date.now }: UrlTextSyncOptions = {}
): UrlTextSync {
  let lastUrl = initial
  let inFlight: Array<{ text: string; at: number }> = []

  const prune = () => {
    const cutoff = now() - maxAgeMs
    if (inFlight.some((entry) => entry.at < cutoff)) {
      inFlight = inFlight.filter((entry) => entry.at >= cutoff)
    }
  }

  return {
    committed(text) {
      prune()
      // A write that equals the URL changes nothing, so no echo will follow.
      if (inFlight.length === 0 && text === lastUrl) return
      inFlight = [...inFlight, { text, at: now() }].slice(-MAX_IN_FLIGHT)
    },
    observe(urlValue) {
      if (urlValue === lastUrl) return false
      lastUrl = urlValue
      prune()
      const index = inFlight.findIndex((entry) => entry.text === urlValue)
      if (index >= 0) {
        inFlight = inFlight.slice(index + 1)
        return false
      }
      inFlight = []
      return true
    },
  }
}

export interface DebouncedCommit {
  /**
   * Write `text` after `delayMs`. A delay of 0, an empty `text` (Escape,
   * "Clear") or `immediate` writes straight away. A newer call replaces a
   * pending one.
   */
  set(text: string, options: { delayMs: number; immediate?: boolean }): void
  /** Write the pending text now, if there is any. */
  flush(): void
  /** Drop the pending text without writing it. */
  cancel(): void
}

export interface TimerApi {
  setTimeout: (callback: () => void, ms: number) => unknown
  clearTimeout: (handle: unknown) => void
}

/**
 * The delay/flush/cancel half of `useUrlTextInput`, kept free of React so it
 * can be tested with a fake clock.
 */
export function createDebouncedCommit(
  write: (text: string) => void,
  timers: TimerApi = {
    setTimeout: (callback, ms) => setTimeout(callback, ms),
    clearTimeout: (handle) =>
      clearTimeout(handle as ReturnType<typeof setTimeout>),
  }
): DebouncedCommit {
  let handle: unknown = null
  let pending: string | null = null

  const cancel = () => {
    if (handle !== null) timers.clearTimeout(handle)
    handle = null
    pending = null
  }

  const commit = (text: string) => {
    cancel()
    write(text)
  }

  return {
    set(text, { delayMs, immediate }) {
      cancel()
      if (delayMs <= 0 || text === '' || immediate) {
        commit(text)
        return
      }
      pending = text
      handle = timers.setTimeout(() => commit(text), delayMs)
    },
    flush() {
      if (pending !== null) commit(pending)
    },
    cancel,
  }
}

export interface UseUrlTextInputOptions {
  /** The value currently in the URL. */
  value: string
  /**
   * Write the text to the URL. Put every dependent key (page reset, cursor)
   * in this one call so they land in a single navigation.
   */
  onCommit: (text: string) => void
  /** Delay before typed text is written to the URL. `0` writes immediately. */
  debounceMs?: number
}

export interface UseUrlTextInputResult {
  /** What the input should display. */
  value: string
  /**
   * Update the box. Clearing it (`''`) and `{ immediate: true }` skip the
   * debounce, so Escape and "Clear" feel instant.
   */
  setValue: (text: string, options?: { immediate?: boolean }) => void
  /** Write any pending text to the URL now. */
  flush: () => void
}

/**
 * A text input bound to a URL query parameter that never drops keystrokes.
 *
 * Reading the box's `value` straight from the URL loses characters: the URL
 * lags the keyboard, and a controlled input snaps back to whatever it was last
 * given. This keeps the text in local state, writes it to the URL (debounced)
 * in one `onCommit`, and adopts URL changes that did not originate here.
 */
export function useUrlTextInput({
  value,
  onCommit,
  debounceMs = 250,
}: UseUrlTextInputOptions): UseUrlTextInputResult {
  const [text, setText] = useState(value)
  const [sync] = useState(() => createUrlTextSync(value))
  const onCommitRef = useRef(onCommit)
  // The delayed write must call the latest `onCommit`, not the one from the
  // render that scheduled it: that is how it sees the current filters.
  useLayoutEffect(() => {
    onCommitRef.current = onCommit
  })
  const debouncedRef = useRef<DebouncedCommit | null>(null)
  // Created on first use rather than during render: the write reads
  // `onCommitRef`, which render must not touch.
  const getDebounced = useCallback((): DebouncedCommit => {
    if (debouncedRef.current === null) {
      debouncedRef.current = createDebouncedCommit((next) => {
        sync.committed(next)
        onCommitRef.current(next)
      })
    }
    return debouncedRef.current
  }, [sync])

  useEffect(() => {
    if (sync.observe(value)) {
      getDebounced().cancel()
      // The box mirrors an external system (the URL); adopting its value is
      // exactly what this effect is for.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setText(value)
    }
  }, [value, getDebounced, sync])

  // Typed text that is still waiting out its delay must not be written after
  // the page it belongs to has gone: that would navigate the next page.
  useEffect(() => () => getDebounced().cancel(), [getDebounced])

  const setValue = useCallback(
    (next: string, options?: { immediate?: boolean }) => {
      setText(next)
      getDebounced().set(next, {
        delayMs: debounceMs,
        immediate: options?.immediate,
      })
    },
    [getDebounced, debounceMs]
  )

  const flush = useCallback(() => getDebounced().flush(), [getDebounced])

  return { value: text, setValue, flush }
}
