// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ReactNode } from 'react'
import { cn } from './lib/cn'

/**
 * The six states a thing can be in. Colour is only ever applied through
 * these, and always next to a glyph and a word.
 *
 *  ok       ●  green   healthy, deployed, passing
 *  warn     ◐  amber   degraded, above threshold, expiring
 *  error    ×  red     failing, unreachable
 *  idle     ○  muted   not deployed, not configured, nothing yet
 *  sampled  ◌  muted   telemetry head-sampled past the plan allowance.
 *                      From pricing.md: "the console says so; it is never
 *                      silently dropped."
 *  running  ◉  ink     building, restoring, scanning: work happening now.
 *                      The word comes from the operation; the pulse is the
 *                      only motion in the system.
 */
export type State = 'ok' | 'warn' | 'error' | 'idle' | 'sampled' | 'running'

export const GLYPH: Record<State, string> = {
  ok: '●',
  warn: '◐',
  error: '×',
  idle: '○',
  sampled: '◌',
  running: '◉',
}

export const GLYPH_CLASS: Record<State, string> = {
  ok: 'text-success',
  warn: 'text-warning',
  error: 'text-destructive',
  idle: 'text-muted-foreground',
  sampled: 'text-muted-foreground',
  // Running is not a verdict, so it takes no tone: plain ink, and the pulse
  // carries the meaning. See docs/motion.md.
  running: 'text-foreground',
}

/** Sort order when a list is "needs attention first". */
export const STATE_RANK: Record<State, number> = {
  error: 0,
  warn: 1,
  running: 2,
  sampled: 3,
  ok: 4,
  idle: 5,
}

/**
 * The glyph's classes. Identical to GLYPH_CLASS except for `running`, which
 * also gets `.op-pulse` — the one sanctioned animation in the system.
 */
export function glyphClass(state: State) {
  return cn(GLYPH_CLASS[state], state === 'running' && 'op-pulse')
}

export function worst(states: State[]): State {
  return states.reduce<State>(
    (w, s) => (STATE_RANK[s] < STATE_RANK[w] ? s : w),
    'idle'
  )
}

/**
 * The page's verdict. Inside the console shell it does not take a line of the
 * page: it renders into the header's attention slot as a glyph + count
 * (`× 2 · ◐ 1`), and the sentences show on demand when that is clicked. A
 * page with nothing wrong shows a quiet green glyph and no number. Outside a
 * shell (docs, demos) it renders inline as the line described below.
 *
 * Inline form: one glyph, one sentence, at most one link.
 *  - the glyph is the worst state on the page
 *  - the sentence is the single most important thing, under ~60 characters;
 *    it may wrap to a second line on a phone, it never truncates
 *  - the link (Phrase) is on the thing the reader can act on, if any
 *  - everything else lives in the page below; further problems collapse
 *    into `more` ("+1 warning"), a muted link on the right. Given `items`,
 *    it unfolds the line in place into one glyph + one sentence per item
 *    (each with its own optional link) and the label becomes "less".
 *    Given only `onClick`, it navigates instead
 * Counts, facts and "fine" things never appear here. If the page is fine,
 * the line says so in three or four words.
 * `sticky` pins it under the header while the page scrolls (default on).
 */
export type StatusItem = { state: State; children: ReactNode }
